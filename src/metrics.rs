//! Metrics model and hardware sampler.

use std::collections::HashMap;
use std::time::Duration;

use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;

use crate::shared::{
  ioreport_channels_filter, is_clpc_energy_channel, is_pmp_ane_channel, zero_div,
};
use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, get_soc_info, libc_ram,
  libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;
type CpuCoreKey = String;
type FreqMetrics = (u32, f32, f32);

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

#[derive(Default)]
struct PowerSources {
  clpc: Option<f32>,
  energy_model: Option<f32>,
  pmp: f32,
}

impl PowerSources {
  fn add_clpc(&mut self, watts: f32) {
    // Negative deltas include IOReport's invalid-value sentinel and counter resets.
    if watts.is_finite() && watts >= 0.0 {
      *self.clpc.get_or_insert(0.0) += watts;
    }
  }

  fn add_energy_model(&mut self, watts: f32) {
    *self.energy_model.get_or_insert(0.0) += watts;
  }

  fn watts(self, force_clpc: bool) -> Option<f32> {
    if force_clpc {
      return self.clpc;
    }

    // On macOS 27 Energy Model CPU/ANE counters can freeze without Apple's
    // entitlement. Prefer readable CLPC counters, including valid idle zeroes.
    // Drivers with unknown CLPC IDs need the legacy sources; ANE can use PMP.
    Some(self.clpc.or(self.energy_model).unwrap_or(self.pmp))
  }
}

// MARK: Structs

/// Average hardware temperatures.
#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  /// Average CPU temperature in Celsius, or None when unavailable.
  pub cpu_temp_avg: Option<f32>,
  /// Average GPU temperature in Celsius, or None when unavailable.
  pub gpu_temp_avg: Option<f32>,
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
#[derive(Debug, Default, Clone, Serialize)]
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

