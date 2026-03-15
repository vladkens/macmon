use std::collections::{BTreeMap, HashMap};

use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;

use crate::diag::startup_log;
use crate::platform::{IOReport, SMC, cfio_collect_residencies, libc_ram, libc_swap};
use crate::sources::{SocInfo, get_soc_info};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default, Clone, Serialize)]
pub struct TempMetrics {
  pub cpu_avg: f32, // Celsius
  pub gpu_avg: f32, // Celsius
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct UsageEntry {
  pub name: String,
  pub freq_mhz: u32,
  pub usage: f32,
  pub units: u32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct UsageMetrics {
  pub cpu: Vec<UsageEntry>,
  pub gpu: Vec<UsageEntry>,
}

#[derive(Debug, Default, Serialize)]
pub struct PowerMetrics {
  pub package: f32, // SoC/package power reported by the sampler.
  pub cpu: f32,     // CPU power included in `package`.
  pub gpu: f32,     // GPU core power included in `package`.
  pub ram: f32,     // DRAM power included in `package`.
  pub gpu_ram: f32, // GPU SRAM power included in `package`.
  pub ane: f32,     // ANE power included in `package`.
  pub board: f32,   // System Total (`PSTR`), independent from battery/DC-in readings.
  pub battery: f32, // Battery rail power (`PPBR`).
  pub dc_in: f32,   // External DC input power (`PDTR`).
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  pub usage: UsageMetrics,
  pub power: PowerMetrics,
  pub memory: MemMetrics,
  pub temp: TempMetrics,
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
    eprintln!(
      "macmon: residency state count ({}) does not match DVFS table length ({}) for states {:?}",
      active.len(),
      freqs.len(),
      items.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
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
  let items = cfio_collect_residencies(item); // (ns, freq)
  calc_freq_from_residencies(&items, freqs)
}

fn calc_freq_avg(items: &[(u32, f32)], min_freq: u32) -> (u32, f32) {
  let avg_freq = zero_div(items.iter().map(|x| x.0 as f32).sum(), items.len() as f32);
  let avg_perc = zero_div(items.iter().map(|x| x.1).sum(), items.len() as f32);
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
  cores: &[(u32, (u32, f32))],
  cluster_names: &[String],
  cluster_units: &HashMap<String, u32>,
) -> Vec<(String, f32)> {
  let core_values = cores.iter().map(|(_, value)| *value).collect::<Vec<_>>();
  let mut usage_by_cluster = Vec::new();
  let mut offset = 0usize;
  for name in cluster_names {
    let units = cluster_units.get(name).copied().unwrap_or(0);
    let end = (offset + units as usize).min(core_values.len());
    let usage =
      zero_div(core_values[offset..end].iter().map(|(_, usage)| *usage).sum::<f32>(), units as f32);
    usage_by_cluster.push((name.clone(), usage));
    offset = end;
  }

  usage_by_cluster
}

fn distribute_units(names: &[String], total_units: u32) -> Vec<(String, u32)> {
  let mut result = Vec::new();
  if names.is_empty() {
    return result;
  }

  let len = names.len() as u32;
  let base = zero_div(total_units, len);
  let remainder = total_units % len;
  for (idx, name) in names.iter().enumerate() {
    let extra = if (idx as u32) < remainder { 1 } else { 0 };
    result.push((name.clone(), base + extra));
  }

  result
}

#[derive(Debug, Default)]
struct SmcSensors {
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
}

fn init_smc() -> WithError<(SMC, SmcSensors)> {
  let mut smc = SMC::new()?;
  startup_log("lib smc: connection ready");

  let names = smc.read_all_keys()?;
  startup_log(format!("lib smc: sensors discovered (indexed_keys={})", names.len()));

  let mut sensors = SmcSensors::default();

  for name in &names {
    let is_cpu = name.starts_with("Tp0") || name.starts_with("Tp1");
    let is_gpu = name.starts_with("Tg0");
    if !is_cpu && !is_gpu {
      continue;
    }

    if smc.read_float_val(name).is_err() {
      continue;
    }

    if is_cpu {
      sensors.cpu_keys.push(name.clone());
    } else if is_gpu {
      sensors.gpu_keys.push(name.clone());
    }
  }

  Ok((smc, sensors))
}

// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  io_report: IOReport,
  smc: SMC,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    startup_log("lib sampler: init start");

    let soc = get_soc_info()?;
    startup_log(format!(
      "lib sampler: soc info ready (chip={}, cpu_domains={}, gpu_cores={})",
      soc.chip_name,
      soc.cpu_domains.len(),
      soc.gpu_cores
    ));

    let channels = |group: &str, subgroup: &str, channel: &str, _unit: &str| {
      if group == "Energy Model" {
        return channel == "GPU Energy"
          || channel.ends_with("CPU Energy")
          || channel.starts_with("ANE")
          || channel.starts_with("DRAM")
          || channel.starts_with("GPU SRAM")
          || channel.starts_with("DISP");
      }
      if group == "CPU Stats" {
        return subgroup == CPU_FREQ_DICE_SUBG || subgroup == CPU_FREQ_CORE_SUBG;
      }
      return group == "GPU Stats" && subgroup == GPU_FREQ_DICE_SUBG
    };
    let io_report = IOReport::new(Some(channels))?;
    startup_log("lib sampler: IOReport subscription ready");

    let (smc, smc_sensors) = init_smc()?;
    startup_log(format!(
      "lib sampler: SMC ready (cpu_sensors={}, gpu_sensors={})",
      smc_sensors.cpu_keys.len(),
      smc_sensors.gpu_keys.len()
    ));

    Ok(Sampler {
      soc,
      io_report,
      smc,
      smc_cpu_keys: smc_sensors.cpu_keys,
      smc_gpu_keys: smc_sensors.gpu_keys,
    })
  }

  fn gpu_freqs(&self) -> &[u32] {
    match self.soc.gpu_freqs.len() > 1 {
      true => &self.soc.gpu_freqs[1..],
      false => &self.soc.gpu_freqs,
    }
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = match self.smc.read_float_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      if val != 0.0 {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = match self.smc.read_float_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      if val != 0.0 {
        gpu_metrics.push(val);
      }
    }

    let cpu_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_avg, gpu_avg })
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram()?;
    let (swap_usage, swap_total) = libc_swap()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  pub fn get_metrics(&mut self) -> WithError<Metrics> {
    let cpu_domains = self.soc.cpu_domains.clone();
    let gpu_freqs = self.gpu_freqs().to_vec();
    let mut cpu_group_usages = vec![Vec::new(); cpu_domains.len()];
    let mut cpu_group_core_usages = vec![BTreeMap::new(); cpu_domains.len()];
    let mut cpu_clusters: HashMap<String, Vec<(u32, f32)>> = HashMap::new();
    let mut cpu_cluster_domains: HashMap<String, String> = HashMap::new();
    let mut gpu_clusters: HashMap<String, Vec<(u32, f32)>> = HashMap::new();
    let mut rs = Metrics::default();
    let mut cpu_usage = Vec::new();
    let mut gpu_usage = Vec::new();

    for x in self.io_report.next_sample() {
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        if let Some(domain_idx) =
          cpu_domains.iter().position(|domain| x.channel.contains(domain.name.as_str()))
        {
          let domain = &cpu_domains[domain_idx];
          let usage = calc_freq(x.channel_item, &domain.freqs);
          cpu_group_usages[domain_idx].push(usage);
          if let Some(idx) = cpu_core_index(&x.channel, domain.core_prefix.as_str()) {
            cpu_group_core_usages[domain_idx].insert(idx, usage);
          }
          continue;
        }
      }

      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_DICE_SUBG {
        if let Some(domain) =
          cpu_domains.iter().find(|domain| x.channel.contains(domain.name.as_str()))
        {
          cpu_clusters.entry(x.channel.clone()).or_default().push(calc_freq(x.channel_item, &domain.freqs));
          cpu_cluster_domains.insert(x.channel.clone(), domain.name.clone());
          continue;
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        gpu_clusters.entry(x.channel.clone()).or_default().push(calc_freq(x.channel_item, &gpu_freqs));
      }

      if x.group == "Energy Model" {
        match x.channel.as_str() {
          "GPU Energy" => rs.power.gpu += x.watts()?,
          // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
          c if c.ends_with("CPU Energy") => rs.power.cpu += x.watts()?,
          // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
          c if c.starts_with("ANE") => rs.power.ane += x.watts()?,
          c if c.starts_with("DRAM") => rs.power.ram += x.watts()?,
          c if c.starts_with("GPU SRAM") => rs.power.gpu_ram += x.watts()?,
          _ => {}
        }
      }
    }

    if !cpu_clusters.is_empty() {
      let mut cpu_cluster_units = HashMap::new();
      for domain in &cpu_domains {
        let cluster_names = cpu_cluster_domains
          .iter()
          .filter(|(_, domain_id)| *domain_id == &domain.name)
          .map(|(name, _)| name.clone())
          .collect::<Vec<_>>();
        for name in &cluster_names {
          let Some(items) = cpu_clusters.get(name) else { continue };
          let (freq, usage) = calc_freq_avg(items, *domain.freqs.first().unwrap_or(&0));
          cpu_usage.push(UsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 });
        }
        cpu_cluster_units.extend(distribute_units(&cluster_names, domain.units));
      }
      for (name, units) in &cpu_cluster_units {
        if let Some(entry) = cpu_usage.iter_mut().find(|entry| entry.name == *name) {
          entry.units = *units;
        }
      }

      for (domain_idx, domain) in cpu_domains.iter().enumerate() {
        let cluster_names = cpu_cluster_domains
          .iter()
          .filter(|(_, domain_id)| *domain_id == &domain.name)
          .map(|(name, _)| name.clone())
          .collect::<Vec<_>>();

        let core_usages =
          cpu_group_core_usages[domain_idx].iter().map(|(index, value)| (*index, *value)).collect::<Vec<_>>();
        for (name, usage) in
          cluster_usage_from_cores(&core_usages, &cluster_names, &cpu_cluster_units)
        {
          if let Some(entry) = cpu_usage.iter_mut().find(|entry| entry.name == name) {
            entry.usage = usage;
          }
        }
      }
    }

