use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;
use std::collections::BTreeMap;

use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, cfio_watts, libc_ram, libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default, Clone, Serialize)]
pub struct TempMetrics {
  pub cpu_temp_avg: f32, // Celsius
  pub gpu_temp_avg: f32, // Celsius
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default, Serialize)]
pub struct UsageMetrics {
  pub cpu: BTreeMap<String, (u32, f32, u32)>,
  pub gpu: BTreeMap<String, (u32, f32, u32)>,
}

#[derive(Debug, Default, Serialize)]
pub struct PowerMetrics {
  pub cpu: f32,
  pub gpu: f32,
  pub ram: f32,
  pub sys: f32,
  pub gpu_ram: f32,
  pub ane: f32,
  pub all: f32,
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  pub usage: UsageMetrics,
  pub power: PowerMetrics,
  pub memory: MemMetrics,
  pub temp: TempMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CpuDomain {
  Efficiency,
  Performance,
}

#[derive(Debug, Clone)]
struct CpuDomainSpec {
  domain: CpuDomain,
  channel_prefix: &'static str,
  core_prefix: &'static str,
  freqs: Vec<u32>,
  total_units: u32,
  fallback_name: &'static str,
}

// MARK: Helpers

pub fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  if b == zero { zero } else { a / b }
}

fn is_active_state(name: &str) -> bool {
  name != "IDLE" && name != "DOWN" && name != "OFF"
}

fn calc_freq_from_residencies(items: &[(String, i64)], freqs: &[u32]) -> (u32, f32) {
  let min_freq = *freqs.first().unwrap_or(&0) as f64;
  let max_freq = *freqs.last().unwrap_or(&0) as f64;
  if max_freq == 0.0 {
    return (0, 0.0);
  }

  let offset = items.iter().position(|x| is_active_state(x.0.as_str())).unwrap_or(items.len());
  let active: Vec<f64> = items[offset..].iter().map(|x| x.1.max(0) as f64).collect();

  let usage = active.iter().sum::<f64>();
  let total = items.iter().map(|x| x.1.max(0) as f64).sum::<f64>();
  if usage == 0.0 || total == 0.0 || active.is_empty() {
    return (min_freq as u32, 0.0);
  }

  let mut avg_freq = 0f64;
  if active.len() == freqs.len() {
    for (residency, freq) in active.iter().zip(freqs.iter()) {
      let percent = zero_div(*residency, usage);
      avg_freq += percent * *freq as f64;
    }
  } else {
    // On some chips/clusters residency state count differs from pmgr DVFS table length.
    // Interpolate across known min/max to avoid silently dropping tail states.
    let steps = active.len().saturating_sub(1) as f64;
    for (idx, residency) in active.iter().enumerate() {
      let percent = zero_div(*residency, usage);
      let state_ratio = if steps == 0.0 { 0.0 } else { idx as f64 / steps };
      let freq = min_freq + (max_freq - min_freq) * state_ratio;
      avg_freq += percent * freq;
    }
  }

  let usage_ratio = zero_div(usage, total);
  let from_max = (avg_freq.max(min_freq) * usage_ratio) / max_freq;
  (avg_freq.max(min_freq) as u32, from_max as f32)
}

fn calc_freq(item: CFDictionaryRef, freqs: &[u32]) -> (u32, f32) {
  let items = cfio_get_residencies(item); // (ns, freq)
  calc_freq_from_residencies(&items, freqs)
}

fn calc_freq_avg(items: &[(u32, f32)], min_freq: u32) -> (u32, f32) {
  let avg_freq = zero_div(items.iter().map(|x| x.0 as f32).sum(), items.len() as f32);
  let avg_perc = zero_div(items.iter().map(|x| x.1).sum(), items.len() as f32);
  (avg_freq.max(min_freq as f32) as u32, avg_perc)
}

fn calc_freq_final(items: &[(u32, f32)], freqs: &[u32]) -> (u32, f32) {
  calc_freq_avg(items, *freqs.first().unwrap_or(&0))
}

