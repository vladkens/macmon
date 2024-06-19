use crate::ioreport::*;
use core_foundation::{
  base::{CFRange, CFRelease},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::CFDictionaryRef,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const CPU_POWER_SUBG: &str = "CPU Complex Performance States";
const GPU_POWER_SUBG: &str = "GPU Performance States";

// MARK: Memory

#[derive(Debug, Default)]
pub struct MemoryMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

fn libc_ram_info() -> WithError<(u64, u64)> {
  let (mut usage, mut total) = (0u64, 0u64);

  unsafe {
    let mut name = [libc::CTL_HW, libc::HW_MEMSIZE];
    let mut size = std::mem::size_of::<u64>();
    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut total as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get total memory".into());
    }
  }

  unsafe {
    let mut count: u32 = libc::HOST_VM_INFO64_COUNT as _;
    let mut stats = std::mem::zeroed::<libc::vm_statistics64>();

    let ret_code = libc::host_statistics64(
      libc::mach_host_self(),
      libc::HOST_VM_INFO64,
      &mut stats as *mut _ as *mut _,
      &mut count,
    );

    if ret_code != 0 {
      return Err("Failed to get memory stats".into());
    }

    let page_size_kb = libc::sysconf(libc::_SC_PAGESIZE) as u64;

    usage = (0
      + stats.active_count as u64
      + stats.inactive_count as u64
      + stats.wire_count as u64
      + stats.speculative_count as u64
      + stats.compressor_page_count as u64
      - stats.purgeable_count as u64
      - stats.external_page_count as u64
      + 0)
      * page_size_kb;
  }

  Ok((usage, total))
}

fn libc_swap_info() -> WithError<(u64, u64)> {
  let (mut usage, mut total) = (0u64, 0u64);

  unsafe {
    let mut name = [libc::CTL_VM, libc::VM_SWAPUSAGE];
    let mut size = std::mem::size_of::<libc::xsw_usage>();
    let mut xsw: libc::xsw_usage = std::mem::zeroed::<libc::xsw_usage>();

    let ret_code = libc::sysctl(
      name.as_mut_ptr(),
      name.len() as _,
      &mut xsw as *mut _ as *mut _,
      &mut size,
      std::ptr::null_mut(),
      0,
    );

    if ret_code != 0 {
      return Err("Failed to get swap usage".into());
    }

    usage = xsw.xsu_used;
    total = xsw.xsu_total;
  }

  Ok((usage, total))
}

// MARK: CPU

// dynamic voltage and frequency scaling
fn get_dvfs_mhz(dict: CFDictionaryRef, key: &str) -> (Vec<u32>, Vec<u32>) {
  unsafe {
    let obj = cfdict_get_val(dict, key).unwrap() as CFDataRef;
    let obj_len = CFDataGetLength(obj);
    let obj_val = vec![0u8; obj_len as usize];
    CFDataGetBytes(obj, CFRange::init(0, obj_len), obj_val.as_ptr() as *mut u8);

    // obj_val is pairs of (freq, voltage) 4 bytes each
    let items_count = (obj_len / 8) as usize;
    let [mut freqs, mut volts] = [vec![0u32; items_count], vec![0u32; items_count]];
    for (i, x) in obj_val.chunks_exact(8).enumerate() {
      volts[i] = u32::from_le_bytes([x[4], x[5], x[6], x[7]]);
      freqs[i] = u32::from_le_bytes([x[0], x[1], x[2], x[3]]);
      freqs[i] = freqs[i] / 1000 / 1000; // as MHz
    }

    (volts, freqs)
  }
}

fn calc_percent(a: f64, b: f64) -> f64 {
  match b {
    0.0 => 0.0,
    _ => (a / b) as f64,
  }
}

fn calc_freq(item: CFDictionaryRef, freqs: &Vec<u32>) -> (u32, f32) {
  let residencies = cfio_get_residencies(item);
  let (len1, len2) = (residencies.len(), freqs.len());
  assert!(len1 > len2, "cacl_freq invalid data: {} vs {}", len1, len2); // todo?

  // first is IDLE for CPU and OFF for GPU
  let usage = residencies.iter().map(|x| x.1 as f64).skip(1).sum::<f64>();
  let total = residencies.iter().map(|x| x.1 as f64).sum::<f64>();
  let count = freqs.len();

  let mut freq = 0f64;
  for i in 0..count {
    let percent = calc_percent(residencies[i + 1].1 as _, usage);
    freq += percent * freqs[i] as f64;
  }

  let percent = calc_percent(usage, total);
  let max_freq = freqs.last().unwrap().clone() as f64;
  let from_max = (freq * percent) / max_freq;

  (freq as u32, from_max as f32)
}

// MARK: SocInfo

#[derive(Debug, Default, Clone)]
pub struct SocInfo {
  pub chip_name: String,
  pub memory_gb: u8,
  pub ecpu_cores: u8,
  pub pcpu_cores: u8,
  pub gpu_cores: u8,
  pub ecpu_freqs: Vec<u32>,
  pub pcpu_freqs: Vec<u32>,
  pub gpu_freqs: Vec<u32>,
}