    if !gpu_clusters.is_empty() {
      let names = gpu_clusters.keys().cloned().collect::<Vec<_>>();
      for name in &names {
        let Some(items) = gpu_clusters.get(name) else { continue };
        let (freq, usage) = calc_freq_avg(items, *gpu_freqs.first().unwrap_or(&0));
        gpu_usage.push(UsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 });
      }
      for (name, units) in distribute_units(&names, self.soc.gpu_cores as u32) {
        if let Some(entry) = gpu_usage.iter_mut().find(|entry| entry.name == name) {
          entry.units = units;
        }
      }
    }

    for (domain_idx, domain) in cpu_domains.iter().enumerate() {
      let has_clusters = cpu_cluster_domains.values().any(|domain_id| domain_id == &domain.name);
      if has_clusters {
        continue;
      }

      let usages = &cpu_group_usages[domain_idx];
      if usages.is_empty() {
        continue;
      }

      let (freq, usage) = calc_freq_avg(usages, *domain.freqs.first().unwrap_or(&0));
      cpu_usage.push(UsageEntry {
        name: domain.name.clone(),
        freq_mhz: freq,
        usage,
        units: domain.units,
      });
    }

    rs.usage.cpu = cpu_usage;
    rs.usage.gpu = gpu_usage;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp_smc()?;

    rs.power.package = rs.power.cpu + rs.power.gpu + rs.power.ane + rs.power.ram + rs.power.gpu_ram;
    rs.power.board = self.smc.read_float_val("PSTR").unwrap_or(0.0);
    rs.power.battery = self.smc.read_float_val("PPBR").unwrap_or(0.0);
    rs.power.dc_in = self.smc.read_float_val("PDTR").unwrap_or(0.0);

    Ok(rs)
  }
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