fn calc_freq_avg_weighted(items: &[(u32, f32, u32)], min_freq: u32) -> (u32, f32) {
  let total_units = items.iter().map(|x| x.2).sum::<u32>() as f32;
  if total_units == 0.0 {
    return (min_freq, 0.0);
  }

  let avg_freq = items.iter().map(|x| x.0 as f32 * x.2 as f32).sum::<f32>() / total_units;
  let avg_perc = items.iter().map(|x| x.1 * x.2 as f32).sum::<f32>() / total_units;
  (avg_freq.max(min_freq as f32) as u32, avg_perc)
}

fn cpu_core_index(channel: &str, prefix: &str) -> Option<u32> {
  let channel = channel.to_ascii_uppercase();
  let suffix = channel.strip_prefix(prefix)?;
  if suffix.is_empty() {
    return Some(0);
  }

  suffix.parse::<u32>().ok()
}

fn cluster_usage_from_cores(
  cores: &BTreeMap<u32, (u32, f32)>,
  cluster_names: &[String],
  cluster_units: &BTreeMap<String, u32>,
) -> BTreeMap<String, f32> {
  let core_values = cores.values().copied().collect::<Vec<_>>();
  let mut usage_by_cluster = BTreeMap::new();
  let mut offset = 0usize;
  for name in cluster_names {
    let units = *cluster_units.get(name).unwrap_or(&0);
    let end = (offset + units as usize).min(core_values.len());
    let usage =
      zero_div(core_values[offset..end].iter().map(|(_, usage)| *usage).sum::<f32>(), units as f32);
    usage_by_cluster.insert(name.clone(), usage);
    offset = end;
  }

  usage_by_cluster
}

fn distribute_units(names: &[String], total_units: u32) -> BTreeMap<String, u32> {
  let mut result = BTreeMap::new();
  if names.is_empty() {
    return result;
  }

  let len = names.len() as u32;
  let base = zero_div(total_units, len);
  let remainder = total_units % len;
  for (idx, name) in names.iter().enumerate() {
    let extra = if (idx as u32) < remainder { 1 } else { 0 };
    result.insert(name.clone(), base + extra);
  }

  result
}

fn cluster_key(channel: &str) -> Option<String> {
  let key = channel
    .trim_matches(|c: char| c == '_' || c == '-' || c == ' ')
    .chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .collect::<String>()
    .to_ascii_uppercase();
  if key.is_empty() { None } else { Some(key) }
}

fn cluster_sort_key(name: &str) -> (String, bool, u32) {
  let split_at = name.rfind(|c: char| !c.is_ascii_digit()).map(|idx| idx + 1).unwrap_or(0);
  if split_at < name.len() {
    let (base, suffix) = name.split_at(split_at);
    if let Ok(index) = suffix.parse::<u32>() {
      return (base.to_string(), true, index);
    }
  }

  (name.to_string(), false, 0)
}

fn sort_cluster_names(names: &mut [String]) {
  names.sort_by_key(|name| cluster_sort_key(name));
}

