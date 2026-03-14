use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;
use std::time::Instant;

use crate::diag::startup_log;
use crate::platform::{IOReport, SMC, cfio_get_residencies, libc_ram, libc_swap};
use crate::sources::{CpuDomainInfo, SocInfo};

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

fn find_named<'a, T>(items: &'a [(String, T)], name: &str) -> Option<&'a T> {
  items.iter().find(|(item_name, _)| item_name == name).map(|(_, value)| value)
}

fn find_named_mut<'a, T>(items: &'a mut [(String, T)], name: &str) -> Option<&'a mut T> {
  items.iter_mut().find(|(item_name, _)| item_name == name).map(|(_, value)| value)
}

fn push_or_extend_named<T>(items: &mut Vec<(String, Vec<T>)>, name: &str, value: T) {
  if let Some(values) = find_named_mut(items, name) {
    values.push(value);
  } else {
    items.push((name.to_string(), vec![value]));
  }
}

fn push_or_replace_indexed<T>(items: &mut Vec<(u32, T)>, index: u32, value: T) {
  if let Some((_, existing)) = items.iter_mut().find(|(item_index, _)| *item_index == index) {
    *existing = value;
  } else {
    items.push((index, value));
  }
}

fn sort_indexed_samples_by_core<T>(items: &mut [(u32, T)]) {
  items.sort_by_key(|(index, _)| *index);
}

fn push_or_replace_usage(items: &mut Vec<UsageEntry>, entry: UsageEntry) {
  if let Some(existing) = items.iter_mut().find(|item| item.name == entry.name) {
    *existing = entry;
  } else {
    items.push(entry);
  }
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
  cluster_units: &[(String, u32)],
) -> Vec<(String, f32)> {
  let core_values = cores.iter().map(|(_, value)| *value).collect::<Vec<_>>();
  let mut usage_by_cluster = Vec::new();
  let mut offset = 0usize;
  for name in cluster_names {
    let units = find_named(cluster_units, name).copied().unwrap_or(0);
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

fn cpu_domain_for_channel<'a>(
  cpu_domains: &'a [CpuDomainInfo],
  channel: &str,
) -> Option<&'a CpuDomainInfo> {
  cpu_domains.iter().find(|domain| channel.contains(domain.name.as_str()))
}

#[derive(Debug, Default)]
struct SmcSensors {
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
}