pub fn get_soc_info() -> WithError<SocInfo> {
  let mut info = SocInfo::default();

  // system_profiler -listDataTypes
  let out = std::process::Command::new("system_profiler")
    .args(&["SPHardwareDataType", "SPDisplaysDataType", "-json"])
    .output()
    .unwrap();

  let out = std::str::from_utf8(&out.stdout).unwrap();
  let out = serde_json::from_str::<serde_json::Value>(out).unwrap();

  // SPHardwareDataType.0.chip_type
  let chip_name = out["SPHardwareDataType"][0]["chip_type"].as_str().unwrap().to_string();

  // SPHardwareDataType.0.physical_memory -> "x GB"
  let mem_gb = out["SPHardwareDataType"][0]["physical_memory"].as_str();
  let mem_gb = mem_gb.expect("No memory found").strip_suffix(" GB").unwrap();
  let mem_gb = mem_gb.parse::<u64>().unwrap();

  // SPHardwareDataType.0.number_processors -> "proc x:y:z"
  let cpu_cores = out["SPHardwareDataType"][0]["number_processors"].as_str();
  let cpu_cores = cpu_cores.expect("No CPU cores found").strip_prefix("proc ").unwrap();
  let cpu_cores = cpu_cores.split(':').map(|x| x.parse::<u64>().unwrap()).collect::<Vec<_>>();
  assert_eq!(cpu_cores.len(), 3, "Invalid number of CPU cores");
  let (ecpu_cores, pcpu_cores, _) = (cpu_cores[2], cpu_cores[1], cpu_cores[0]);

  let gpu_cores = match out["SPDisplaysDataType"][0]["sppci_cores"].as_str() {
    Some(x) => x.parse::<u64>().unwrap(),
    None => 0,
  };

  info.chip_name = chip_name;
  info.memory_gb = mem_gb as u8;
  info.gpu_cores = gpu_cores as u8;
  info.ecpu_cores = ecpu_cores as u8;
  info.pcpu_cores = pcpu_cores as u8;

  // cpu frequencies
  for (entry, name) in IOServiceIterator::new("AppleARMIODevice")? {
    if name == "pmgr" {
      let item = cfio_get_props(entry, name)?;
      info.ecpu_freqs = get_dvfs_mhz(item, "voltage-states1-sram").1;
      info.pcpu_freqs = get_dvfs_mhz(item, "voltage-states5-sram").1;
      info.gpu_freqs = get_dvfs_mhz(item, "voltage-states9").1;
      unsafe { CFRelease(item as _) }
    }
  }

  if info.ecpu_freqs.len() == 0 || info.pcpu_freqs.len() == 0 {
    return Err("No CPU cores found".into());
  }

  Ok(info)
}

// MARK: Metrics

#[derive(Debug, Default)]
pub struct Metrics {
  pub ecpu_usage: (u32, f32), // freq, percent_from_max
  pub pcpu_usage: (u32, f32), // freq, percent_from_max
  pub gpu_usage: (u32, f32),  // freq, percent_from_max
  pub cpu_power: f32,         // Watts
  pub gpu_power: f32,         // Watts
  pub ane_power: f32,         // Watts
  pub all_power: f32,         // Watts
  pub memory: MemoryMetrics,
}

pub struct Sampler {
  info: SocInfo,
  sampler: IOReportSampler,
}

impl Sampler {
  pub fn new(info: SocInfo, channels: Vec<(&str, Option<&str>)>) -> WithError<Self> {
    let iosampler = IOReportSampler::new(channels)?;
    Ok(Self { info, sampler: iosampler })
  }

  pub fn get_metrics(&mut self, duration: u64) -> WithError<Metrics> {
    let mut rs = Metrics::default();

    let (ram_usage, ram_total) = libc_ram_info()?;
    let (swap_usage, swap_total) = libc_swap_info()?;
    rs.memory = MemoryMetrics { ram_total, ram_usage, swap_total, swap_usage };

    for x in self.sampler.sample(duration) {
      if x.group == "CPU Stats" && x.subgroup == CPU_POWER_SUBG {
        match x.channel.as_str() {
          "ECPU" => rs.ecpu_usage = calc_freq(x.item, &self.info.ecpu_freqs),
          "PCPU" => rs.pcpu_usage = calc_freq(x.item, &self.info.pcpu_freqs),
          _ => {}
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_POWER_SUBG {
        match x.channel.as_str() {
          "GPUPH" => rs.gpu_usage = calc_freq(x.item, &self.info.gpu_freqs[1..].to_vec()),
          _ => {}
        }
      }

      if x.group == "Energy Model" {
        match x.channel.as_str() {
          "CPU Energy" => rs.cpu_power += get_watts(x.item, &x.unit, duration)?,
          "GPU Energy" => rs.gpu_power += get_watts(x.item, &x.unit, duration)?,
          c if c.starts_with("ANE") => rs.ane_power += get_watts(x.item, &x.unit, duration)?,
          _ => {}
        }
      }
    }

    rs.all_power = rs.cpu_power + rs.gpu_power + rs.ane_power;

    Ok(rs)
  }
}

pub fn get_metrics_sampler(info: SocInfo) -> WithError<Sampler> {
  let channels = vec![
    ("Energy Model", None),              // cpu+gpu+ane power
    ("CPU Stats", Some(CPU_POWER_SUBG)), // cpu freq by cluster
    ("GPU Stats", Some(GPU_POWER_SUBG)), // gpu freq
  ];

  Sampler::new(info, channels)
}
