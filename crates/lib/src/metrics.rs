use std::collections::HashMap;
use std::thread::JoinHandle;

use core_foundation::dictionary::CFDictionaryRef;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

use crate::diag::startup_log;
use crate::platform::{IOReport, SMC, cfio_collect_residencies, libc_ram, libc_swap};
use crate::sources::{SocInfo, get_soc_info};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

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

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CoreUsageEntry {
  pub freq_mhz: u32,
  pub usage: f32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CpuUsageEntry {
  pub name: String,
  pub freq_mhz: u32,
  pub usage: f32,
  pub cores: Vec<CoreUsageEntry>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct GpuUsageEntry {
  pub name: String,
  pub freq_mhz: u32,
  pub usage: f32,
  pub units: u32,
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

#[derive(Debug, Default)]
pub struct Metrics {
  pub cpu_usage: Vec<CpuUsageEntry>,
  pub gpu_usage: Vec<GpuUsageEntry>,
  pub power: PowerMetrics,
  pub memory: MemMetrics,
  pub temp: TempMetrics,
}

#[derive(Serialize)]
struct CpuUsageValue<'a> {
  units: u32,
  freq_mhz: u32,
  usage: f32,
  cores: CorePairs<'a>,
}

#[derive(Serialize)]
struct GpuUsageValue {
  units: u32,
  freq_mhz: u32,
  usage: f32,
}

struct CorePairs<'a>(&'a [CoreUsageEntry]);

impl Serialize for CorePairs<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
    for core in self.0 {
      seq.serialize_element(&(core.freq_mhz, core.usage))?;
    }
    seq.end()
  }
}

struct CpuUsageMap<'a>(&'a [CpuUsageEntry]);

impl Serialize for CpuUsageMap<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(self.0.len()))?;
    for entry in self.0 {
      map.serialize_entry(
        &entry.name,
        &CpuUsageValue {
          units: entry.cores.len() as u32,
          freq_mhz: entry.freq_mhz,
          usage: entry.usage,
          cores: CorePairs(&entry.cores),
        },
      )?;
    }
    map.end()
  }
}

struct GpuUsageMap<'a>(&'a [GpuUsageEntry]);

impl Serialize for GpuUsageMap<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(self.0.len()))?;
    for entry in self.0 {
      map.serialize_entry(
        &entry.name,
        &GpuUsageValue { units: entry.units, freq_mhz: entry.freq_mhz, usage: entry.usage },
      )?;
    }
    map.end()
  }
}

impl Serialize for Metrics {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(5))?;
    map.serialize_entry("cpu_usage", &CpuUsageMap(&self.cpu_usage))?;
    map.serialize_entry("gpu_usage", &GpuUsageMap(&self.gpu_usage))?;
    map.serialize_entry("power", &self.power)?;
    map.serialize_entry("memory", &self.memory)?;
    map.serialize_entry("temp", &self.temp)?;
    map.end()
  }
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
  if min_freq == 0.0 {
    return (0, 0.0);
  }

  let offset = items.iter().position(|x| is_active_state(x.0.as_str())).unwrap_or(items.len());
  let usage = items[offset..].iter().map(|x| x.1.max(0) as f64).sum::<f64>();
  let total = items.iter().map(|x| x.1.max(0) as f64).sum::<f64>();
  if usage == 0.0 || total == 0.0 {
    return (min_freq as u32, 0.0);
  }

  assert!(items.len() > freqs.len(), "calc_freq invalid data: {} vs {}", items.len(), freqs.len());

  let mut avg_freq = 0f64;
  for i in 0..freqs.len() {
    let residency = items.get(i + offset).map(|(_, val)| (*val).max(0) as f64).unwrap_or(0.0);
    let percent = zero_div(residency, usage);
    avg_freq += percent * freqs[i] as f64;
  }

  let usage_ratio = zero_div(usage, total);
  (avg_freq.max(min_freq) as u32, usage_ratio as f32)
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