/// Metrics for one CPU tier (core type) of [`Metrics::cpu_tiers`].
#[derive(Debug, Default, Serialize)]
pub struct CpuTierMetrics {
  /// Tier label: `E`, `P` or `S`, as in [`SocInfo::cpu_tiers`].
  pub label: String,
  /// Tier frequency in MHz.
  pub freq_mhz: u32,
  /// Mean frequency-weighted active residency across the tier's cores.
  pub scaled_ratio: f32,
  /// Mean fraction of the sampling interval spent in active frequency states.
  pub active_ratio: f32,
  /// Metrics for each core, ordered by die and core index.
  pub cores: Vec<CpuCoreMetrics>,
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
  /// CPU tiers (core types) from the lowest to the highest, as in [`SocInfo::cpu_tiers`].
  pub cpu_tiers: Vec<CpuTierMetrics>,
  /// Frequency of the lowest CPU tier in MHz.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub ecpu_freq_mhz: u32,
  /// Scaled ratio of the lowest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub ecpu_scaled_ratio: f32,
  /// Active ratio of the lowest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub ecpu_active_ratio: f32,
  /// Frequency of the highest CPU tier in MHz.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub pcpu_freq_mhz: u32,
  /// Scaled ratio of the highest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub pcpu_scaled_ratio: f32,
  /// Active ratio of the highest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub pcpu_active_ratio: f32,
  /// Cores of the lowest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
  pub ecpu_cores: Vec<CpuCoreMetrics>,
  /// Cores of the highest CPU tier.
  #[deprecated(since = "0.10.0", note = "use `cpu_tiers`")]
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

/// Average of the valid sensor readings, or None when no sensor has one.
fn temperature_average(values: &[f32]) -> Option<f32> {
  let valid: Vec<f32> = values.iter().copied().filter(|&val| is_valid_temp(val)).collect();
  (!valid.is_empty()).then(|| valid.iter().sum::<f32>() / valid.len() as f32)
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

fn aggregate_ioreport_metrics(mut rs: Metrics, soc: &SocInfo) -> Metrics {
  let (mut total_scaled, mut total_active, mut total_cores) = (0.0, 0.0, 0.0);
  for (tier, info) in rs.cpu_tiers.iter_mut().zip(&soc.cpu_tiers) {
    let scaled: f32 = tier.cores.iter().map(|core| core.scaled_ratio).sum();
    let active: f32 = tier.cores.iter().map(|core| core.active_ratio).sum();
    let cores = tier.cores.len().max(info.cores as usize) as f32;

    let min_frequency_mhz = info.freqs.first().copied().unwrap_or_default();
    tier.freq_mhz = aggregate_frequency(&tier.cores, min_frequency_mhz);
    tier.scaled_ratio = zero_div(scaled, cores);
    tier.active_ratio = zero_div(active, cores);
    total_scaled += scaled;
    total_active += active;
    total_cores += cores;
  }
  rs.cpu_scaled_ratio = zero_div(total_scaled, total_cores);
  rs.cpu_active_ratio = zero_div(total_active, total_cores);
  rs.all_power = rs.cpu_power + rs.gpu_power + rs.ane_power;
  rs.with_deprecated_tiers()
}

impl Metrics {
  // Fills the deprecated `ecpu_*` / `pcpu_*` fields from the lowest and the highest CPU tier.
  #[allow(deprecated)]
  fn with_deprecated_tiers(mut self) -> Self {
    if let Some(tier) = self.cpu_tiers.first() {
      self.ecpu_freq_mhz = tier.freq_mhz;
      self.ecpu_scaled_ratio = tier.scaled_ratio;
      self.ecpu_active_ratio = tier.active_ratio;
      self.ecpu_cores = tier.cores.clone();
    }
    if let Some(tier) = self.cpu_tiers.last() {
      self.pcpu_freq_mhz = tier.freq_mhz;
      self.pcpu_scaled_ratio = tier.scaled_ratio;
      self.pcpu_active_ratio = tier.active_ratio;
      self.pcpu_cores = tier.cores.clone();
    }
    self
  }
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
  M,
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

fn cpu_core_prefix(channel: &str) -> Option<&'static str> {
  ["PCPU", "ECPU", "MCPU"].into_iter().find(|prefix| channel.contains(prefix))
}

fn cpu_core_sort_key(channel: &str) -> (usize, usize, usize) {
  let die_id = parse_die_id(channel);
  let Some(prefix) = cpu_core_prefix(channel) else { return (die_id, 0, 0) };
  let start = channel.find(prefix).unwrap_or_default() + prefix.len();
  let suffix = &channel[start..];

  if let Some((cluster_id, core_id)) = suffix.split_once("_CPU") {
    let cluster_id = if cluster_id.is_empty() { 0 } else { cluster_id.parse().unwrap_or(0) };
    return (die_id, cluster_id, core_id.parse().unwrap_or(0));
  }

  (die_id, 0, parse_cpu_core_id(channel, prefix).unwrap_or(0))
}

fn parse_cpu_core_channel(channel: &str) -> Option<(CpuCoreKind, CpuCoreKey)> {
  let kind = if channel.contains("PCPU") {
    CpuCoreKind::P
  } else if channel.contains("MCPU") {
    CpuCoreKind::M
  } else if channel.contains("ECPU") {
    CpuCoreKind::E
  } else {
    return None;
  };

  // Ultra channel numbers identify a cluster and a core separately, so the complete channel name
  // is the only collision-free key shared by both regular and Ultra chips.
  Some((kind, channel.to_owned()))
}

// Index of a core's tier among `tiers` CPU tiers, lowest first: PCPU cores are the highest tier,
// ECPU cores the lowest. MCPU Performance cores are the middle of three tiers on M6 and the lower
// of two on M5 Pro/Max.
fn cpu_tier_index(kind: CpuCoreKind, tiers: usize) -> Option<usize> {
  match kind {
    CpuCoreKind::M if tiers >= 3 => Some(1),
    CpuCoreKind::E | CpuCoreKind::M => (tiers > 0).then_some(0),
    CpuCoreKind::P => tiers.checked_sub(1),
  }
}

// Whether a frequency table fits a channel's residencies: a frequency for each state after the
// leading IDLE / DOWN at most, as calc_freq_from_residencies expects.
fn freqs_fit(residencies: &[(String, i64)], freqs: &[u32]) -> bool {
  let idle = |x: &&(String, i64)| matches!(x.0.as_str(), "IDLE" | "DOWN" | "OFF");
  let states = residencies.iter().skip_while(idle).count();
  !freqs.is_empty() && residencies.len() > freqs.len() && states >= freqs.len()
}

/// Per-core metrics of the tiers of `SocInfo::cpu_tiers`, keyed by IOReport channel.
struct CpuTierCores<'a> {
  soc: &'a SocInfo,
  tiers: Vec<HashMap<CpuCoreKey, FreqMetrics>>,
}

impl<'a> CpuTierCores<'a> {
  fn new(soc: &'a SocInfo) -> Self {
    Self { soc, tiers: vec![HashMap::new(); soc.cpu_tiers.len()] }
  }

