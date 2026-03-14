use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;

use crate::platform::{IOHIDSensors, IOReport, SMC, cfio_get_residencies, cfio_watts, libc_ram, libc_swap};
use crate::sources::{CpuDomainInfo, SocInfo};

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

fn sort_usage_entries_by_name(items: &mut [UsageEntry]) {
  items.sort_by(|a, b| a.name.cmp(&b.name));
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
  cpu_domains.iter().find(|domain| channel.contains(domain.channel_prefix.as_str()))
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
    let cpu_domains = self.soc.cpu_domains.clone();
    let gpu_freqs = self.gpu_freqs().to_vec();
    let mut cpu_group_usages: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut cpu_group_core_usages: Vec<(String, Vec<(u32, (u32, f32))>)> = Vec::new();
    let mut cpu_clusters: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut cpu_cluster_domains: Vec<(String, String)> = Vec::new();
    let mut gpu_clusters: Vec<(String, Vec<(u32, f32)>)> = Vec::new();
    let mut rs = Metrics::default();
    let mut gpu_usage = (0, 0.0);
    let mut cpu_usage = Vec::new();
    let mut gpu_usage_entries = Vec::new();

    for x in self.ior.get_sample(duration as u64) {
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        if let Some(domain) = cpu_domain_for_channel(&cpu_domains, &x.channel) {
          let usage = calc_freq(x.item, &domain.freqs);
          push_or_extend_named(&mut cpu_group_usages, &domain.id, usage);
          if let Some(idx) = cpu_core_index(&x.channel, domain.core_prefix.as_str()) {
            if let Some(samples) = find_named_mut(&mut cpu_group_core_usages, &domain.id) {
              push_or_replace_indexed(samples, idx, usage);
            } else {
              cpu_group_core_usages.push((domain.id.clone(), vec![(idx, usage)]));
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
              *existing = domain.id.clone();
            } else {
              cpu_cluster_domains.push((name, domain.id.clone()));
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
          "GPU Energy" => rs.power.gpu += cfio_watts(x.item, &x.unit, duration as u64)?,
          // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
          c if c.ends_with("CPU Energy") => {
            rs.power.cpu += cfio_watts(x.item, &x.unit, duration as u64)?
          }
          // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
          c if c.starts_with("ANE") => {
            rs.power.ane += cfio_watts(x.item, &x.unit, duration as u64)?
          }
          c if c.starts_with("DRAM") => {
            rs.power.ram += cfio_watts(x.item, &x.unit, duration as u64)?
          }
          c if c.starts_with("GPU SRAM") => {
            rs.power.gpu_ram += cfio_watts(x.item, &x.unit, duration as u64)?
          }
          _ => {}
        }
      }
    }

    if !cpu_clusters.is_empty() {
      let mut cpu_cluster_units = Vec::new();
      for (name, items) in &cpu_clusters {
        let Some(domain_id) = find_named(&cpu_cluster_domains, name).cloned() else { continue };
        let Some(domain) = cpu_domains.iter().find(|candidate| candidate.id == domain_id) else {
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
          .filter(|(_, domain_id)| domain_id == &domain.id)
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
          .filter(|(_, domain_id)| domain_id == &domain.id)
          .map(|(name, _)| name.clone())
          .collect::<Vec<_>>();
        sort_cluster_names(&mut cluster_names);

        let mut core_usages = find_named(&cpu_group_core_usages, &domain.id).cloned().unwrap_or_default();
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
          &mut gpu_usage_entries,
          UsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 },
        );
      }
      let names = gpu_usage_entries.iter().map(|entry| entry.name.clone()).collect::<Vec<_>>();
      for (name, units) in distribute_units(&names, self.soc.gpu_cores as u32) {
        if let Some(entry) = gpu_usage_entries.iter_mut().find(|entry| entry.name == name) {
          entry.units = units;
        }
      }
    }

    for domain in &cpu_domains {
      let has_clusters =
        cpu_cluster_domains.iter().any(|(_, domain_id)| domain_id == &domain.id);
      if has_clusters {
        continue;
      }

      let usages = find_named(&cpu_group_usages, &domain.id).cloned().unwrap_or_default();
      if usages.is_empty() {
        continue;
      }

      let (freq, usage) = calc_freq_final(&usages, &domain.freqs);
      push_or_replace_usage(
        &mut cpu_usage,
        UsageEntry {
          name: domain.channel_prefix.clone(),
          freq_mhz: freq,
          usage,
          units: domain.units,
        },
      );
    }

    let gpu_cluster_values =
      gpu_usage_entries.iter().map(|entry| (entry.freq_mhz, entry.usage, entry.units)).collect::<Vec<_>>();
    if !gpu_cluster_values.is_empty() {
      gpu_usage = calc_freq_avg_weighted(&gpu_cluster_values, *gpu_freqs.first().unwrap_or(&0));
    }

    if gpu_usage_entries.is_empty() {
      gpu_usage_entries.push(UsageEntry {
        name: "GPU".to_string(),
        freq_mhz: gpu_usage.0,
        usage: gpu_usage.1,
        units: self.soc.gpu_cores as u32,
      });
    }

    sort_usage_entries_by_name(&mut cpu_usage);
    sort_usage_entries_by_name(&mut gpu_usage_entries);
    rs.usage.cpu = cpu_usage;
    rs.usage.gpu = gpu_usage_entries;

    rs.power.all = rs.power.cpu + rs.power.gpu + rs.power.ane + rs.power.ram + rs.power.gpu_ram;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

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
mod tests {
  use super::{
    Metrics, UsageEntry, UsageMetrics, calc_freq_from_residencies, cluster_key, cluster_usage_from_cores,
    cpu_domain_for_channel, distribute_units, sort_cluster_names, sort_indexed_samples_by_core,
  };
  use crate::sources::CpuDomainInfo;

  fn usage_entry<'a>(items: &'a [UsageEntry], name: &str) -> Option<&'a UsageEntry> {
    items.iter().find(|entry| entry.name == name)
  }

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
  fn cpu_domain_matching_uses_domain_metadata() {
    let domains = vec![
      CpuDomainInfo {
        id: "cpu0".to_string(),
        units: 4,
        freqs: vec![1000, 2000],
        channel_prefix: "ECPU".to_string(),
        core_prefix: "ECPU".to_string(),
      },
      CpuDomainInfo {
        id: "cpu1".to_string(),
        units: 8,
        freqs: vec![2000, 3000],
        channel_prefix: "PCPU".to_string(),
        core_prefix: "PCPU".to_string(),
      },
    ];

    assert_eq!(
      cpu_domain_for_channel(&domains, "PCPU1").map(|domain| domain.id.as_str()),
      Some("cpu1")
    );
    assert_eq!(
      cpu_domain_for_channel(&domains, "ECPU").map(|domain| domain.id.as_str()),
      Some("cpu0")
    );
  }

  #[test]
  fn metrics_preserve_missing_cpu_clusters() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: vec![UsageEntry { name: "PCPU".to_string(), freq_mhz: 3200, usage: 0.42, units: 4 }],
        gpu: vec![UsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.15, units: 10 }],
      },
      ..Default::default()
    };

    assert_eq!(
      usage_entry(&metrics.usage.cpu, "PCPU"),
      Some(&UsageEntry { name: "PCPU".to_string(), freq_mhz: 3200, usage: 0.42, units: 4 })
    );
    assert_eq!(
      usage_entry(&metrics.usage.gpu, "GPU"),
      Some(&UsageEntry { name: "GPU".to_string(), freq_mhz: 800, usage: 0.15, units: 10 })
    );
    assert!(usage_entry(&metrics.usage.cpu, "ECPU").is_none());
  }

  #[test]
  fn metrics_preserve_dynamic_cluster_names() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: vec![
          UsageEntry { name: "PCPU".to_string(), freq_mhz: 3030, usage: 0.31, units: 4 },
          UsageEntry { name: "PCPU1".to_string(), freq_mhz: 3220, usage: 0.44, units: 4 },
        ],
        gpu: vec![UsageEntry { name: "GPUPH".to_string(), freq_mhz: 1296, usage: 0.2, units: 16 }],
      },
      ..Default::default()
    };

    assert_eq!(metrics.usage.cpu.len(), 2);
    assert_eq!(
      usage_entry(&metrics.usage.cpu, "PCPU"),
      Some(&UsageEntry { name: "PCPU".to_string(), freq_mhz: 3030, usage: 0.31, units: 4 })
    );
    assert_eq!(
      usage_entry(&metrics.usage.cpu, "PCPU1"),
      Some(&UsageEntry { name: "PCPU1".to_string(), freq_mhz: 3220, usage: 0.44, units: 4 })
    );
    assert_eq!(
      usage_entry(&metrics.usage.gpu, "GPUPH"),
      Some(&UsageEntry { name: "GPUPH".to_string(), freq_mhz: 1296, usage: 0.2, units: 16 })
    );
  }

  #[test]
  fn metrics_serialize_with_expected_shape() {
    let metrics = Metrics {
      usage: UsageMetrics {
        cpu: vec![UsageEntry { name: "ECPU".to_string(), freq_mhz: 1181, usage: 0.33, units: 4 }],
        gpu: vec![UsageEntry { name: "GPU".to_string(), freq_mhz: 461, usage: 0.21, units: 10 }],
      },
      power: super::PowerMetrics {
        cpu: 0.2,
        gpu: 0.01,
        ram: 0.11,
        sys: 5.8,
        gpu_ram: 0.001,
        ane: 0.0,
        all: 0.321,
      },
      memory: super::MemMetrics { ram_total: 1, ram_usage: 2, swap_total: 3, swap_usage: 4 },
      temp: super::TempMetrics { cpu_temp_avg: 42.0, gpu_temp_avg: 36.0 },
    };

    let value = serde_json::to_value(&metrics).unwrap();

    assert_eq!(value["usage"]["cpu"][0]["name"], serde_json::json!("ECPU"));
    assert_eq!(value["usage"]["cpu"][0]["freq_mhz"], serde_json::json!(1181));
    assert!((value["usage"]["cpu"][0]["usage"].as_f64().unwrap() - 0.33).abs() < 1e-6);
    assert_eq!(value["usage"]["cpu"][0]["units"], serde_json::json!(4));
    assert_eq!(value["usage"]["gpu"][0]["name"], serde_json::json!("GPU"));
    assert_eq!(value["usage"]["gpu"][0]["freq_mhz"], serde_json::json!(461));
    assert!((value["usage"]["gpu"][0]["usage"].as_f64().unwrap() - 0.21).abs() < 1e-6);
    assert_eq!(value["usage"]["gpu"][0]["units"], serde_json::json!(10));
    assert!((value["power"]["cpu"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert!((value["power"]["all"].as_f64().unwrap() - (0.2 + 0.01 + 0.11 + 0.001)).abs() < 1e-6);
    assert_eq!(value["memory"]["swap_usage"], serde_json::json!(4));
    assert!((value["temp"]["cpu_temp_avg"].as_f64().unwrap() - 42.0).abs() < 1e-6);
  }

  #[test]
  fn cluster_usage_is_averaged_across_cluster_units() {
    let cores = vec![
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
    ];
    let cluster_units = vec![("PCPU".to_string(), 5), ("PCPU1".to_string(), 5)];
    let cluster_names = vec!["PCPU".to_string(), "PCPU1".to_string()];

    let usage = cluster_usage_from_cores(&cores, &cluster_names, &cluster_units);

    assert_eq!(usage.iter().find(|(name, _)| name == "PCPU").map(|(_, usage)| *usage), Some(0.2));
    assert_eq!(usage.iter().find(|(name, _)| name == "PCPU1").map(|(_, usage)| *usage), Some(0.0));
  }

  #[test]
  fn cluster_names_are_sorted_naturally() {
    let mut clusters = vec!["PCPU10".to_string(), "PCPU2".to_string(), "PCPU".to_string()];

    sort_cluster_names(&mut clusters);

    assert_eq!(clusters, vec!["PCPU".to_string(), "PCPU2".to_string(), "PCPU10".to_string()]);
  }

  #[test]
  fn distribute_units_preserves_input_order() {
    let units = distribute_units(&["ECPU".to_string(), "PCPU".to_string(), "PCPU1".to_string()], 10);

    assert_eq!(
      units,
      vec![
        ("ECPU".to_string(), 4),
        ("PCPU".to_string(), 3),
        ("PCPU1".to_string(), 3),
      ]
    );
  }

  #[test]
  fn core_samples_are_sorted_before_cluster_aggregation() {
    let mut cores = vec![(5, (1260, 0.0)), (0, (4512, 1.0)), (1, (1260, 0.0))];
    sort_indexed_samples_by_core(&mut cores);

    assert_eq!(cores.iter().map(|(idx, _)| *idx).collect::<Vec<_>>(), vec![0, 1, 5]);
  }
}