fn calc_core_freq_avg(items: &[CoreUsageEntry], min_freq: u32) -> (u32, f32) {
  let items = items.iter().map(|x| (x.freq_mhz, x.usage)).collect::<Vec<_>>();
  calc_freq_avg(&items, min_freq)
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

fn avg_in_range(values: &[f32], min: f32, max: f32) -> f32 {
  let mut sum = 0.0_f32;
  let mut count = 0_usize;

  for &value in values {
    if value >= min && value <= max {
      sum += value;
      count += 1;
    }
  }

  zero_div(sum, count as f32)
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

type SmcInitResult = Result<(SMC, SmcSensors), String>;

// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  io_report: IOReport,
  smc_init: Option<JoinHandle<SmcInitResult>>,
  smc: Option<SMC>,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let smc_init = std::thread::spawn(|| init_smc().map_err(|err| err.to_string()));
    startup_log("lib sampler: SMC init started in background");

    let soc = match get_soc_info() {
      Ok(soc) => soc,
      Err(err) => {
        let _ = smc_init.join();
        return Err(err);
      }
    };
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
        return subgroup == CPU_FREQ_CORE_SUBG;
      }
      group == "GPU Stats" && subgroup == GPU_FREQ_DICE_SUBG
    };
    let io_report = match IOReport::new(Some(channels)) {
      Ok(io_report) => io_report,
      Err(err) => {
        let _ = smc_init.join();
        return Err(err);
      }
    };
    startup_log("lib sampler: IOReport subscription ready");

    Ok(Sampler {
      soc,
      io_report,
      smc_init: Some(smc_init),
      smc: None,
      smc_cpu_keys: Vec::new(),
      smc_gpu_keys: Vec::new(),
    })
  }

  fn gpu_freqs(&self) -> &[u32] {
    match self.soc.gpu_freqs_mhz.len() > 1 {
      true => &self.soc.gpu_freqs_mhz[1..],
      false => &self.soc.gpu_freqs_mhz,
    }
  }

  fn wait_for_smc(&mut self) -> WithError<()> {
    if self.smc.is_some() {
      return Ok(());
    }

    let Some(smc_init) = self.smc_init.take() else {
      return Err("SMC initialization state is inconsistent".into());
    };

    startup_log("lib sampler: waiting for SMC initialization");
    let (smc, smc_sensors) = match smc_init.join() {
      Ok(Ok(result)) => result,
      Ok(Err(err)) => return Err(err.into()),
      Err(_) => return Err("SMC initialization thread panicked".into()),
    };

    startup_log(format!(
      "lib sampler: SMC ready (cpu_sensors={}, gpu_sensors={})",
      smc_sensors.cpu_keys.len(),
      smc_sensors.gpu_keys.len()
    ));

    self.smc = Some(smc);
    self.smc_cpu_keys = smc_sensors.cpu_keys;
    self.smc_gpu_keys = smc_sensors.gpu_keys;

    Ok(())
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let Some(smc) = self.smc.as_mut() else {
      return Err("SMC is not initialized".into());
    };

    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = match smc.read_float_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      if val != 0.0 {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = match smc.read_float_val(sensor) {
        Ok(val) => val,
        Err(_) => continue,
      };
      if val != 0.0 {
        gpu_metrics.push(val);
      }
    }

    let cpu_avg = avg_in_range(&cpu_metrics, 15.0, 150.0);
    let gpu_avg = avg_in_range(&gpu_metrics, 15.0, 150.0);

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
    let mut cpu_domain_cores = vec![Vec::new(); cpu_domains.len()];
    let mut gpu_clusters: HashMap<String, Vec<(u32, f32)>> = HashMap::new();
    let mut rs = Metrics::default();
    let mut cpu_usage = Vec::new();
    let mut gpu_usage = Vec::new();

    self.wait_for_smc()?;

    for x in self.io_report.next_sample() {
      if x.group == "CPU Stats"
        && x.subgroup == CPU_FREQ_CORE_SUBG
        && let Some(domain_idx) =
          cpu_domains.iter().position(|domain| x.channel.contains(domain.name.as_str()))
      {
        let domain = &cpu_domains[domain_idx];
        let (freq_mhz, usage) = calc_freq(x.channel_item, &domain.freqs_mhz);
        cpu_domain_cores[domain_idx].push(CoreUsageEntry { freq_mhz, usage });
        continue;
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        gpu_clusters
          .entry(x.channel.clone())
          .or_default()
          .push(calc_freq(x.channel_item, &gpu_freqs));
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

    if !gpu_clusters.is_empty() {
      let names = gpu_clusters.keys().cloned().collect::<Vec<_>>();
      for name in &names {
        let Some(items) = gpu_clusters.get(name) else { continue };
        let (freq, usage) = calc_freq_avg(items, *gpu_freqs.first().unwrap_or(&0));
        gpu_usage.push(GpuUsageEntry { name: name.clone(), freq_mhz: freq, usage, units: 0 });
      }
      for (name, units) in distribute_units(&names, self.soc.gpu_cores as u32) {
        if let Some(entry) = gpu_usage.iter_mut().find(|entry| entry.name == name) {
          entry.units = units;
        }
      }
    }

    for (domain_idx, domain) in cpu_domains.iter().enumerate() {
      let cores = &cpu_domain_cores[domain_idx];
      if cores.is_empty() {
        continue;
      }

      let (freq, usage) = calc_core_freq_avg(cores, *domain.freqs_mhz.first().unwrap_or(&0));
      cpu_usage.push(CpuUsageEntry {
        name: domain.name.clone(),
        freq_mhz: freq,
        usage,
        cores: cores.clone(),
      });
    }

    rs.cpu_usage = cpu_usage;
    rs.gpu_usage = gpu_usage;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp_smc()?;

    let Some(smc) = self.smc.as_mut() else {
      return Err("SMC is not initialized".into());
    };

    rs.power.package = rs.power.cpu + rs.power.gpu + rs.power.ane + rs.power.ram + rs.power.gpu_ram;
    rs.power.board = smc.read_float_val("PSTR").unwrap_or(0.0);
    rs.power.battery = smc.read_float_val("PPBR").unwrap_or(0.0);
    rs.power.dc_in = smc.read_float_val("PDTR").unwrap_or(0.0);

    Ok(rs)
  }
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
