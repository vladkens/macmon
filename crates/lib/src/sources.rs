use core_foundation::{
  base::{CFRange, CFRelease},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::CFDictionaryRef,
};
use serde::Serialize;

use crate::platform::{IOServiceIterator, WithError, cfdict_get_val, cfio_get_props};

#[derive(Debug, Default, Clone, Serialize)]
pub struct CpuDomainInfo {
  pub id: String,
  pub units: u32,
  pub freqs: Vec<u32>,
  pub channel_prefix: String,
  pub core_prefix: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SocInfo {
  pub mac_model: String,
  pub chip_name: String,
  pub memory_gb: u8,
  pub cpu_cores_total: u16,
  pub cpu_domains: Vec<CpuDomainInfo>,
  pub gpu_cores: u8,
  pub gpu_freqs: Vec<u32>,
}

impl SocInfo {
  pub fn new() -> WithError<Self> {
    get_soc_info()
  }
}

pub fn get_dvfs_mhz(dict: CFDictionaryRef, key: &str) -> (Vec<u32>, Vec<u32>) {
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
      freqs[i] = u32::from_le_bytes([x[0], x[1], x[2], x[3]]);
    }

    (volts, freqs)
  }
}

pub fn run_system_profiler() -> WithError<serde_json::Value> {
  let out = std::process::Command::new("system_profiler")
    .args(["SPHardwareDataType", "SPDisplaysDataType", "SPSoftwareDataType", "-json"])
    .output()?;

  let out = std::str::from_utf8(&out.stdout)?;
  let out = serde_json::from_str::<serde_json::Value>(out)?;
  Ok(out)
}

fn to_mhz(vals: Vec<u32>, scale: u32) -> Vec<u32> {
  vals.iter().map(|x| *x / scale).collect()
}

struct CpuDomainBinding {
  channel_prefix: &'static str,
  core_prefix: &'static str,
  pmgr_key: &'static str,
}

const CPU_DOMAIN_BINDINGS: [CpuDomainBinding; 2] = [
  CpuDomainBinding {
    channel_prefix: "ECPU",
    core_prefix: "ECPU",
    pmgr_key: "voltage-states1-sram",
  },
  CpuDomainBinding {
    channel_prefix: "PCPU",
    core_prefix: "PCPU",
    pmgr_key: "voltage-states5-sram",
  },
];

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
      id: format!("cpu{idx}"),
      units: units.get(idx).copied().unwrap_or(0),
      freqs: Vec::new(),
      channel_prefix: binding.channel_prefix.to_string(),
      core_prefix: binding.core_prefix.to_string(),
    })
    .collect()
}

fn cpu_freq_tables(item: CFDictionaryRef, scale: u32) -> Vec<Vec<u32>> {
  CPU_DOMAIN_BINDINGS
    .iter()
    .map(|binding| to_mhz(get_dvfs_mhz(item, binding.pmgr_key).1, scale))
    .collect()
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
    .filter(|(_, domain)| !domain.freqs.is_empty())
    .map(|(idx, _)| idx)
    .collect::<Vec<_>>();

  if unit_indices.len() == 1 && freq_indices.len() == 1 && unit_indices[0] != freq_indices[0] {
    let freqs = std::mem::take(&mut domains[freq_indices[0]].freqs);
    domains[unit_indices[0]].freqs = freqs;
  }

  domains.retain(|domain| domain.units > 0 || !domain.freqs.is_empty());
  for (idx, domain) in domains.iter_mut().enumerate() {
    domain.id = format!("cpu{idx}");
  }
}

pub fn get_soc_info() -> WithError<SocInfo> {
  let out = run_system_profiler()?;
  let mut info = SocInfo::default();

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
        domain.freqs = freqs;
      }
      info.gpu_freqs = to_mhz(get_dvfs_mhz(item, "voltage-states9").1, gpu_scale);
      unsafe { CFRelease(item as _) }
    }
  }

  finalize_cpu_freq_domains(&mut cpu_freq_domains);
  info.cpu_domains = cpu_freq_domains;
  info.cpu_cores_total = info.cpu_domains.iter().map(|domain| domain.units).sum::<u32>() as u16;

  if !info.cpu_domains.iter().any(|domain| !domain.freqs.is_empty()) {
    return Err("No CPU frequencies found".into());
  }

  Ok(info)
}

