use std::sync::OnceLock;

use core_foundation::{
  base::{CFRange, CFRelease},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::CFDictionaryRef,
};
use serde::Serialize;

use crate::diag::startup_log;
use crate::platform::{IOServiceIterator, WithError, cfdict_get_val, cfio_get_props};

struct CpuDomainBinding {
  channel_prefix: &'static str,
  pmgr_key: &'static str,
}

// Apple APIs expose CPU cluster/core channels and pmgr DVFS tables as separate islands
// of data. This table is the manual bridge between them: it tells the library which
// channel prefixes belong to the same CPU domain and which pmgr key provides that
// domain's frequency table.
const CPU_DOMAIN_BINDINGS: [CpuDomainBinding; 3] = [
  CpuDomainBinding { channel_prefix: "ECPU", pmgr_key: "voltage-states1-sram" },
  CpuDomainBinding { channel_prefix: "PCPU", pmgr_key: "voltage-states5-sram" },
  CpuDomainBinding { channel_prefix: "MCPU", pmgr_key: "voltage-states1-sram" },
];

static SOC_INFO_CACHE: OnceLock<SocInfo> = OnceLock::new();

#[derive(Debug, Default, Clone, Serialize)]
pub struct CpuDomainInfo {
  /// Stable public name for this CPU domain or cluster, for example `ECPU` or `PCPU`.
  pub name: String,
  /// Number of CPU units (cores) that belong to this domain.
  pub units: u32,
  /// Available DVFS operating points for this domain in MHz, in the order reported by pmgr.
  /// This is not a `{base, max}` pair: it is the full frequency table used to interpret
  /// residency counters and derive the current estimated frequency.
  pub freqs_mhz: Vec<u32>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SocInfo {
  /// Machine model identifier reported by macOS, for example `Mac15,6`.
  pub mac_model: String,
  /// Marketing chip name reported by macOS, for example `Apple M3 Pro`.
  pub chip_name: String,
  /// Installed unified memory capacity in gigabytes.
  pub memory_gb: u8,
  /// CPU frequency domains discovered for this SoC.
  pub cpu_domains: Vec<CpuDomainInfo>,
  /// GPU core count reported by macOS.
  pub gpu_cores: u8,
  /// Available GPU DVFS operating points in MHz, in the order reported by pmgr.
  pub gpu_freqs_mhz: Vec<u32>,
}

pub fn get_dvfs_mhz(dict: CFDictionaryRef, key: &str, scale: u32) -> (Vec<u32>, Vec<u32>) {
  unsafe {
    let Some(obj) = cfdict_get_val(dict, key) else {
      return (Vec::new(), Vec::new());
    };
    let obj = obj as CFDataRef;
    let obj_len = CFDataGetLength(obj);
    let obj_val = vec![0u8; obj_len as usize];
    CFDataGetBytes(obj, CFRange::init(0, obj_len), obj_val.as_ptr() as *mut u8);

    let items_count = (obj_len / 8) as usize;
    let [mut freqs, mut volts] = [vec![0u32; items_count], vec![0u32; items_count]];
    for (i, x) in obj_val.chunks_exact(8).enumerate() {
      volts[i] = u32::from_le_bytes([x[4], x[5], x[6], x[7]]);
      freqs[i] = u32::from_le_bytes([x[0], x[1], x[2], x[3]]) / scale;
    }

    (volts, freqs)
  }
}

pub fn run_system_profiler() -> WithError<serde_json::Value> {
  let out = std::process::Command::new("system_profiler")
    .args(["SPHardwareDataType", "SPDisplaysDataType", "-json"])
    .output()?;

  let out = std::str::from_utf8(&out.stdout)?;
  let out = serde_json::from_str::<serde_json::Value>(out)?;
  Ok(out)
}

fn parse_cpu_domain_units(number_processors: Option<&str>) -> Vec<u32> {
  let parts = number_processors
    .and_then(|value| value.strip_prefix("proc "))
    .unwrap_or("")
    .split(':')
    .map(|part| part.parse::<u32>().unwrap_or(0))
    .collect::<Vec<_>>();

  match parts.as_slice() {
    [_, first_domain_units, second_domain_units] => vec![*second_domain_units, *first_domain_units],
    _ => Vec::new(),
  }
}

fn init_cpu_freq_domains(units: Vec<u32>) -> Vec<CpuDomainInfo> {
  CPU_DOMAIN_BINDINGS
    .iter()
    .enumerate()
    .map(|(idx, binding)| CpuDomainInfo {
      name: binding.channel_prefix.to_string(),
      units: units.get(idx).copied().unwrap_or(0),
      freqs_mhz: Vec::new(),
    })
    .collect()
}

fn cpu_freq_tables(item: CFDictionaryRef, scale: u32) -> Vec<Vec<u32>> {
  CPU_DOMAIN_BINDINGS.iter().map(|binding| get_dvfs_mhz(item, binding.pmgr_key, scale).1).collect()
}

fn finalize_cpu_freq_domains(domains: &mut Vec<CpuDomainInfo>) {
  let unit_indices = domains
    .iter()
    .enumerate()
    .filter(|(_, domain)| domain.units > 0)
    .map(|(idx, _)| idx)
    .collect::<Vec<_>>();
  let freq_indices = domains
    .iter()
    .enumerate()
    .filter(|(_, domain)| !domain.freqs_mhz.is_empty())
    .map(|(idx, _)| idx)
    .collect::<Vec<_>>();

  if unit_indices.len() == 1 && freq_indices.len() == 1 && unit_indices[0] != freq_indices[0] {
    let freqs_mhz = std::mem::take(&mut domains[freq_indices[0]].freqs_mhz);
    domains[unit_indices[0]].freqs_mhz = freqs_mhz;
  }

  // Keep only domains that have an actual core count. pmgr may expose extra DVFS
  // tables for bindings that are not present on the current SoC.
  domains.retain(|domain| domain.units > 0);
}

fn load_soc_info() -> WithError<SocInfo> {
  let mut info = SocInfo::default();
  let out = run_system_profiler()?;
  startup_log("lib soc: system_profiler complete");

  let chip_name =
    out["SPHardwareDataType"][0]["chip_type"].as_str().unwrap_or("Unknown chip").to_string();
  let mac_model =
    out["SPHardwareDataType"][0]["machine_model"].as_str().unwrap_or("Unknown model").to_string();
  let mem_gb = out["SPHardwareDataType"][0]["physical_memory"]
    .as_str()
    .and_then(|mem| mem.strip_suffix(" GB"))
    .unwrap_or("0")
    .parse::<u64>()
    .unwrap_or(0);
  let cpu_domain_units =
    parse_cpu_domain_units(out["SPHardwareDataType"][0]["number_processors"].as_str());
  let gpu_cores =
    out["SPDisplaysDataType"][0]["sppci_cores"].as_str().unwrap_or("0").parse::<u64>().unwrap_or(0);

  let before_m4 = chip_name.contains("M1") || chip_name.contains("M2") || chip_name.contains("M3");
  let cpu_scale: u32 = if before_m4 { 1000 * 1000 } else { 1000 };
  let gpu_scale: u32 = 1000 * 1000;

  let mut cpu_freq_domains = init_cpu_freq_domains(cpu_domain_units);

  info.chip_name = chip_name;
  info.mac_model = mac_model;
  info.memory_gb = mem_gb as u8;
  info.gpu_cores = gpu_cores as u8;

  for (entry, name) in IOServiceIterator::new("AppleARMIODevice")? {
    if name == "pmgr" {
      let item = cfio_get_props(entry, name)?;
      for (domain, freqs) in cpu_freq_domains.iter_mut().zip(cpu_freq_tables(item, cpu_scale)) {
        domain.freqs_mhz = freqs;
      }
      info.gpu_freqs_mhz = get_dvfs_mhz(item, "voltage-states9", gpu_scale).1;
      unsafe { CFRelease(item as _) }
    }
  }

  finalize_cpu_freq_domains(&mut cpu_freq_domains);
  info.cpu_domains = cpu_freq_domains;

  if !info.cpu_domains.iter().any(|domain| !domain.freqs_mhz.is_empty()) {
    return Err("No CPU frequencies found".into());
  }

  Ok(info)
}

pub fn get_soc_info() -> WithError<SocInfo> {
  if let Some(info) = SOC_INFO_CACHE.get() {
    startup_log("lib soc: cache hit");
    return Ok(info.clone());
  }

  startup_log("lib soc: cache miss");
  let info = load_soc_info()?;
  let _ = SOC_INFO_CACHE.set(info.clone());
  Ok(info)
}

#[cfg(test)]
#[path = "sources_test.rs"]
mod tests;