fn init_smc() -> WithError<(SMC, SmcSensors)> {
  const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "
  startup_log("lib smc: connection start");
  let mut smc = SMC::new()?;
  startup_log("lib smc: connection ready");

  startup_log("lib smc: sensor discovery start");
  let total_start = Instant::now();

  let read_start = Instant::now();
  let names = smc.read_all_keys()?;
  let read_elapsed = read_start.elapsed();

  let filter_start = Instant::now();
  let mut sensors = SmcSensors::default();
  for name in &names {
    let is_cpu = name.starts_with("Tp") || name.starts_with("Te");
    let is_gpu = name.starts_with("Tg");
    if !is_cpu && !is_gpu {
      continue;
    }

    let key = match smc.read_key_info(name) {
      Ok(key) => key,
      Err(_) => continue,
    };

    if key.data_size != 4 || key.data_type != FLOAT_TYPE {
      continue;
    }

    if is_cpu {
      sensors.cpu_keys.push(name.clone());
    } else if is_gpu {
      sensors.gpu_keys.push(name.clone());
    }
  }
  let filter_elapsed = filter_start.elapsed();

  startup_log(format!(
    "lib smc: sensor discovery complete (indexed_keys={}, cpu_sensors={}, gpu_sensors={}, read_all_keys={:.3}s, filter={:.3}s, total={:.3}s)",
    names.len(),
    sensors.cpu_keys.len(),
    sensors.gpu_keys.len(),
    read_elapsed.as_secs_f64(),
    filter_elapsed.as_secs_f64(),
    total_start.elapsed().as_secs_f64()
  ));

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
    let channels = vec![
      ("Energy Model", None),                  // cpu/gpu/ane power
      ("CPU Stats", Some(CPU_FREQ_DICE_SUBG)), // cpu freq by cluster
      ("CPU Stats", Some(CPU_FREQ_CORE_SUBG)), // cpu freq per core
      ("GPU Stats", Some(GPU_FREQ_DICE_SUBG)), // gpu freq
    ];

    let soc = SocInfo::new()?;
    startup_log(format!(
      "lib sampler: soc info ready (chip={}, cpu_domains={}, gpu_cores={})",
      soc.chip_name,
      soc.cpu_domains.len(),
      soc.gpu_cores
    ));
    let io_report = IOReport::new(channels)?;
    startup_log("lib sampler: IOReport subscription ready");
    let (smc, smc_sensors) = init_smc()?;
    startup_log(format!(
      "lib sampler: SMC ready (cpu_sensors={}, gpu_sensors={})",
      smc_sensors.cpu_keys.len(),
      smc_sensors.gpu_keys.len()
    ));

    startup_log("lib sampler: init complete");
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
      let val = match self.smc.read_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
      if val != 0.0 {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = match self.smc.read_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
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

  fn get_sys_power(&mut self) -> WithError<f32> {
    let val = self.smc.read_val("PSTR")?;
    let val = f32::from_le_bytes(val.data.clone().try_into().unwrap());
    Ok(val)
  }

  pub fn get_metrics(&mut self) -> WithError<Metrics> {
    let cpu_domains = self.soc.cpu_domains.clone();
    let gpu_freqs = self.gpu_freqs().to_vec();
    let mut cpu_group_usages: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut cpu_group_core_usages: Vec<(String, Vec<(u32, (u32, f32))>)> = Vec::new();
    let mut cpu_clusters: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut cpu_cluster_domains: Vec<(String, String)> = Vec::new();
    let mut gpu_clusters: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut rs = Metrics::default();
    let mut cpu_usage = Vec::new();
    let mut gpu_usage = Vec::new();

    for x in self.io_report.get_sample() {
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        if let Some(domain) = cpu_domain_for_channel(&cpu_domains, &x.channel) {
          let usage = calc_freq(x.item, &domain.freqs);
          push_or_extend_named(&mut cpu_group_usages, &domain.name, usage);
          if let Some(idx) = cpu_core_index(&x.channel, domain.core_prefix.as_str()) {
            if let Some(samples) = find_named_mut(&mut cpu_group_core_usages, &domain.name) {
              push_or_replace_indexed(samples, idx, usage);
            } else {
              cpu_group_core_usages.push((domain.name.clone(), vec![(idx, usage)]));
            }
          }
          continue;
        }
      }

      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_DICE_SUBG {
        if let Some(domain) = cpu_domain_for_channel(&cpu_domains, &x.channel) {
          if let Some(name) = cluster_key(&x.channel) {
            push_or_extend_named(&mut cpu_clusters, &name, calc_freq(x.item, &domain.freqs));
            if let Some(existing) = find_named_mut(&mut cpu_cluster_domains, &name) {
              *existing = domain.name.clone();
            } else {
              cpu_cluster_domains.push((name, domain.name.clone()));
            }
          }
          continue;
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        if let Some(name) = cluster_key(&x.channel) {
          push_or_extend_named(&mut gpu_clusters, &name, calc_freq(x.item, &gpu_freqs));
        }
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
      let mut cpu_cluster_units = Vec::new();
      for (name, items) in &cpu_clusters {
        let Some(domain_id) = find_named(&cpu_cluster_domains, name).cloned() else { continue };
        let Some(domain) = cpu_domains.iter().find(|candidate| candidate.name == domain_id) else {
          continue;
        };
        let (freq, usage) = calc_freq_final(items, &domain.freqs);
        push_or_replace_usage(
          &mut cpu_usage,
          UsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 },
        );
      }
      for domain in &cpu_domains {
        let mut cluster_names = cpu_cluster_domains
          .iter()
          .filter(|(_, domain_id)| domain_id == &domain.name)
          .map(|(name, _)| name.clone())
          .collect::<Vec<_>>();
        sort_cluster_names(&mut cluster_names);
        cpu_cluster_units.extend(distribute_units(&cluster_names, domain.units));
      }
      for (name, units) in &cpu_cluster_units {
        if let Some(entry) = cpu_usage.iter_mut().find(|entry| entry.name == *name) {
          entry.units = *units;
        }
      }

      for domain in &cpu_domains {
        let mut cluster_names = cpu_cluster_domains
          .iter()
          .filter(|(_, domain_id)| domain_id == &domain.name)
          .map(|(name, _)| name.clone())
          .collect::<Vec<_>>();
        sort_cluster_names(&mut cluster_names);

        let mut core_usages = find_named(&cpu_group_core_usages, &domain.name).cloned().unwrap_or_default();
        sort_indexed_samples_by_core(&mut core_usages);
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
      for (name, items) in &gpu_clusters {
        let (freq, usage) = calc_freq_final(items, &gpu_freqs);
        push_or_replace_usage(
          &mut gpu_usage,
          UsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 },
        );
      }
      let names = gpu_usage.iter().map(|entry| entry.name.clone()).collect::<Vec<_>>();
      for (name, units) in distribute_units(&names, self.soc.gpu_cores as u32) {
        if let Some(entry) = gpu_usage.iter_mut().find(|entry| entry.name == name) {
          entry.units = units;
        }
      }
    }

    for domain in &cpu_domains {
      let has_clusters =
        cpu_cluster_domains.iter().any(|(_, domain_id)| domain_id == &domain.name);
      if has_clusters {
        continue;
      }

      let usages = find_named(&cpu_group_usages, &domain.name).cloned().unwrap_or_default();
      if usages.is_empty() {
        continue;
      }

      let (freq, usage) = calc_freq_final(&usages, &domain.freqs);
      push_or_replace_usage(
        &mut cpu_usage,
        UsageEntry {
          name: domain.name.clone(),
          freq_mhz: freq,
          usage,
          units: domain.units,
        },
      );
    }

    if gpu_usage.is_empty() {
      gpu_usage.push(UsageEntry {
        name: "GPU".to_string(),
        freq_mhz: 0,
        usage: 0.0,
        units: self.soc.gpu_cores as u32,
      });
    }

    cpu_usage.sort_by(|a, b| a.name.cmp(&b.name));
    rs.usage.cpu = cpu_usage;
    gpu_usage.sort_by(|a, b| a.name.cmp(&b.name));
    rs.usage.gpu = gpu_usage;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp_smc()?;

    rs.power.all = rs.power.cpu + rs.power.gpu + rs.power.ane + rs.power.ram + rs.power.gpu_ram;
    rs.power.sys = match self.get_sys_power() {
      Ok(val) => val.max(rs.power.all),
      Err(_) => 0.0,
    };

    Ok(rs)
  }

  // Getter for the `soc` field
  pub fn get_soc_info(&self) -> &SocInfo {
    &self.soc
  }
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