  /// Adds the core of a "CPU Core Performance States" `channel` from its state residencies, or
  /// returns false when the channel is not a CPU core.
  fn add(&mut self, channel: &str, residencies: &[(String, i64)]) -> bool {
    let Some((kind, key)) = parse_cpu_core_channel(channel) else { return false };
    let Some(i) = cpu_tier_index(kind, self.tiers.len()) else { return false };
    // A table with more frequencies than the core has states belongs to another complex (say a
    // future middle tier with its own); leave the core out instead of aborting on it.
    let freqs = &self.soc.cpu_tiers[i].freqs;
    if freqs_fit(residencies, freqs) {
      self.tiers[i].insert(key, calc_freq_from_residencies(residencies, freqs));
    }
    true
  }

  /// Tiers with their cores; `aggregate_ioreport_metrics` fills in the tier values.
  fn into_metrics(self) -> Vec<CpuTierMetrics> {
    let labels = self.soc.cpu_tiers.iter().map(|tier| tier.label.clone());
    let tier = |(label, cores)| CpuTierMetrics {
      label,
      cores: collect_cpu_core_metrics(cores),
      ..Default::default()
    };
    labels.zip(self.tiers).map(tier).collect()
  }
}

fn collect_cpu_core_metrics(metrics: HashMap<CpuCoreKey, FreqMetrics>) -> Vec<CpuCoreMetrics> {
  let mut metrics: Vec<_> = metrics.into_iter().collect();
  metrics.sort_by_key(|(channel, _)| cpu_core_sort_key(channel));
  let mut next_clustered_core_id = HashMap::<usize, usize>::new();

  metrics
    .into_iter()
    .map(|(channel, (freq_mhz, scaled_ratio, active_ratio))| {
      let die_id = parse_die_id(&channel);
      let core_id = if channel.contains("_CPU") {
        let next = next_clustered_core_id.entry(die_id).or_default();
        let core_id = *next;
        *next += 1;
        core_id
      } else {
        cpu_core_prefix(&channel)
          .and_then(|prefix| parse_cpu_core_id(&channel, prefix))
          .unwrap_or_default()
      };

      CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio, active_ratio }
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
  force_clpc: bool,
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
      force_clpc: false,
      soc,
      ior,
      hid,
      smc: smc_sensors.smc,
      smc_cpu_keys: smc_sensors.cpu_keys,
      smc_gpu_keys: smc_sensors.gpu_keys,
      smc_fan_keys: smc_sensors.fan_keys,
    })
  }

  /// Require valid CLPC counters for CPU, GPU, and ANE power, without legacy fallback.
  /// [`Self::get_metrics`] returns an error if any counter is unavailable or invalid.
  pub fn with_clpc() -> WithError<Self> {
    let mut sampler = Self::new()?;
    sampler.force_clpc = true;
    Ok(sampler)
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      cpu_metrics.push(self.smc.read_float_val(sensor)?);
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      gpu_metrics.push(self.smc.read_float_val(sensor)?);
    }

    let cpu_temp_avg = temperature_average(&cpu_metrics);
    let gpu_temp_avg = temperature_average(&gpu_metrics);

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
        gpu_values.push(*value);
        continue;
      }
    }

    let cpu_temp_avg = temperature_average(&cpu_values);
    let gpu_temp_avg = temperature_average(&gpu_values);

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
    let mut cpu_cores = CpuTierCores::new(&self.soc);
    let mut rs = Metrics::default();
    let mut cpu_power = PowerSources::default();
    let mut gpu_power = PowerSources::default();
    let mut ane_power = PowerSources::default();

    // Keep this channel handling in sync with ioreport_channels_filter.
    for x in sample {
      if x.group == "CPU Stats"
        && x.subgroup == CPU_FREQ_CORE_SUBG
        && cpu_cores.add(&x.channel, &cfio_get_residencies(x.item))
      {
        continue;
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
          "GPU Energy" => gpu_power.add_energy_model(cfio_watts(x.item, &x.unit, dt)?),
          // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
          c if c.ends_with("CPU Energy") => {
            cpu_power.add_energy_model(cfio_watts(x.item, &x.unit, dt)?);
          }
          // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
          c if c.starts_with("ANE") => {
            ane_power.add_energy_model(cfio_watts(x.item, &x.unit, dt)?);
          }
          c if c.starts_with("DRAM") => rs.ram_power += cfio_watts(x.item, &x.unit, dt)?,
          c if c.starts_with("GPU SRAM") => rs.gpu_ram_power += cfio_watts(x.item, &x.unit, dt)?,
          _ => {}
        }
      }

      if is_pmp_ane_channel(&x.group, &x.subgroup, &x.channel, &x.unit) {
        ane_power.pmp += cfio_watts(x.item, &x.unit, dt)?;
      }

      if is_clpc_energy_channel(&x.group, &x.subgroup, &x.channel, &x.unit) {
        let power = match x.channel.as_str() {
          "CPU Energy" => &mut cpu_power,
          "GPU Energy" => &mut gpu_power,
          "ANE" => &mut ane_power,
          _ => continue,
        };
        power.add_clpc(cfio_watts(x.item, &x.unit, dt)?);
      }
    }

    rs.cpu_power = cpu_power
      .watts(self.force_clpc)
      .ok_or("CPU power unavailable from CLPC; legacy fallback is disabled")?;
    rs.gpu_power = gpu_power
      .watts(self.force_clpc)
      .ok_or("GPU power unavailable from CLPC; legacy fallback is disabled")?;
    rs.ane_power = ane_power
      .watts(self.force_clpc)
      .ok_or("ANE power unavailable from CLPC; legacy fallback is disabled")?;
    rs.cpu_tiers = cpu_cores.into_metrics();

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
    //   M5:     The family has three core tiers, but current chips expose two at a time:
    //             Base:    PCPU* = Super, ECPU* = Efficiency
    //             Pro/Max: PCPU* = Super, MCPU* = Performance
    //           MCPU is a separate middle-tier design, not a renamed ECPU core.
    //   M6:     All three tiers at once (issue #80): EACC_ECPU* = Efficiency,
    //           PACC0_MCPU* = Performance, PACC0_PCPU* = Super.
    //   Ultra:  Any-generation Ultra chips prefix channels with "DIE_N_"
    //           and include cluster/core separators (e.g. "DIE_0_PCPU1_CPU0").

    let duration = Duration::from_millis(duration as u64);
    let (sample, elapsed) = self.ior.get_sample_interval(duration);
    let mut rs = aggregate_ioreport_metrics(self.get_ioreport_metrics(sample, elapsed)?, &self.soc);

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
  use std::collections::{HashMap, HashSet};

  use super::{
    CpuCoreKind, CpuCoreMetrics, CpuTierCores, CpuTierMetrics, Metrics, PowerSources, TempMetrics,
    aggregate_ioreport_metrics, calc_freq_from_residencies, collect_cpu_core_metrics,
    parse_cpu_core_channel, smc_numeric_value, temperature_average,
  };
  use crate::sources::{CpuTierInfo, SocInfo, cpu_tier_infos, tiers_from_perflevels};

  #[test]
  fn ane_power_uses_pmp_only_when_energy_model_is_absent() {
    assert_eq!(PowerSources::default().watts(false), Some(0.0));
    assert_eq!(PowerSources { pmp: 0.8, ..Default::default() }.watts(false), Some(0.8));

    let mut power = PowerSources { pmp: 0.8, ..Default::default() };
    power.add_energy_model(0.0);
    assert_eq!(power.watts(false), Some(0.0));

    let mut power = PowerSources { pmp: 0.8, ..Default::default() };
    power.add_energy_model(0.5);
    power.add_energy_model(0.25);
    assert_eq!(power.watts(false), Some(0.75));
  }

  #[test]
  fn clpc_power_takes_precedence_without_double_counting_or_replacing_idle_zero() {
    for watts in [0.0, 0.8, 13.0] {
      let mut power = PowerSources { energy_model: Some(2.0), pmp: 0.8, ..Default::default() };
      power.add_clpc(watts);
      assert_eq!(power.watts(false), Some(watts));
    }
    let mut power = PowerSources::default();
    power.add_clpc(1.0);
    power.add_clpc(2.0);
    assert_eq!(power.watts(false), Some(3.0));
  }

  #[test]
  fn invalid_clpc_power_keeps_the_legacy_fallback() {
    for watts in [f32::NAN, f32::INFINITY, -1.0, i64::MIN as f32] {
      let mut power = PowerSources { energy_model: Some(2.0), ..Default::default() };
      power.add_clpc(watts);
      assert_eq!(power.watts(false), Some(2.0));
    }
  }

  #[test]
  fn forced_clpc_rejects_legacy_fallback_but_accepts_idle_zero() {
    for watts in [None, Some(f32::NAN), Some(f32::INFINITY), Some(-1.0), Some(0.0), Some(1.2)] {
      let mut power = PowerSources { energy_model: Some(2.0), pmp: 0.8, ..Default::default() };
      if let Some(watts) = watts {
        power.add_clpc(watts);
      }
      assert_eq!(power.watts(true), watts.filter(|x| x.is_finite() && *x >= 0.0));
    }
  }

  #[test]
  fn temperature_average_rejects_missing_and_invalid_values() {
    assert_eq!(temperature_average(&[]), None);
    assert_eq!(temperature_average(&[0.0, -1.0, 151.0, f32::NAN, f32::INFINITY]), None);
    assert_eq!(temperature_average(&[0.0, 40.0, f32::NAN, 60.0]), Some(50.0));
  }

  #[test]
  fn unavailable_temperature_is_json_null() {
    let metrics = TempMetrics { cpu_temp_avg: Some(45.0), gpu_temp_avg: None };
    let value = serde_json::to_value(metrics).unwrap();
    assert_eq!(value["cpu_temp_avg"], 45.0);
    assert!(value["gpu_temp_avg"].is_null());
  }

  fn core(
    die_id: usize,
    core_id: usize,
    freq_mhz: u32,
    scaled_ratio: f32,
    active_ratio: f32,
  ) -> CpuCoreMetrics {
    CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio, active_ratio }
  }

  /// Chip with an `E` tier (800 MHz) of `ecpu_cores` and a `P` tier (1800 MHz) of `pcpu_cores`.
  fn soc_info(ecpu_cores: u8, pcpu_cores: u8) -> SocInfo {
    let tier =
      |label: &str, cores, freq| CpuTierInfo { label: label.into(), cores, freqs: vec![freq] };
    SocInfo {
      cpu_tiers: vec![tier("E", ecpu_cores, 800), tier("P", pcpu_cores, 1800)],
      ..Default::default()
    }
  }

  /// Sampled `E` and `P` tiers with their cores, before aggregation.
  fn cpu_tiers(ecpu: Vec<CpuCoreMetrics>, pcpu: Vec<CpuCoreMetrics>) -> Vec<CpuTierMetrics> {
    let tier =
      |label: &str, cores| CpuTierMetrics { label: label.into(), cores, ..Default::default() };
    vec![tier("E", ecpu), tier("P", pcpu)]
  }

  #[test]
  fn parse_smc_numeric_values() {
    assert_eq!(smc_numeric_value(&42.5f32.to_le_bytes(), "flt "), Some(42.5));
    assert_eq!(smc_numeric_value(&[0x13, 0x88], "fpe2"), Some(1250.0));
    assert_eq!(smc_numeric_value(&[0x04, 0xd2], "ui16"), Some(1234.0));
    assert_eq!(smc_numeric_value(&[0x00, 0x00, 0x04, 0xd2], "ui32"), Some(1234.0));
  }

  #[test]
  #[allow(deprecated)]
  fn aggregates_ioreport_metrics() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        cpu_tiers: cpu_tiers(
          vec![core(0, 0, 2000, 1.0, 1.0)],
          vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 4000, 1.0, 1.0)],
        ),
        cpu_power: 1.5,
        gpu_power: 2.0,
        ane_power: 0.5,
        ..Default::default()
      },
      &soc_info(2, 1),
    );

    let tiers =
      rs.cpu_tiers.iter().map(|x| (x.label.as_str(), x.freq_mhz, x.scaled_ratio, x.active_ratio));
    assert_eq!(tiers.collect::<Vec<_>>(), [("E", 2000, 0.5, 0.5), ("P", 2000, 0.5, 0.5)]);
    assert_eq!(rs.cpu_scaled_ratio, 0.5);
    assert_eq!(rs.cpu_active_ratio, 0.5);
    assert_eq!(rs.all_power, 4.0);

    // deprecated fields: the lowest and the highest tier
    assert_eq!((rs.ecpu_freq_mhz, rs.ecpu_scaled_ratio, rs.ecpu_active_ratio), (2000, 0.5, 0.5));
    assert_eq!((rs.pcpu_freq_mhz, rs.pcpu_scaled_ratio, rs.pcpu_active_ratio), (2000, 0.5, 0.5));
    assert_eq!((rs.ecpu_cores.len(), rs.pcpu_cores.len()), (1, 2));
  }

  #[test]
  #[allow(deprecated)]
  fn deprecated_fields_keep_the_lowest_and_the_highest_tier() {
    // two tiers with different values, so a swapped or unfilled field shows
    let rs = aggregate_ioreport_metrics(
      Metrics {
        cpu_tiers: cpu_tiers(vec![core(0, 0, 1200, 0.3, 0.6)], vec![core(0, 0, 3000, 0.7, 0.9)]),
        ..Default::default()
      },
      &soc_info(2, 1),
    );
    assert_eq!(
      (rs.ecpu_freq_mhz, rs.ecpu_scaled_ratio, rs.ecpu_active_ratio),
      (1200, 0.3 / 2.0, 0.6 / 2.0)
    );
    assert_eq!((rs.pcpu_freq_mhz, rs.pcpu_scaled_ratio, rs.pcpu_active_ratio), (3000, 0.7, 0.9));
    assert_eq!((rs.ecpu_cores[0].freq_mhz, rs.pcpu_cores[0].freq_mhz), (1200, 3000));
    assert_eq!((rs.cpu_scaled_ratio, rs.cpu_active_ratio), ((0.3 + 0.7) / 3.0, (0.6 + 0.9) / 3.0));

    // JSON keeps the v0.9 keys next to cpu_tiers
    let json = serde_json::to_value(&rs).unwrap();
    for (key, tier) in [("ecpu", 0), ("pcpu", 1)] {
      for field in ["freq_mhz", "scaled_ratio", "active_ratio", "cores"] {
        assert_eq!(json[format!("{key}_{field}")], json["cpu_tiers"][tier][field], "{key}_{field}");
      }
    }

    // pcpu is the highest of three tiers on M6
    let soc = m6_soc();
    let tier = |(info, freq): (&CpuTierInfo, u32)| CpuTierMetrics {
      label: info.label.clone(),
      cores: vec![core(0, 0, freq, 0.5, 0.5)],
      ..Default::default()
    };
    let tiers = soc.cpu_tiers.iter().zip([2000, 3000, 4000]).map(tier).collect();
    let rs = aggregate_ioreport_metrics(Metrics { cpu_tiers: tiers, ..Default::default() }, &soc);
    assert_eq!((rs.ecpu_freq_mhz, rs.pcpu_freq_mhz), (2000, 4000));
  }

  #[test]
  #[allow(deprecated)]
  fn frequency_uses_minimum_floor_when_cluster_is_idle() {
    let rs = aggregate_ioreport_metrics(
      Metrics {
        cpu_tiers: cpu_tiers(
          vec![core(0, 0, 0, 0.0, 0.0), core(0, 1, 0, 0.0, 0.0)],
          vec![core(0, 0, 0, 0.0, 0.0)],
        ),
        ..Default::default()
      },
      &soc_info(0, 0),
    );

    assert_eq!(rs.ecpu_freq_mhz, 800);
    assert_eq!(rs.pcpu_freq_mhz, 1800);
  }

  #[test]
  fn routes_core_channels_to_the_lowest_and_highest_tier() {
    let soc = soc_info(1, 1);
    let mut cores = CpuTierCores::new(&soc);
    let busy = [("IDLE".to_string(), 50), ("V0P0".to_string(), 50)];
    // ECPU, and MCPU of M5 Pro/Max, go to the lowest tier; PCPU, also on Ultra, to the highest
    for channel in ["ECPU0", "MCPU3", "PCPU0", "DIE_1_PCPU1_CPU0"] {
      assert!(cores.add(channel, &busy), "{channel}");
    }
    assert!(!cores.add("GPU0", &busy));

    let tiers = cores.into_metrics();
    let counts = tiers.iter().map(|x| (x.label.as_str(), x.cores.len())).collect::<Vec<_>>();
    assert_eq!(counts, [("E", 2), ("P", 2)]);
  }

  /// A core channel: (channel, states, IDLE residency, non-zero states as in `residencies`).
  type CoreStates = (&'static str, usize, i64, &'static [(usize, i64)]);

  /// Residencies of a core with `states` frequency states named like IOReport's (`V0P6` … `V6P0`
  /// for 7 states): `idle` in IDLE and `active` as (state index, residency).
  fn residencies(states: usize, idle: i64, active: &[(usize, i64)]) -> Vec<(String, i64)> {
    let mut items = vec![("IDLE".to_string(), idle)];
    for i in 0..states {
      let residency = active.iter().find(|(state, _)| *state == i).map_or(0, |(_, x)| *x);
      items.push((format!("V{i}P{}", states - 1 - i), residency));
    }
    items
  }

  /// M6 (Mac18,5) as `load_soc_info` builds it: perflevels 2 / 4 / 6 (issue #80) and the
  /// `pmgr-child` tables voltage-states1-sram and voltage-states5-sram (exelban/stats#3668).
  fn m6_soc() -> SocInfo {
    let tiers = tiers_from_perflevels(&[2, 4, 6], &[], "Apple M6").unwrap();
    let ecpu = [972, 1152, 1584, 1980, 2304, 2640, 2940];
    #[rustfmt::skip]
    let pcpu = [
      1440, 1728, 2040, 2340, 2640, 2940, 3216, 3468, 3696, 3924,
      4092, 4272, 4416, 4476, 4512, 4536, 4584, 4644, 4692, 4788,
    ];
    SocInfo { cpu_tiers: cpu_tier_infos(&tiers, &ecpu, &pcpu), ..Default::default() }
  }

  #[test]
  fn samples_the_three_cpu_tiers_of_m6() {
    // "CPU Core Performance States" of the `macmon debug` report in issue #80:
    // (channel, states, IDLE residency, non-zero states)
    #[rustfmt::skip]
    let channels: [CoreStates; 12] = [
      ("EACC_ECPU0", 7, 260162, &[(6, 2237056)]),
      ("EACC_ECPU1", 7, 397284, &[(6, 2099934)]),
      ("EACC_ECPU2", 7, 655766, &[(6, 1841452)]),
      ("EACC_ECPU3", 7, 698127, &[(6, 1799091)]),
      ("EACC_ECPU4", 7, 1109666, &[(6, 1387552)]),
      ("EACC_ECPU5", 7, 1196151, &[(6, 1301067)]),
      ("PACC0_PCPU0", 20, 2211943, &[(0, 664), (17, 3101), (19, 281544)]),
      ("PACC0_PCPU1", 20, 2100790, &[(0, 656), (17, 19819), (19, 375987)]),
      ("PACC0_MCPU2", 20, 2491819, &[(19, 5433)]),
      ("PACC0_MCPU3", 20, 2495721, &[(19, 1531)]),
      ("PACC0_MCPU4", 20, 2497252, &[]),
      ("PACC0_MCPU5", 20, 2497252, &[]),
    ];
    let soc = m6_soc();
    let mut cores = CpuTierCores::new(&soc);
    for (channel, states, idle, active) in channels {
      assert!(cores.add(channel, &residencies(states, idle, active)), "{channel}");
    }
    let rs = aggregate_ioreport_metrics(
      Metrics { cpu_tiers: cores.into_metrics(), ..Default::default() },
      &soc,
    );

    // 6 E cores, P cores 2-5 and S cores 0-1, each in its own tier. The P-core frequencies follow
    // from the assumed P-complex table (see cpu_tier_infos), not from an M6 measurement.
    let cores =
      |i: usize| rs.cpu_tiers[i].cores.iter().map(|x| (x.core_id, x.freq_mhz)).collect::<Vec<_>>();
    assert_eq!(cores(0), (0..6).map(|i| (i, 2940)).collect::<Vec<_>>());
    assert_eq!(cores(1), [(2, 4788), (3, 4788), (4, 0), (5, 0)]);
    assert_eq!(cores(2), [(0, 4778), (1, 4775)]);

    // expected values computed from the log outside macmon
    let close = |a: f32, b: f64| (a as f64 - b).abs() < 1e-6;
    let expected = [
      ("E", 2940, 0.711868968, 0.711868968),
      ("P", 2394, 0.000697166, 0.000697166),
      ("S", 4776, 0.136181424, 0.136504245),
    ];
    for (tier, (label, freq, scaled, active)) in rs.cpu_tiers.iter().zip(expected) {
      assert_eq!((tier.label.as_str(), tier.freq_mhz), (label, freq));
      assert!(close(tier.scaled_ratio, scaled) && close(tier.active_ratio, active), "{tier:?}");
    }
    assert!(close(rs.cpu_scaled_ratio, 0.378863777) && close(rs.cpu_active_ratio, 0.378917580));
  }

  #[test]
  fn routes_mcpu_to_the_middle_of_three_tiers() {
    let soc = m6_soc();
    let mut cores = CpuTierCores::new(&soc);
    let busy = |states| residencies(states, 50, &[(0, 50)]);
    for (channel, states) in [("EACC_ECPU0", 7), ("PACC0_MCPU2", 20), ("PACC0_PCPU0", 20)] {
      assert!(cores.add(channel, &busy(states)), "{channel}");
    }
    let tiers = cores.into_metrics();
    let counts = tiers.iter().map(|x| (x.label.as_str(), x.cores.len())).collect::<Vec<_>>();
    assert_eq!(counts, [("E", 1), ("P", 1), ("S", 1)]);
  }

  #[test]
  fn skips_a_core_with_fewer_states_than_its_table() {
    // a middle tier in its own complex with 16 states, against the 20 of the P-complex table
    let soc = m6_soc();
    let mut cores = CpuTierCores::new(&soc);
    assert!(cores.add("MACC_MCPU0", &residencies(16, 50, &[(15, 50)])));
    assert!(cores.into_metrics()[1].cores.is_empty());
  }

  #[test]
  fn orders_named_core_metrics() {
    let cores = collect_cpu_core_metrics(HashMap::from([
      ("DIE_1_ECPU0".into(), (2000, 0.50, 0.75)),
      ("DIE_0_ECPU1".into(), (1000, 0.25, 0.50)),
    ]));

    assert_eq!((cores[0].die_id, cores[0].core_id), (0, 1));
    assert_eq!((cores[1].die_id, cores[1].core_id), (1, 0));
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
      ("DIE_0_ECPU0", Some(CpuCoreKind::E)),
      ("DIE_1_ECPU0", Some(CpuCoreKind::E)),
      ("DIE_0_PCPU0", Some(CpuCoreKind::P)),
      ("DIE_1_PCPU0", Some(CpuCoreKind::P)),
      ("ECPU7", Some(CpuCoreKind::E)),
      ("PCPU12", Some(CpuCoreKind::P)),
      ("MCPU3", Some(CpuCoreKind::M)),
      ("EACC_ECPU0", Some(CpuCoreKind::E)), // M6
      ("PACC0_PCPU1", Some(CpuCoreKind::P)),
      ("PACC0_MCPU2", Some(CpuCoreKind::M)),
      ("GPU0", None),
    ];
    for (channel, expected) in cases {
      assert_eq!(
        parse_cpu_core_channel(channel).map(|(kind, _)| kind),
        expected,
        "channel {channel}"
      );
    }
  }

  #[test]
  fn parses_real_m3_ultra_core_channels_without_collisions() {
    let channels = [
      "DIE_0_ECPU_CPU0",
      "DIE_0_ECPU_CPU1",
      "DIE_0_PCPU_CPU0",
      "DIE_0_PCPU_CPU1",
      "DIE_0_PCPU1_CPU0",
      "DIE_0_PCPU1_CPU1",
      "DIE_1_ECPU_CPU0",
      "DIE_1_PCPU_CPU0",
      "DIE_1_PCPU1_CPU0",
    ];
    let parsed = channels.map(parse_cpu_core_channel);

    assert!(parsed.iter().all(Option::is_some));
    let unique = parsed
      .into_iter()
      .flatten()
      .map(|(kind, key)| (kind == CpuCoreKind::P, key))
      .collect::<HashSet<_>>();
    assert_eq!(unique.len(), channels.len());
  }

  #[test]
  fn flattens_real_ultra_cluster_channels_into_core_ids() {
    let cores = collect_cpu_core_metrics(HashMap::from([
      ("DIE_0_PCPU1_CPU0".into(), (2000, 0.50, 0.75)),
      ("DIE_0_PCPU_CPU1".into(), (1100, 0.25, 0.50)),
      ("DIE_0_PCPU_CPU0".into(), (1000, 0.20, 0.40)),
      ("DIE_1_PCPU_CPU0".into(), (1200, 0.30, 0.60)),
    ]));

    assert_eq!(
      cores.iter().map(|core| (core.die_id, core.core_id, core.freq_mhz)).collect::<Vec<_>>(),
      [(0, 0, 1000), (0, 1, 1100), (0, 2, 2000), (1, 0, 1200)]
    );
  }
}