#[cfg(test)]
mod tests {
  use super::{CpuDomainInfo, finalize_cpu_freq_domains, init_cpu_freq_domains, parse_cpu_domain_units};
  use crate::platform::smc::KeyInfo;

  fn cached_key_info(cache: &[(u32, KeyInfo)], key: u32) -> Option<KeyInfo> {
    cache.iter().find(|(cached_key, _)| *cached_key == key).map(|(_, info)| *info)
  }

  #[test]
  fn parse_cpu_domain_units_returns_generic_domain_order() {
    assert_eq!(parse_cpu_domain_units(Some("proc 0:8:4")), vec![4, 8]);
    assert_eq!(parse_cpu_domain_units(Some("invalid")), Vec::<u32>::new());
  }

  #[test]
  fn init_cpu_freq_domains_uses_binding_slots() {
    let domains = init_cpu_freq_domains(vec![4, 8]);

    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].id, "cpu0");
    assert_eq!(domains[0].units, 4);
    assert_eq!(domains[0].channel_prefix, "ECPU");
    assert_eq!(domains[1].id, "cpu1");
    assert_eq!(domains[1].units, 8);
    assert_eq!(domains[1].channel_prefix, "PCPU");
  }

  #[test]
  fn finalize_cpu_freq_domains_assigns_stable_ids_after_filtering() {
    let mut domains = vec![
      CpuDomainInfo {
        units: 4,
        freqs: vec![1000, 2000],
        id: "stale0".to_string(),
        channel_prefix: "CPUCL0".to_string(),
        core_prefix: "CPUCORE0".to_string(),
      },
      CpuDomainInfo {
        units: 8,
        freqs: vec![2000, 3000],
        id: "stale1".to_string(),
        channel_prefix: "CPUCL1".to_string(),
        core_prefix: "CPUCORE1".to_string(),
      },
      CpuDomainInfo {
        units: 0,
        freqs: vec![],
        id: "stale2".to_string(),
        channel_prefix: "CPUCL2".to_string(),
        core_prefix: "CPUCORE2".to_string(),
      },
    ];

    finalize_cpu_freq_domains(&mut domains);

    assert_eq!(domains.len(), 2);
    assert_eq!(domains[0].id, "cpu0");
    assert_eq!(domains[0].units, 4);
    assert_eq!(domains[0].channel_prefix, "CPUCL0");
    assert_eq!(domains[1].id, "cpu1");
    assert_eq!(domains[1].units, 8);
    assert_eq!(domains[1].channel_prefix, "CPUCL1");
  }

  #[test]
  fn finalize_cpu_freq_domains_moves_single_freq_table_to_single_domain_with_units() {
    let mut domains = vec![
      CpuDomainInfo {
        units: 0,
        freqs: vec![1000, 2000],
        id: "stale0".to_string(),
        channel_prefix: "CPUCL0".to_string(),
        core_prefix: "CPUCORE0".to_string(),
      },
      CpuDomainInfo {
        units: 10,
        freqs: vec![],
        id: "stale1".to_string(),
        channel_prefix: "CPUCL1".to_string(),
        core_prefix: "CPUCORE1".to_string(),
      },
    ];

    finalize_cpu_freq_domains(&mut domains);

    assert_eq!(domains.len(), 1);
    assert_eq!(domains[0].id, "cpu0");
    assert_eq!(domains[0].units, 10);
    assert_eq!(domains[0].freqs, vec![1000, 2000]);
    assert_eq!(domains[0].channel_prefix, "CPUCL1");
  }

  #[test]
  fn smc_vec_cache_returns_inserted_key_info() {
    let key = u32::from_be_bytes(*b"TEST");
    let info = KeyInfo { data_size: 4, data_type: 0x666c7420, data_attributes: 0 };
    let mut cache = Vec::new();

    assert_eq!(cached_key_info(&cache, key), None);

    cache.push((key, info));

    assert_eq!(cached_key_info(&cache, key), Some(info));
    assert_eq!(cached_key_info(&cache, key), Some(info));
  }
}
