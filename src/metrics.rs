use core_foundation::dictionary::CFDictionaryRef;

use serde::Serialize;
use std::{collections::HashMap, time::Duration};

use crate::shared::{ioreport_channels_filter, zero_div};
use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, get_soc_info, libc_ram,
  libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;
type CpuCoreKey = (usize, usize);
type FreqMetrics = (u32, f32, f32);

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

/// Average hardware temperatures.
#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  /// Average CPU temperature in Celsius.
  pub cpu_temp_avg: f32,
  /// Average GPU temperature in Celsius.
  pub gpu_temp_avg: f32,
}

/// Memory and swap usage.
#[derive(Debug, Default, Serialize)]
pub struct MemMetrics {
  /// Total physical memory in bytes.
  pub ram_total: u64,
  /// Used physical memory in bytes.
  pub ram_usage: u64,
  /// Total configured swap in bytes.
  pub swap_total: u64,
  /// Used swap in bytes.
  pub swap_usage: u64,
}

/// Fan speed metrics.
#[derive(Debug, Default, Serialize)]
pub struct FanMetric {
  /// Stable fan name derived from the fan order, e.g. `fan0`.
  pub name: String,
  /// Current fan speed in revolutions per minute.
  pub rpm: u32,
  /// Maximum fan speed in revolutions per minute, when reported by SMC.
  pub max_rpm: Option<u32>,
}

/// Metrics for one CPU core.
#[derive(Debug, Default, Serialize)]
pub struct CpuCoreMetrics {
  /// Die index reported by the IOReport channel.
  pub die_id: usize,
  /// Core index within the die.
  pub core_id: usize,
  /// Average frequency in MHz while the core was active.
  pub freq_mhz: u32,
  /// Active residency weighted by operating frequency relative to the core maximum.
  pub scaled_ratio: f32,
  /// Fraction of the sampling interval spent in active frequency states.
  pub active_ratio: f32,
}

struct SmcSensors {
  smc: SMC,
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
  fan_keys: Vec<String>,
}

/// A complete metrics snapshot returned by [`Sampler`].
///
/// Scaled ratios weight active-state residency by operating frequency relative
/// to the hardware maximum. Active ratios count all active-state residency
/// equally, regardless of frequency. Both are in the `0.0..=1.0` range.
/// CPU cluster frequencies use the arithmetic mean of per-core frequencies
/// with a minimum-frequency floor. Per-core and GPU frequencies are averaged
/// over active residency.
/// Power values are reported in Watts.
///
/// This struct may gain new metrics in future releases. When constructing it
/// manually, for example in tests, use struct update syntax:
///
/// ```
/// # use macmon::Metrics;
/// let metrics = Metrics { cpu_power: 1.0, ..Default::default() };
/// ```
#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  /// Temperature metrics.
  pub temp: TempMetrics,
  /// Memory and swap metrics.
  pub memory: MemMetrics,
  /// Fan metrics ordered by stable SMC fan key order.
  pub fans: Vec<FanMetric>,
  /// Combined frequency-weighted active residency across all CPU cores.
  pub cpu_scaled_ratio: f32,
  /// Combined fraction of the sampling interval spent in active CPU frequency states.
  pub cpu_active_ratio: f32,
  /// Efficiency-cluster frequency in MHz.
  pub ecpu_freq_mhz: u32,
  /// Mean frequency-weighted active residency across efficiency cores.
  pub ecpu_scaled_ratio: f32,
  /// Mean fraction of the sampling interval spent in active efficiency-core frequency states.
  pub ecpu_active_ratio: f32,
  /// Performance-cluster frequency in MHz.
  pub pcpu_freq_mhz: u32,
  /// Mean frequency-weighted active residency across performance cores.
  pub pcpu_scaled_ratio: f32,
  /// Mean fraction of the sampling interval spent in active performance-core frequency states.
  pub pcpu_active_ratio: f32,
  /// Metrics for efficiency cores, ordered by die and core index.
  pub ecpu_cores: Vec<CpuCoreMetrics>,
  /// Metrics for performance cores, ordered by die and core index.
  pub pcpu_cores: Vec<CpuCoreMetrics>,
  /// GPU frequency in MHz, averaged over active residency.
  pub gpu_freq_mhz: u32,
  /// GPU active residency weighted by operating frequency relative to its maximum.
  pub gpu_scaled_ratio: f32,
  /// Fraction of the sampling interval spent in active GPU frequency states.
  pub gpu_active_ratio: f32,
  /// CPU package power in Watts.
  pub cpu_power: f32,
  /// GPU power in Watts.
  pub gpu_power: f32,
  /// Apple Neural Engine power in Watts.
  pub ane_power: f32,
  /// Sum of CPU, GPU, and ANE power in Watts.
  pub all_power: f32,
  /// System power estimate in Watts, when available.
  pub sys_power: f32,
  /// DRAM power in Watts.
  pub ram_power: f32,
  /// GPU SRAM power in Watts.
  pub gpu_ram_power: f32,
}

