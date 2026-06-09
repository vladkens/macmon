use core_foundation::dictionary::CFDictionaryRef;

use serde::Serialize;
use std::collections::HashMap;

use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, get_soc_info, libc_ram,
  libc_swap,
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
pub struct FanMetric {
  pub rpm: u32,
  pub max_rpm: Option<u32>,
}

struct SmcSensors {
  smc: SMC,
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
  fan_keys: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  pub temp: TempMetrics,
  pub memory: MemMetrics,
  pub fans: Vec<FanMetric>,
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

fn is_valid_temp(val: f32) -> bool {
  val > 0.0 && val <= 150.0
}

fn is_valid_fan_rpm(val: f32) -> bool {
  (0.0..=100_000.0).contains(&val)
}

fn fan_rpm_value(val: f32) -> Option<u32> {
  if is_valid_fan_rpm(val) { Some(val.trunc() as u32) } else { None }
}

fn smc_numeric_value(data: &[u8], unit: &str) -> Option<f32> {
  match unit {
    "flt " if data.len() == 4 => Some(f32::from_le_bytes(data.try_into().ok()?)),
    "fpe2" if data.len() >= 2 => Some(((data[0] as u16) << 6 | ((data[1] as u16) >> 2)) as f32),
    "ui8 " if !data.is_empty() => Some(data[0] as f32),
    "ui16" if data.len() >= 2 => Some(u16::from_be_bytes(data[0..2].try_into().ok()?) as f32),
    "ui32" if data.len() >= 4 => Some(u32::from_be_bytes(data[0..4].try_into().ok()?) as f32),
    _ => None,
  }
}

fn read_smc_numeric_u32(smc: &mut SMC, key: &str) -> Option<u32> {
  let val = smc.read_val(key).ok()?;
  let val = smc_numeric_value(&val.data, &val.unit)?;
  fan_rpm_value(val)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuCoreKind {
  E,
  P,
}

fn parse_cpu_core_id(channel: &str, prefix: &str) -> Option<usize> {
  let start = channel.find(prefix)? + prefix.len();
  let digits = channel[start..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
  if digits.is_empty() { None } else { digits.parse().ok() }
}

fn parse_die_id(channel: &str) -> usize {
  let Some(rest) = channel.strip_prefix("DIE_") else { return 0 };
  rest.split_once('_').and_then(|(id, _)| id.parse().ok()).unwrap_or(0)
}

fn parse_cpu_core_channel(channel: &str) -> Option<(CpuCoreKind, (usize, usize))> {
  if let Some(core_id) = parse_cpu_core_id(channel, "PCPU") {
    return Some((CpuCoreKind::P, (parse_die_id(channel), core_id)));
  }

  parse_cpu_core_id(channel, "ECPU")
    .or_else(|| parse_cpu_core_id(channel, "MCPU"))
    .map(|core_id| (CpuCoreKind::E, (parse_die_id(channel), core_id)))
}

fn init_smc() -> WithError<SmcSensors> {
  let mut smc = SMC::new()?;

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();
  let mut fan_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    if name.len() == 4 && name.starts_with('F') && name.ends_with("Ac") {
      fan_sensors.push(name.clone());
      continue;
    }
    // Unfortunately, it is not known which keys are responsible for what.
    // Basically in the code that can be found publicly "Tp" is used for CPU and "Tg" for GPU.

    let is_cpu = name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts");
    let is_gpu = name.starts_with("Tg");
    if !is_cpu && !is_gpu {
      continue;
    }

    if smc.read_float_val(name).is_err() {
      continue;
    }

    if is_cpu {
      cpu_sensors.push(name.clone());
    } else if is_gpu {
      gpu_sensors.push(name.clone());
    }
  }

  // Sort first so fan order is stable and any duplicate keys become adjacent for dedup().
  fan_sensors.sort();
  fan_sensors.dedup();

  // println!("{} {}", cpu_sensors.len(), gpu_sensors.len());
  Ok(SmcSensors { smc, cpu_keys: cpu_sensors, gpu_keys: gpu_sensors, fan_keys: fan_sensors })
}

pub(crate) fn ioreport_channels_filter(
  group: &str,
  subgroup: &str,
  channel: &str,
  _unit: &str,
) -> bool {
  // Keep this filter in sync with the channel handling in Sampler::get_metrics.
  if group == "Energy Model" {
    return channel == "GPU Energy"
      || channel.ends_with("CPU Energy")
      || channel.starts_with("ANE")
      || channel.starts_with("DRAM")
      || channel.starts_with("GPU SRAM");
  }

  if group == "CPU Stats" {
    return subgroup == CPU_FREQ_CORE_SUBG;
  }

  group == "GPU Stats" && subgroup == GPU_FREQ_DICE_SUBG
}
// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  ior: IOReport,
  hid: IOHIDSensors,
  smc: SMC,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
  smc_fan_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let soc = get_soc_info()?;
    let ior = IOReport::with_filter(Some(ioreport_channels_filter))?;
    let hid = IOHIDSensors::new()?;
    let smc_sensors = init_smc()?;

    Ok(Sampler {
      soc,
      ior,
      hid,
      smc: smc_sensors.smc,
      smc_cpu_keys: smc_sensors.cpu_keys,
      smc_gpu_keys: smc_sensors.gpu_keys,
      smc_fan_keys: smc_sensors.fan_keys,
    })
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = self.smc.read_float_val(sensor)?;
      if is_valid_temp(val) {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = self.smc.read_float_val(sensor)?;
      if is_valid_temp(val) {
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
        if is_valid_temp(*value) {
          cpu_values.push(*value);
        }
        continue;
      }

      if name.starts_with("GPU MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if is_valid_temp(*value) {
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

  fn get_fans(&mut self) -> Vec<FanMetric> {
    let mut fans = Vec::new();
    for key in &self.smc_fan_keys {
      let Some(rpm) = read_smc_numeric_u32(&mut self.smc, key) else { continue };
      let max_rpm = match key.strip_suffix("Ac") {
        Some(prefix) => {
          read_smc_numeric_u32(&mut self.smc, &format!("{prefix}Mx")).filter(|rpm| *rpm > 0)
        }
        None => None,
      };
      fans.push(FanMetric { rpm, max_rpm });
    }
    fans
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram()?;
    let (swap_usage, swap_total) = libc_swap()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  fn get_sys_power(&mut self) -> WithError<f32> {
    self.smc.read_float_val("PSTR")
  }

  fn get_ioreport_metrics(
    &self,
    sample: crate::sources::IOReportIterator,
    dt: u64,
  ) -> WithError<Metrics> {
    let mut ecpu_map: HashMap<(usize, usize), (u32, f32)> = HashMap::new();
    let mut pcpu_map: HashMap<(usize, usize), (u32, f32)> = HashMap::new();
    let mut rs = Metrics::default();

    // Keep this channel handling in sync with ioreport_channels_filter.
    for x in sample {
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        match parse_cpu_core_channel(&x.channel) {
          Some((CpuCoreKind::P, key)) => {
            let metrics = calc_freq(x.item, &self.soc.pcpu_freqs);
            pcpu_map.insert(key, metrics);
            continue;
          }
          Some((CpuCoreKind::E, key)) => {
            let metrics = calc_freq(x.item, &self.soc.ecpu_freqs);
            // Filter dead/disabled cores (e.g. M5 Max MCPU0 cluster is all-DOWN).
            if metrics.1 > 0.0 {
              ecpu_map.insert(key, metrics);
            }
            continue;
          }
          None => {}
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

    let mut ecpu_usages: Vec<((usize, usize), (u32, f32))> = ecpu_map.into_iter().collect();
    ecpu_usages.sort_by_key(|&(key, _)| key);
    rs.ecpu_core_usages = ecpu_usages.into_iter().map(|(_, metrics)| metrics).collect();

    let mut pcpu_usages: Vec<((usize, usize), (u32, f32))> = pcpu_map.into_iter().collect();
    pcpu_usages.sort_by_key(|&(key, _)| key);
    rs.pcpu_core_usages = pcpu_usages.into_iter().map(|(_, metrics)| metrics).collect();

    Ok(rs)
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
      results.push(self.get_ioreport_metrics(sample, dt)?);
    }

    // Average across samples for each core
    let ecpu_core_count = results.iter().map(|r| r.ecpu_core_usages.len()).max().unwrap_or(0);
    let pcpu_core_count = results.iter().map(|r| r.pcpu_core_usages.len()).max().unwrap_or(0);

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
    rs.fans = self.get_fans();

    rs.sys_power = match self.get_sys_power() {
      Ok(val) => val.max(rs.all_power),
      Err(_) => 0.0,
    };

    Ok(rs)
  }

  /// Return metrics for manually scheduled sampling.
  ///
  /// This method does not sleep or use the 4-sample smoothing from
  /// [`Sampler::get_metrics`]. It returns `None` for the first or stale sample window.
  pub fn get_metrics_now(&mut self, stale_after_ms: u32) -> WithError<Option<Metrics>> {
    let Some((sample, dt)) = self.ior.get_sample_now(stale_after_ms as u64)? else {
      return Ok(None);
    };
    let mut rs = self.get_ioreport_metrics(sample, dt)?;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;
    rs.fans = self.get_fans();

    rs.sys_power = match self.get_sys_power() {
      Ok(val) => val.max(rs.all_power),
      Err(_) => 0.0,
    };

    Ok(Some(rs))
  }

  /// Getter for the `soc` field
  pub fn get_soc_info(&self) -> &SocInfo {
    &self.soc
  }
}

#[cfg(test)]
mod tests {
  use super::{CpuCoreKind, fan_rpm_value, parse_cpu_core_channel, smc_numeric_value};

  #[test]
  fn parse_smc_numeric_values() {
    assert_eq!(smc_numeric_value(&42.5f32.to_le_bytes(), "flt "), Some(42.5));
    assert_eq!(smc_numeric_value(&[0x13, 0x88], "fpe2"), Some(1250.0));
    assert_eq!(smc_numeric_value(&[0x04, 0xd2], "ui16"), Some(1234.0));
    assert_eq!(smc_numeric_value(&[0x00, 0x00, 0x04, 0xd2], "ui32"), Some(1234.0));
  }

  #[test]
  fn parse_fan_rpm_values() {
    assert_eq!(fan_rpm_value(1234.9), Some(1234));
    assert_eq!(fan_rpm_value(0.0), Some(0));
    assert_eq!(fan_rpm_value(-1.0), None);
  }

  #[test]
  fn ultra_cpu_channel_matching() {
    // On Ultra chips (M1/M2/M3 Ultra) IOReport CPU Stats channels are prefixed "DIE_N_".
    // The DIE_N prefix must not be mistaken for the core id.
    let cases = [
      ("DIE_0_ECPU0", Some((CpuCoreKind::E, (0, 0)))),
      ("DIE_1_ECPU0", Some((CpuCoreKind::E, (1, 0)))),
      ("DIE_0_PCPU0", Some((CpuCoreKind::P, (0, 0)))),
      ("DIE_1_PCPU0", Some((CpuCoreKind::P, (1, 0)))),
      ("ECPU7", Some((CpuCoreKind::E, (0, 7)))),
      ("PCPU12", Some((CpuCoreKind::P, (0, 12)))),
      ("MCPU3", Some((CpuCoreKind::E, (0, 3)))), // M5+ performance cores map to ecpu slot
      ("GPU0", None),
    ];
    for (channel, expected) in cases {
      assert_eq!(parse_cpu_core_channel(channel), expected, "channel {channel}");
    }
  }
}
