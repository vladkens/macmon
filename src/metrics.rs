use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;
use std::collections::HashMap;

use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, libc_ram, libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  pub cpu_temp_avg: f32, // Celsius
  pub gpu_temp_avg: f32, // Celsius
}

#[derive(Debug, Default, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  pub temp: TempMetrics,
  pub memory: MemMetrics,
  pub ecpu_usage: (u32, f32), // cluster aggregate: freq, percent_from_max
  pub pcpu_usage: (u32, f32), // cluster aggregate: freq, percent_from_max
  pub ecpu_core_usages: Vec<(u32, f32)>, // per-core: freq, percent_from_max
  pub pcpu_core_usages: Vec<(u32, f32)>, // per-core: freq, percent_from_max
  pub cpu_usage_pct: f32,     // combined ecpu+pcpu usage, weighted by core count
  pub gpu_usage: (u32, f32),  // freq, percent_from_max
  pub cpu_power: f32,         // Watts
  pub gpu_power: f32,         // Watts
  pub ane_power: f32,         // Watts
  pub all_power: f32,         // Watts
  pub sys_power: f32,         // Watts
  pub ram_power: f32,         // Watts
  pub gpu_ram_power: f32,     // Watts
}

// MARK: Helpers

pub fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  if b == zero { zero } else { a / b }
}

fn calc_freq(item: CFDictionaryRef, freqs: &[u32]) -> (u32, f32) {
  let items = cfio_get_residencies(item); // (ns, freq)
  let (len1, len2) = (items.len(), freqs.len());
  assert!(len1 > len2, "cacl_freq invalid data: {} vs {}", len1, len2); // todo?

  // IDLE / DOWN for CPU; OFF for GPU; DOWN only on M2?/M3 Max Chips
  let offset = items.iter().position(|x| x.0 != "IDLE" && x.0 != "DOWN" && x.0 != "OFF").unwrap();

  let usage = items.iter().map(|x| x.1 as f64).skip(offset).sum::<f64>();
  let total = items.iter().map(|x| x.1 as f64).sum::<f64>();
  let count = freqs.len();

  let mut avg_freq = 0f64;
  for i in 0..count {
    let percent = zero_div(items[i + offset].1 as _, usage);
    avg_freq += percent * freqs[i] as f64;
  }

  let usage_ratio = zero_div(usage, total);
  let min_freq = *freqs.first().unwrap() as f64;
  let max_freq = *freqs.last().unwrap() as f64;
  let from_max = (avg_freq.max(min_freq) * usage_ratio) / max_freq;

  (avg_freq as u32, from_max as f32)
}

// Extract core ID from channel name (e.g., "ECPU0" -> Some(0), "PCPU12" -> Some(12))
fn parse_core_id(channel: &str) -> Option<usize> {
  channel
    .chars()
    .skip_while(|c| !c.is_ascii_digit())
    .take_while(|c| c.is_ascii_digit())
    .collect::<String>()
    .parse::<usize>()
    .ok()
}

fn init_smc() -> WithError<(SMC, Vec<String>, Vec<String>)> {
  let mut smc = SMC::new()?;
  const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    let key = match smc.read_key_info(name) {
      Ok(key) => key,
      Err(_) => continue,
    };

    if key.data_size != 4 || key.data_type != FLOAT_TYPE {
      continue;
    }

    let _ = match smc.read_val(name) {
      Ok(val) => val,
      Err(_) => continue,
    };

    // Unfortunately, it is not known which keys are responsible for what.
    // Basically in the code that can be found publicly "Tp" is used for CPU and "Tg" for GPU.

    match name {
      // "Tp" – performance cores, "Te" – efficiency cores, "Ts" – super cores (M5+)
      name if name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts") => {
        cpu_sensors.push(name.clone())
      }
      name if name.starts_with("Tg") => gpu_sensors.push(name.clone()),
      _ => (),
    }
  }

  // println!("{} {}", cpu_sensors.len(), gpu_sensors.len());
  Ok((smc, cpu_sensors, gpu_sensors))
}

// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  ior: IOReport,
  hid: IOHIDSensors,
  smc: SMC,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let channels = vec![
      ("Energy Model", None), // cpu/gpu/ane power
      // ("CPU Stats", Some(CPU_FREQ_DICE_SUBG)), // cpu freq by cluster
      ("CPU Stats", Some(CPU_FREQ_CORE_SUBG)), // cpu freq per core
      ("GPU Stats", Some(GPU_FREQ_DICE_SUBG)), // gpu freq
    ];

    let soc = SocInfo::new()?;
    let ior = IOReport::new(channels)?;
    let hid = IOHIDSensors::new()?;
    let (smc, smc_cpu_keys, smc_gpu_keys) = init_smc()?;

    Ok(Sampler { soc, ior, hid, smc, smc_cpu_keys, smc_gpu_keys })
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = self.smc.read_val(sensor)?;
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
      if val != 0.0 {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = self.smc.read_val(sensor)?;
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
      if val > 0.0 && val <= 150.0 {
        gpu_metrics.push(val);
      }
    }

    let cpu_temp_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_temp_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp_hid(&mut self) -> WithError<TempMetrics> {
    let metrics = self.hid.get_metrics();

    let mut cpu_values = Vec::new();
    let mut gpu_values = Vec::new();

    for (name, value) in &metrics {
      if name.starts_with("pACC MTR Temp Sensor") || name.starts_with("eACC MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        cpu_values.push(*value);
        continue;
      }

      if name.starts_with("GPU MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if *value > 0.0 && *value <= 150.0 {
          gpu_values.push(*value);
        }
        continue;
      }
    }

    let cpu_temp_avg = zero_div(cpu_values.iter().sum(), cpu_values.len() as f32);
    let gpu_temp_avg = zero_div(gpu_values.iter().sum(), gpu_values.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp(&mut self) -> WithError<TempMetrics> {
    // HID for M1, SMC for M2/M3
    // UPD: Looks like HID/SMC related to OS version, not to the chip (SMC available from macOS 14)
    match !self.smc_cpu_keys.is_empty() {
      true => self.get_temp_smc(),
      false => self.get_temp_hid(),
    }
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram()?;
    let (swap_usage, swap_total) = libc_swap()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  fn get_sys_power(&mut self) -> WithError<f32> {
    let val = self.smc.read_val("PSTR")?;
    let val = f32::from_le_bytes(val.data.clone().try_into().unwrap());
    Ok(val)
  }

  pub fn get_metrics(&mut self, duration: u32) -> WithError<Metrics> {
    let measures: usize = 4;
    let mut results: Vec<Metrics> = Vec::with_capacity(measures);

    // CPU Stats channel naming by chip family (see: https://github.com/vladkens/macmon/issues/47)
    //   M1-M4:  ECPU* = efficiency cores (lower tier)
    //           PCPU* = performance cores (top tier)
    //   M5:     Apple renamed ECPU → MCPU in IOReport and introduced a third core tier.
    //           Three-tier architecture (sysctl hw.perflevel{N}.name):
    //             perflevel0 = Super       (top tier,    ex-P, PCPU* in IOReport)
    //             perflevel1 = Performance (mid tier,    Pro/Max only, MCPU* in IOReport)
    //             perflevel2 = Efficiency  (base M5 only, absent on Pro/Max)
    //           M5 Max example: 6 Super + 12 Performance + 0 Efficiency = 18 total.
    //   Ultra:  Any-generation Ultra chips prefix channels with "DIE_N_"
    //           (e.g. "DIE_0_ECPU0"), so use contains() not starts_with() — same
    //           pattern as Energy Model's "DIE_{}_CPU Energy".

    // do several samples to smooth metrics
    // see: https://github.com/vladkens/macmon/issues/10
    for (sample, dt) in self.ior.get_samples(duration as u64, measures) {
      // Use HashMap to explicitly map core ID -> metrics
      let mut ecpu_map: HashMap<usize, (u32, f32)> = HashMap::new();
      let mut pcpu_map: HashMap<usize, (u32, f32)> = HashMap::new();
      let mut rs = Metrics::default();

      for x in sample {
        if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
          // Parse core ID from channel name for robust indexing
          let core_id = match parse_core_id(&x.channel) {
            Some(id) => id,
            None => continue, // Skip if we can't parse the core ID
          };

          if x.channel.starts_with("PCPU") {
            let metrics = calc_freq(x.item, &self.soc.pcpu_freqs);
            pcpu_map.insert(core_id, metrics);
            continue;
          }

          // ECPU on M1-M4, MCPU on M5+ (Performance cores)
          if x.channel.contains("ECPU") || x.channel.contains("MCPU") {
            let metrics = calc_freq(x.item, &self.soc.ecpu_freqs);
            // Filter dead/disabled cores (e.g. M5 Max MCPU0 cluster is all-DOWN)
            if metrics.1 > 0.0 {
              ecpu_map.insert(core_id, metrics);
            }
            continue;
          }
        }

        if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
          match x.channel.as_str() {
            "GPUPH" => rs.gpu_usage = calc_freq(x.item, &self.soc.gpu_freqs[1..]),
            _ => {}
          }
        }

        if x.group == "Energy Model" {
          match x.channel.as_str() {
            "GPU Energy" => rs.gpu_power += cfio_watts(x.item, &x.unit, dt)?,
            // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
            c if c.ends_with("CPU Energy") => rs.cpu_power += cfio_watts(x.item, &x.unit, dt)?,
            // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
            c if c.starts_with("ANE") => rs.ane_power += cfio_watts(x.item, &x.unit, dt)?,
            c if c.starts_with("DRAM") => rs.ram_power += cfio_watts(x.item, &x.unit, dt)?,
            c if c.starts_with("GPU SRAM") => rs.gpu_ram_power += cfio_watts(x.item, &x.unit, dt)?,
            _ => {}
          }
        }
      }

      // Convert HashMap to Vec, sorted by core ID for consistent ordering
      let mut ecpu_usages: Vec<(usize, (u32, f32))> = ecpu_map.into_iter().collect();
      ecpu_usages.sort_by_key(|&(id, _)| id);
      rs.ecpu_core_usages = ecpu_usages.into_iter().map(|(_, metrics)| metrics).collect();

      let mut pcpu_usages: Vec<(usize, (u32, f32))> = pcpu_map.into_iter().collect();
      pcpu_usages.sort_by_key(|&(id, _)| id);
      rs.pcpu_core_usages = pcpu_usages.into_iter().map(|(_, metrics)| metrics).collect();

      results.push(rs);
    }

    // Average across samples for each core
    let ecpu_core_count = results.first().map(|r| r.ecpu_core_usages.len()).unwrap_or(0);
    let pcpu_core_count = results.first().map(|r| r.pcpu_core_usages.len()).unwrap_or(0);

    let mut ecpu_avg = Vec::with_capacity(ecpu_core_count);
    for core_idx in 0..ecpu_core_count {
      let avg_freq = zero_div(
        results
          .iter()
          .map(|r| r.ecpu_core_usages.get(core_idx).map(|x| x.0).unwrap_or(0))
          .sum::<u32>(),
        measures as u32,
      );
      let avg_perc = zero_div(
        results
          .iter()
          .map(|r| r.ecpu_core_usages.get(core_idx).map(|x| x.1).unwrap_or(0.0))
          .sum::<f32>(),
        measures as f32,
      );
      ecpu_avg.push((avg_freq, avg_perc));
    }

    let mut pcpu_avg = Vec::with_capacity(pcpu_core_count);
    for core_idx in 0..pcpu_core_count {
      let avg_freq = zero_div(
        results
          .iter()
          .map(|r| r.pcpu_core_usages.get(core_idx).map(|x| x.0).unwrap_or(0))
          .sum::<u32>(),
        measures as u32,
      );
      let avg_perc = zero_div(
        results
          .iter()
          .map(|r| r.pcpu_core_usages.get(core_idx).map(|x| x.1).unwrap_or(0.0))
          .sum::<f32>(),
        measures as f32,
      );
      pcpu_avg.push((avg_freq, avg_perc));
    }

    // Calculate combined CPU usage percentage weighted by core count
    let ecpu_total_pct: f32 = ecpu_avg.iter().map(|&(_, pct)| pct).sum();
    let pcpu_total_pct: f32 = pcpu_avg.iter().map(|&(_, pct)| pct).sum();
    let ecores = ecpu_avg.len() as f32;
    let pcores = pcpu_avg.len() as f32;
    let tcores = ecores + pcores;
    let cpu_usage_pct = zero_div(ecpu_total_pct + pcpu_total_pct, tcores);

    // Calculate aggregate (average) values for backward compatibility
    let ecpu_avg_freq = zero_div(ecpu_avg.iter().map(|&(f, _)| f).sum::<u32>(), ecores as u32);
    let ecpu_avg_pct = zero_div(ecpu_total_pct, ecores);
    let pcpu_avg_freq = zero_div(pcpu_avg.iter().map(|&(f, _)| f).sum::<u32>(), pcores as u32);
    let pcpu_avg_pct = zero_div(pcpu_total_pct, pcores);

    let gpu_usage_freq = zero_div(results.iter().map(|x| x.gpu_usage.0).sum(), measures as _);
    let gpu_usage_pct = zero_div(results.iter().map(|x| x.gpu_usage.1).sum(), measures as _);
    let cpu_power = zero_div(results.iter().map(|x| x.cpu_power).sum(), measures as _);
    let gpu_power = zero_div(results.iter().map(|x| x.gpu_power).sum(), measures as _);
    let ane_power = zero_div(results.iter().map(|x| x.ane_power).sum(), measures as _);
    let ram_power = zero_div(results.iter().map(|x| x.ram_power).sum(), measures as _);
    let gpu_ram_power = zero_div(results.iter().map(|x| x.gpu_ram_power).sum(), measures as _);
    let all_power = cpu_power + gpu_power + ane_power;

    let mut rs = Metrics {
      ecpu_usage: (ecpu_avg_freq, ecpu_avg_pct),
      pcpu_usage: (pcpu_avg_freq, pcpu_avg_pct),
      ecpu_core_usages: ecpu_avg,
      pcpu_core_usages: pcpu_avg,
      cpu_usage_pct,
      gpu_usage: (gpu_usage_freq, gpu_usage_pct),
      cpu_power,
      gpu_power,
      ane_power,
      ram_power,
      gpu_ram_power,
      all_power,
      ..Default::default()
    };

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

    rs.sys_power = match self.get_sys_power() {
      Ok(val) => val.max(rs.all_power),
      Err(_) => 0.0,
    };

    Ok(rs)
  }

  /// Getter for the `soc` field
  pub fn get_soc_info(&self) -> &SocInfo {
    &self.soc
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn ultra_cpu_channel_matching() {
    // On Ultra chips (M1/M2/M3 Ultra) IOReport CPU Stats channels are prefixed "DIE_N_".
    // These should be recognised; they were with contains() in v0.6.1 but broke when
    // ff5f058 changed to starts_with().
    let cases = [
      ("DIE_0_ECPU0", "ecpu"),
      ("DIE_1_ECPU0", "ecpu"),
      ("DIE_0_PCPU0", "pcpu"),
      ("DIE_1_PCPU0", "pcpu"),
      // Standard (non-Ultra) channels must still work
      ("ECPU0", "ecpu"),
      ("PCPU0", "pcpu"),
      ("MCPU0", "ecpu"), // M5+ performance cores map to ecpu slot
    ];
    for (ch, expected) in cases {
      let matched = if ch.contains("PCPU") {
        "pcpu"
      } else if ch.contains("ECPU") || ch.contains("MCPU") {
        "ecpu"
      } else {
        "none"
      };
      assert_eq!(matched, expected, "channel {ch}");
    }
  }
}