// MARK: Helpers

fn is_valid_temp(val: f32) -> bool {
  val > 0.0 && val <= 150.0
}

fn is_valid_fan_rpm(val: f32) -> bool {
  (0.0..=100_000.0).contains(&val)
}

fn fan_rpm_value(val: f32) -> Option<u32> {
  if is_valid_fan_rpm(val) { Some(val.trunc() as u32) } else { None }
}

fn aggregate_frequency(cores: &[CpuCoreMetrics], min_frequency_mhz: u32) -> u32 {
  let average =
    zero_div(cores.iter().map(|core| core.freq_mhz as f64).sum::<f64>(), cores.len() as f64);
  average.max(min_frequency_mhz as f64) as u32
}

fn aggregate_ioreport_metrics(
  mut rs: Metrics,
  ecpu_min_frequency_mhz: u32,
  pcpu_min_frequency_mhz: u32,
) -> Metrics {
  let ecpu_total_scaled: f32 = rs.ecpu_cores.iter().map(|core| core.scaled_ratio).sum();
  let pcpu_total_scaled: f32 = rs.pcpu_cores.iter().map(|core| core.scaled_ratio).sum();
  let ecpu_total_active: f32 = rs.ecpu_cores.iter().map(|core| core.active_ratio).sum();
  let pcpu_total_active: f32 = rs.pcpu_cores.iter().map(|core| core.active_ratio).sum();
  let ecores = rs.ecpu_cores.len() as f32;
  let pcores = rs.pcpu_cores.len() as f32;
  let tcores = ecores + pcores;

  rs.ecpu_freq_mhz = aggregate_frequency(&rs.ecpu_cores, ecpu_min_frequency_mhz);
  rs.ecpu_scaled_ratio = zero_div(ecpu_total_scaled, ecores);
  rs.ecpu_active_ratio = zero_div(ecpu_total_active, ecores);
  rs.pcpu_freq_mhz = aggregate_frequency(&rs.pcpu_cores, pcpu_min_frequency_mhz);
  rs.pcpu_scaled_ratio = zero_div(pcpu_total_scaled, pcores);
  rs.pcpu_active_ratio = zero_div(pcpu_total_active, pcores);
  rs.cpu_scaled_ratio = zero_div(ecpu_total_scaled + pcpu_total_scaled, tcores);
  rs.cpu_active_ratio = zero_div(ecpu_total_active + pcpu_total_active, tcores);
  rs.all_power = rs.cpu_power + rs.gpu_power + rs.ane_power;
  rs
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

fn calc_freq_from_residencies(items: &[(String, i64)], freqs: &[u32]) -> FreqMetrics {
  let (len1, len2) = (items.len(), freqs.len());
  assert!(len1 > len2, "calc_freq invalid data: {len1} vs {len2}"); // todo?

  // CPU layouts are [IDLE, frequencies...] or [DOWN, IDLE, frequencies...];
  // GPU uses OFF before frequency states.
  let offset = items
    .iter()
    .position(|x| x.0 != "IDLE" && x.0 != "DOWN" && x.0 != "OFF")
    .expect("calc_freq missing active states");

  let usage = items.iter().skip(offset).take(freqs.len()).map(|x| x.1 as f64).sum::<f64>();
  let total = items.iter().map(|x| x.1 as f64).sum::<f64>();

  let mut avg_freq = 0f64;
  for i in 0..freqs.len() {
    let percent = zero_div(items[i + offset].1 as _, usage);
    avg_freq += percent * freqs[i] as f64;
  }

  let active_ratio = zero_div(usage, total);
  let min_freq = *freqs.first().unwrap() as f64;
  let max_freq = *freqs.last().unwrap() as f64;
  let scaled_ratio = (avg_freq.max(min_freq) * active_ratio) / max_freq;

  (avg_freq as u32, scaled_ratio as f32, active_ratio as f32)
}

fn calc_freq(item: CFDictionaryRef, freqs: &[u32]) -> FreqMetrics {
  calc_freq_from_residencies(&cfio_get_residencies(item), freqs)
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

fn collect_cpu_core_metrics(metrics: HashMap<CpuCoreKey, FreqMetrics>) -> Vec<CpuCoreMetrics> {
  let mut metrics: Vec<_> = metrics.into_iter().collect();
  metrics.sort_by_key(|&(key, _)| key);
  metrics
    .into_iter()
    .map(|((die_id, core_id), (freq_mhz, scaled_ratio, active_ratio))| CpuCoreMetrics {
      die_id,
      core_id,
      freq_mhz,
      scaled_ratio,
      active_ratio,
    })
    .collect()
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

// MARK: Sampler

/// Hardware metrics sampler for Apple Silicon Macs.
///
/// Create one sampler and call [`Sampler::get_metrics`] in a continuous polling
/// loop. Run the sampler in a worker thread when sampling must not block the
/// application thread.
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
  /// Initialize hardware metric sources.
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
    for (i, key) in self.smc_fan_keys.iter().enumerate() {
      let Some(rpm) = read_smc_numeric_u32(&mut self.smc, key) else { continue };
      let name = format!("fan{i}");
      let max_rpm = match key.strip_suffix("Ac") {
        Some(prefix) => {
          read_smc_numeric_u32(&mut self.smc, &format!("{prefix}Mx")).filter(|rpm| *rpm > 0)
        }
        None => None,
      };
      fans.push(FanMetric { name, rpm, max_rpm });
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
    dt: Duration,
  ) -> WithError<Metrics> {
    let mut ecpu_map: HashMap<CpuCoreKey, FreqMetrics> = HashMap::new();
    let mut pcpu_map: HashMap<CpuCoreKey, FreqMetrics> = HashMap::new();
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
            ecpu_map.insert(key, calc_freq(x.item, &self.soc.ecpu_freqs));
            continue;
          }
          None => {}
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        match x.channel.as_str() {
          "GPUPH" => {
            let (freq, scaled_ratio, active_ratio) = calc_freq(x.item, &self.soc.gpu_freqs[1..]);
            rs.gpu_freq_mhz = freq;
            rs.gpu_scaled_ratio = scaled_ratio;
            rs.gpu_active_ratio = active_ratio;
          }
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

    rs.ecpu_cores = collect_cpu_core_metrics(ecpu_map);
    rs.pcpu_cores = collect_cpu_core_metrics(pcpu_map);

    Ok(rs)
  }

  /// Collect metrics for the next polling interval.
  ///
  /// Intended to be called continuously in a polling loop. The sampler keeps
  /// an IOReport baseline between calls and derives metrics from the complete
  /// interval between consecutive samples.
  ///
  /// `duration` is the requested polling interval in milliseconds.
  pub fn get_metrics(&mut self, duration: u32) -> WithError<Metrics> {
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

    let duration = Duration::from_millis(duration as u64);
    let (sample, elapsed) = self.ior.get_sample_interval(duration);
    let ecpu_min_frequency_mhz = self.soc.ecpu_freqs.first().copied().unwrap_or_default();
    let pcpu_min_frequency_mhz = self.soc.pcpu_freqs.first().copied().unwrap_or_default();
    let mut rs = aggregate_ioreport_metrics(
      self.get_ioreport_metrics(sample, elapsed)?,
      ecpu_min_frequency_mhz,
      pcpu_min_frequency_mhz,
    );

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;
    rs.fans = self.get_fans();

    rs.sys_power = match self.get_sys_power() {
      Ok(val) => val.max(rs.all_power),
      Err(_) => 0.0,
    };

    Ok(rs)
  }

  /// Return static SoC information used by this sampler.
  pub fn get_soc_info(&self) -> &SocInfo {
    &self.soc
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use super::{
    CpuCoreKind, CpuCoreMetrics, Metrics, aggregate_ioreport_metrics, calc_freq_from_residencies,
    collect_cpu_core_metrics, fan_rpm_value, parse_cpu_core_channel, smc_numeric_value,
  };

  fn core(
    die_id: usize,
    core_id: usize,
    freq_mhz: u32,
    scaled_ratio: f32,
    active_ratio: f32,
  ) -> CpuCoreMetrics {
    CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio, active_ratio }
  }

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
  fn aggregates_ioreport_metrics() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 1000, 0.25, 0.50), core(0, 1, 1200, 0.50, 0.75)],
        pcpu_cores: vec![core(0, 0, 2000, 0.75, 1.0)],
        cpu_power: 1.5,
        gpu_power: 2.0,
        ane_power: 0.5,
        ..Default::default()
      },
      800,
      1800,
    );

    assert_eq!(rs.ecpu_freq_mhz, 1100);
    assert_eq!(rs.ecpu_scaled_ratio, 0.375);
    assert_eq!(rs.ecpu_active_ratio, 0.625);
    assert_eq!(rs.pcpu_freq_mhz, 2000);
    assert_eq!(rs.pcpu_scaled_ratio, 0.75);
    assert_eq!(rs.pcpu_active_ratio, 1.0);
    assert_eq!(rs.cpu_scaled_ratio, 0.5);
    assert_eq!(rs.cpu_active_ratio, 0.75);
    assert_eq!(rs.all_power, 4.0);
  }

  #[test]
  fn inactive_cores_count_toward_cluster_capacity() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 2000, 1.0, 1.0)],
        ..Default::default()
      },
      800,
      1800,
    );

    assert_eq!(rs.ecpu_freq_mhz, 1000);
    assert_eq!(rs.ecpu_scaled_ratio, 0.5);
    assert_eq!(rs.ecpu_active_ratio, 0.5);
    assert_eq!(rs.cpu_scaled_ratio, 0.5);
    assert_eq!(rs.cpu_active_ratio, 0.5);
  }

  #[test]
  fn frequency_uses_minimum_floor_when_cluster_is_idle() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 0, 0.0, 0.0)],
        pcpu_cores: vec![core(0, 0, 0, 0.0, 0.0)],
        ..Default::default()
      },
      800,
      1800,
    );

    assert_eq!(rs.ecpu_freq_mhz, 800);
    assert_eq!(rs.pcpu_freq_mhz, 1800);
  }

  #[test]
  fn serializes_named_core_metrics_in_stable_order() {
    let metrics = Metrics {
      ecpu_cores: collect_cpu_core_metrics(HashMap::from([
        ((1, 0), (2000, 0.50, 0.75)),
        ((0, 1), (1000, 0.25, 0.50)),
      ])),
      ..Default::default()
    };
    let json = serde_json::to_value(metrics).unwrap();
    let cores = json["ecpu_cores"].as_array().unwrap();

    assert_eq!(cores[0]["die_id"], 0);
    assert_eq!(cores[0]["core_id"], 1);
    assert_eq!(cores[0]["freq_mhz"], 1000);
    assert_eq!(cores[0]["scaled_ratio"], 0.25);
    assert_eq!(cores[0]["active_ratio"], 0.50);
    assert_eq!(cores[1]["die_id"], 1);

    let mut keys: Vec<_> = cores[0].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["active_ratio", "core_id", "die_id", "freq_mhz", "scaled_ratio"]);
  }

  #[test]
  fn serializes_one_frequency_and_parallel_ratios() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        ecpu_cores: vec![core(0, 0, 1000, 0.25, 0.25), core(0, 1, 2000, 0.75, 0.75)],
        pcpu_cores: vec![core(0, 0, 2000, 0.75, 1.0)],
        gpu_freq_mhz: 500,
        gpu_scaled_ratio: 0.20,
        ..Default::default()
      },
      800,
      1800,
    );

    assert_eq!(rs.ecpu_freq_mhz, 1500);
    assert_eq!(rs.pcpu_freq_mhz, 2000);
    assert_eq!(rs.gpu_freq_mhz, 500);

    let json = serde_json::to_value(rs).unwrap();
    assert_eq!(json["ecpu_freq_mhz"], 1500);
    assert_eq!(json["ecpu_scaled_ratio"], 0.5);
    assert_eq!(json["pcpu_freq_mhz"], 2000);
    assert_eq!(json["pcpu_scaled_ratio"], 0.75);
    assert_eq!(json["gpu_freq_mhz"], 500);
    assert!((json["gpu_scaled_ratio"].as_f64().unwrap() - 0.20).abs() < f32::EPSILON as f64);

    let mut keys: Vec<_> = json
      .as_object()
      .unwrap()
      .keys()
      .filter(|key| key.ends_with("_freq_mhz") || key.ends_with("_ratio"))
      .map(String::as_str)
      .collect();
    keys.sort_unstable();
    assert_eq!(
      keys,
      [
        "cpu_active_ratio",
        "cpu_scaled_ratio",
        "ecpu_active_ratio",
        "ecpu_freq_mhz",
        "ecpu_scaled_ratio",
        "gpu_active_ratio",
        "gpu_freq_mhz",
        "gpu_scaled_ratio",
        "pcpu_active_ratio",
        "pcpu_freq_mhz",
        "pcpu_scaled_ratio",
      ]
    );
  }

  #[test]
  fn calculates_frequency_over_the_complete_residency_window() {
    let (frequency, scaled_ratio, active_ratio) = calc_freq_from_residencies(
      &[
        ("DOWN".into(), 0),
        ("IDLE".into(), 500),
        ("1000 MHz".into(), 100),
        ("2000 MHz".into(), 400),
      ],
      &[1000, 2000],
    );

    assert_eq!(frequency, 1800);
    assert!((scaled_ratio - 0.45).abs() < f32::EPSILON);
    assert!((active_ratio - 0.5).abs() < f32::EPSILON);
  }

  #[test]
  fn treats_down_as_dynamic_inactive_residency() {
    let (frequency, scaled_ratio, active_ratio) = calc_freq_from_residencies(
      &[
        ("DOWN".into(), 800),
        ("IDLE".into(), 100),
        ("1000 MHz".into(), 100),
        ("2000 MHz".into(), 0),
      ],
      &[1000, 2000],
    );

    assert_eq!(frequency, 1000);
    assert!((scaled_ratio - 0.05).abs() < f32::EPSILON);
    assert!((active_ratio - 0.1).abs() < f32::EPSILON);
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