fn init_smc() -> WithError<(SMC, Vec<String>, Vec<String>)> {
  let mut smc = SMC::new()?;
  const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    // eprintln!("init_smc found key: {}", name);

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
      // "Tp" – performance cores, "Te" – efficiency cores
      name if name.starts_with("Tp") || name.starts_with("Te") => cpu_sensors.push(name.clone()),
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
      ("Energy Model", None),                  // cpu/gpu/ane power
      ("CPU Stats", Some(CPU_FREQ_DICE_SUBG)), // cpu freq by cluster
      ("CPU Stats", Some(CPU_FREQ_CORE_SUBG)), // cpu freq per core
      ("GPU Stats", Some(GPU_FREQ_DICE_SUBG)), // gpu freq
    ];

    let soc = SocInfo::new()?;
    let ior = IOReport::new(channels)?;
    let hid = IOHIDSensors::new()?;
    let (smc, smc_cpu_keys, smc_gpu_keys) = init_smc()?;

    Ok(Sampler { soc, ior, hid, smc, smc_cpu_keys, smc_gpu_keys })
  }

  fn gpu_freqs(&self) -> &[u32] {
    match self.soc.gpu_freqs.len() > 1 {
      true => &self.soc.gpu_freqs[1..],
      false => &self.soc.gpu_freqs,
    }
  }

  fn cpu_domains(&self) -> Vec<CpuDomainSpec> {
    let mut domains = Vec::new();
    if !self.soc.ecpu_freqs.is_empty() || self.soc.ecpu_cores > 0 {
      domains.push(CpuDomainSpec {
        domain: CpuDomain::Efficiency,
        channel_prefix: "ECPU",
        core_prefix: "ECPU",
        freqs: self.soc.ecpu_freqs.clone(),
        total_units: self.soc.ecpu_cores as u32,
        fallback_name: "ECPU",
      });
    }
    if !self.soc.pcpu_freqs.is_empty() || self.soc.pcpu_cores > 0 {
      domains.push(CpuDomainSpec {
        domain: CpuDomain::Performance,
        channel_prefix: "PCPU",
        core_prefix: "PCPU",
        freqs: self.soc.pcpu_freqs.clone(),
        total_units: self.soc.pcpu_cores as u32,
        fallback_name: "PCPU",
      });
    }
    domains
  }

  fn cpu_domain_for_channel(&self, channel: &str) -> Option<CpuDomainSpec> {
    self.cpu_domains().into_iter().find(|domain| channel.contains(domain.channel_prefix))
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
      if val != 0.0 {
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
        gpu_values.push(*value);
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
    let cpu_domains = self.cpu_domains();

    // do several samples to smooth metrics
    // see: https://github.com/vladkens/macmon/issues/10
    for (sample, dt) in self.ior.get_samples(duration as u64, measures) {
      let mut cpu_group_usages: BTreeMap<CpuDomain, Vec<(u32, f32)>> = BTreeMap::new();
      let mut cpu_group_core_usages: BTreeMap<CpuDomain, BTreeMap<u32, (u32, f32)>> =
        BTreeMap::new();
      let mut cpu_clusters: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
      let mut cpu_cluster_domains: BTreeMap<String, CpuDomain> = BTreeMap::new();
      let mut gpu_clusters: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
      let mut rs = Metrics::default();
      let gpu_freqs = self.gpu_freqs();

      for x in sample {
        if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
          if let Some(domain) = self.cpu_domain_for_channel(&x.channel) {
            let usage = calc_freq(x.item, &domain.freqs);
            cpu_group_usages.entry(domain.domain).or_default().push(usage);
            if let Some(idx) = cpu_core_index(&x.channel, domain.core_prefix) {
              cpu_group_core_usages.entry(domain.domain).or_default().insert(idx, usage);
            }
            continue;
          }
        }

        if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_DICE_SUBG {
          if let Some(domain) = self.cpu_domain_for_channel(&x.channel) {
            if let Some(name) = cluster_key(&x.channel) {
              cpu_clusters.entry(name.clone()).or_default().push(calc_freq(x.item, &domain.freqs));
              cpu_cluster_domains.insert(name, domain.domain);
            }
            continue;
          }
        }

        if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
          if let Some(name) = cluster_key(&x.channel) {
            gpu_clusters.entry(name).or_default().push(calc_freq(x.item, gpu_freqs));
          }
        }

        if x.group == "Energy Model" {
          match x.channel.as_str() {
            "GPU Energy" => rs.power.gpu += cfio_watts(x.item, &x.unit, dt)?,
            // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
            c if c.ends_with("CPU Energy") => rs.power.cpu += cfio_watts(x.item, &x.unit, dt)?,
            // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
            c if c.starts_with("ANE") => rs.power.ane += cfio_watts(x.item, &x.unit, dt)?,
            c if c.starts_with("DRAM") => rs.power.ram += cfio_watts(x.item, &x.unit, dt)?,
            c if c.starts_with("GPU SRAM") => rs.power.gpu_ram += cfio_watts(x.item, &x.unit, dt)?,
            _ => {}
          }
        }
      }

      let mut gpu_usage = (0, 0.0);

      if !cpu_clusters.is_empty() {
        let mut cpu_cluster_units = BTreeMap::new();
        for (name, items) in cpu_clusters {
          let Some(domain_id) = cpu_cluster_domains.get(&name).copied() else { continue };
          let Some(domain) = cpu_domains.iter().find(|candidate| candidate.domain == domain_id)
          else {
            continue;
          };
          let (freq, usage) = calc_freq_final(&items, &domain.freqs);
          rs.usage.cpu.insert(name, (freq, usage, 0));
        }
        for domain in &cpu_domains {
          let mut cluster_names = cpu_cluster_domains
            .iter()
            .filter(|(_, domain_id)| **domain_id == domain.domain)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
          sort_cluster_names(&mut cluster_names);
          cpu_cluster_units.extend(distribute_units(&cluster_names, domain.total_units));
        }
        for (name, units) in &cpu_cluster_units {
          if let Some((_, _, cluster_units)) = rs.usage.cpu.get_mut(name) {
            *cluster_units = *units;
          }
        }

        for domain in &cpu_domains {
          let mut cluster_names = cpu_cluster_domains
            .iter()
            .filter(|(_, domain_id)| **domain_id == domain.domain)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
          sort_cluster_names(&mut cluster_names);

          let core_usages = cpu_group_core_usages.get(&domain.domain).cloned().unwrap_or_default();
          for (name, usage) in
            cluster_usage_from_cores(&core_usages, &cluster_names, &cpu_cluster_units)
          {
            if let Some((_, cluster_usage, _)) = rs.usage.cpu.get_mut(&name) {
              *cluster_usage = usage;
            }
          }
        }
      }

      if !gpu_clusters.is_empty() {
        for (name, items) in gpu_clusters {
          let (freq, usage) = calc_freq_final(&items, gpu_freqs);
          rs.usage.gpu.insert(name, (freq, usage, 0));
        }
        let names = rs.usage.gpu.keys().cloned().collect::<Vec<_>>();
        for (name, units) in distribute_units(&names, self.soc.gpu_cores as u32) {
          if let Some((_, _, cluster_units)) = rs.usage.gpu.get_mut(&name) {
            *cluster_units = units;
          }
        }
      }

      for domain in &cpu_domains {
        let has_clusters =
          cpu_cluster_domains.values().any(|domain_id| *domain_id == domain.domain);
        if has_clusters {
          continue;
        }

        let usages = cpu_group_usages.get(&domain.domain).cloned().unwrap_or_default();
        if usages.is_empty() {
          continue;
        }

        let (freq, usage) = calc_freq_final(&usages, &domain.freqs);
        rs.usage.cpu.insert(domain.fallback_name.to_string(), (freq, usage, domain.total_units));
      }

      let gpu_cluster_values = rs.usage.gpu.iter().map(|(_, usage)| *usage).collect::<Vec<_>>();
      if !gpu_cluster_values.is_empty() {
        gpu_usage = calc_freq_avg_weighted(&gpu_cluster_values, *gpu_freqs.first().unwrap_or(&0));
      }

      if rs.usage.gpu.is_empty() {
        rs.usage
          .gpu
          .insert("GPU".to_string(), (gpu_usage.0, gpu_usage.1, self.soc.gpu_cores as u32));
      }

      results.push(rs);
    }

    let mut rs = Metrics::default();
    let measures = results.len() as u32;
    rs.power.cpu = zero_div(results.iter().map(|x| x.power.cpu).sum(), measures as f32);
    rs.power.gpu = zero_div(results.iter().map(|x| x.power.gpu).sum(), measures as f32);
    rs.power.ane = zero_div(results.iter().map(|x| x.power.ane).sum(), measures as f32);
    rs.power.ram = zero_div(results.iter().map(|x| x.power.ram).sum(), measures as f32);
    rs.power.gpu_ram = zero_div(results.iter().map(|x| x.power.gpu_ram).sum(), measures as f32);

    let mut cpu_cluster_items: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
    let mut gpu_cluster_items: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
    let mut cpu_cluster_units: BTreeMap<String, u32> = BTreeMap::new();
    let mut gpu_cluster_units: BTreeMap<String, u32> = BTreeMap::new();
    for sample in &results {
      for (name, (freq, usage, units)) in &sample.usage.cpu {
        cpu_cluster_items.entry(name.clone()).or_default().push((*freq, *usage));
        cpu_cluster_units.entry(name.clone()).or_insert(*units);
      }
      for (name, (freq, usage, units)) in &sample.usage.gpu {
        gpu_cluster_items.entry(name.clone()).or_default().push((*freq, *usage));
        gpu_cluster_units.entry(name.clone()).or_insert(*units);
      }
    }

    let gpu_freqs = self.gpu_freqs();
    for (name, items) in cpu_cluster_items {
      let min_freq = items.iter().map(|(freq, _)| *freq).min().unwrap_or(0);
      let (freq, usage) = calc_freq_avg(&items, min_freq);
      let units = *cpu_cluster_units.get(&name).unwrap_or(&0);
      rs.usage.cpu.insert(name, (freq, usage, units));
    }

    for (name, items) in gpu_cluster_items {
      let (freq, usage) = calc_freq_avg(&items, *gpu_freqs.first().unwrap_or(&0));
      let units = *gpu_cluster_units.get(&name).unwrap_or(&0);
      rs.usage.gpu.insert(name, (freq, usage, units));
    }

    let gpu_cluster_values = rs.usage.gpu.iter().map(|(_, usage)| *usage).collect::<Vec<_>>();
    let mut gpu_usage = (0, 0.0);
    if !gpu_cluster_values.is_empty() {
      gpu_usage = calc_freq_avg_weighted(&gpu_cluster_values, *gpu_freqs.first().unwrap_or(&0));
    }

    if rs.usage.gpu.is_empty() {
      rs.usage.gpu.insert("GPU".to_string(), (gpu_usage.0, gpu_usage.1, self.soc.gpu_cores as u32));
    }

    rs.power.all = rs.power.cpu + rs.power.gpu + rs.power.ane;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

    rs.power.sys = match self.get_sys_power() {
      Ok(val) => val.max(rs.power.all),
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
  use super::{
    Metrics, UsageMetrics, calc_freq_from_residencies, cluster_key, cluster_usage_from_cores,
    sort_cluster_names,
  };
  use std::collections::BTreeMap;

  #[test]
  fn calc_freq_with_matching_states() {
    let items = vec![("IDLE".to_string(), 50), ("F1".to_string(), 25), ("F2".to_string(), 25)];
    let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

    assert_eq!(freq, 1500);
    assert!((usage - 0.375f32).abs() < 1e-6f32);
  }

  #[test]
  fn calc_freq_with_mismatched_states_uses_tail_activity() {
    let items = vec![
      ("IDLE".to_string(), 50),
      ("S1".to_string(), 0),
      ("S2".to_string(), 0),
      ("S3".to_string(), 0),
      ("S4".to_string(), 50),
    ];
    let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

    assert_eq!(freq, 2000);
    assert!((usage - 0.5f32).abs() < 1e-6f32);
  }

  #[test]
  fn calc_freq_with_only_idle_returns_zero_usage() {
    let items = vec![("IDLE".to_string(), 100)];
    let (freq, usage) = calc_freq_from_residencies(&items, &[1200, 3000]);

    assert_eq!(freq, 1200);
    assert_eq!(usage, 0.0);
  }

  #[test]
  fn cluster_keys_are_raw_and_stable() {
    assert_eq!(cluster_key("ECPU"), Some("ECPU".to_string()));
    assert_eq!(cluster_key("PCPU1"), Some("PCPU1".to_string()));
    assert_eq!(cluster_key("GPUPH"), Some("GPUPH".to_string()));
    assert_eq!(cluster_key("GPU-2"), Some("GPU2".to_string()));
  }

  #[test]
  fn metrics_preserve_missing_cpu_clusters() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: BTreeMap::from([("PCPU".to_string(), (3200, 0.42, 4))]),
        gpu: BTreeMap::from([("GPU".to_string(), (800, 0.15, 10))]),
      },
      ..Default::default()
    };

    assert_eq!(metrics.usage.cpu.get("PCPU"), Some(&(3200, 0.42, 4)));
    assert_eq!(metrics.usage.gpu.get("GPU"), Some(&(800, 0.15, 10)));
    assert!(!metrics.usage.cpu.contains_key("ECPU"));
  }

  #[test]
  fn metrics_preserve_dynamic_cluster_names() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: BTreeMap::from([
          ("PCPU".to_string(), (3030, 0.31, 4)),
          ("PCPU1".to_string(), (3220, 0.44, 4)),
        ]),
        gpu: BTreeMap::from([("GPUPH".to_string(), (1296, 0.2, 16))]),
      },
      ..Default::default()
    };

    assert_eq!(metrics.usage.cpu.len(), 2);
    assert_eq!(metrics.usage.cpu.get("PCPU"), Some(&(3030, 0.31, 4)));
    assert_eq!(metrics.usage.cpu.get("PCPU1"), Some(&(3220, 0.44, 4)));
    assert_eq!(metrics.usage.gpu.get("GPUPH"), Some(&(1296, 0.2, 16)));
  }

  #[test]
  fn metrics_serialize_with_expected_shape() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: BTreeMap::from([("ECPU".to_string(), (1181, 0.33, 4))]),
        gpu: BTreeMap::from([("GPU".to_string(), (461, 0.21, 10))]),
      },
      power: super::PowerMetrics {
        cpu: 0.2,
        gpu: 0.01,
        ram: 0.11,
        sys: 5.8,
        gpu_ram: 0.001,
        ane: 0.0,
        all: 0.21,
      },
      memory: super::MemMetrics { ram_total: 1, ram_usage: 2, swap_total: 3, swap_usage: 4 },
      temp: super::TempMetrics { cpu_temp_avg: 42.0, gpu_temp_avg: 36.0 },
    };

    let value = serde_json::to_value(&metrics).unwrap();

    assert_eq!(value["usage"]["cpu"]["ECPU"][0], serde_json::json!(1181));
    assert!((value["usage"]["cpu"]["ECPU"][1].as_f64().unwrap() - 0.33).abs() < 1e-6);
    assert_eq!(value["usage"]["cpu"]["ECPU"][2], serde_json::json!(4));
    assert_eq!(value["usage"]["gpu"]["GPU"][0], serde_json::json!(461));
    assert!((value["usage"]["gpu"]["GPU"][1].as_f64().unwrap() - 0.21).abs() < 1e-6);
    assert_eq!(value["usage"]["gpu"]["GPU"][2], serde_json::json!(10));
    assert!((value["power"]["cpu"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert_eq!(value["memory"]["swap_usage"], serde_json::json!(4));
    assert!((value["temp"]["cpu_temp_avg"].as_f64().unwrap() - 42.0).abs() < 1e-6);
  }

  #[test]
  fn cluster_usage_is_averaged_across_cluster_units() {
    let cores = BTreeMap::from([
      (0, (4512, 1.0)),
      (1, (1260, 0.0)),
      (2, (1260, 0.0)),
      (3, (1260, 0.0)),
      (4, (1260, 0.0)),
      (5, (1260, 0.0)),
      (6, (1260, 0.0)),
      (7, (1260, 0.0)),
      (8, (1260, 0.0)),
      (9, (1260, 0.0)),
    ]);
    let cluster_units = BTreeMap::from([("PCPU".to_string(), 5), ("PCPU1".to_string(), 5)]);
    let cluster_names = vec!["PCPU".to_string(), "PCPU1".to_string()];

    let usage = cluster_usage_from_cores(&cores, &cluster_names, &cluster_units);

    assert_eq!(usage.get("PCPU"), Some(&0.2));
    assert_eq!(usage.get("PCPU1"), Some(&0.0));
  }

  #[test]
  fn cluster_names_are_sorted_naturally() {
    let mut clusters = vec!["PCPU10".to_string(), "PCPU2".to_string(), "PCPU".to_string()];

    sort_cluster_names(&mut clusters);

    assert_eq!(clusters, vec!["PCPU".to_string(), "PCPU2".to_string(), "PCPU10".to_string()]);
  }
}
