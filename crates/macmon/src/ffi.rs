use serde::Serialize;
use std::error::Error;
use std::ffi::{CStr, c_char};
use std::fmt::{Display, Formatter};
use std::ptr;

type WithError<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Default, Clone, Serialize)]
pub struct PowerMetrics {
  pub cpu: f32,
  pub gpu: f32,
  pub ram: f32,
  pub sys: f32,
  pub gpu_ram: f32,
  pub ane: f32,
  pub all: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,
  pub ram_usage: u64,
  pub swap_total: u64,
  pub swap_usage: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct TempMetrics {
  pub cpu_temp_avg: f32,
  pub gpu_temp_avg: f32,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Metrics {
  pub usage: UsageMetrics,
  pub power: PowerMetrics,
  pub memory: MemMetrics,
  pub temp: TempMetrics,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct CpuDomainInfo {
  pub name: String,
  pub units: u32,
  pub freqs_mhz: Vec<u32>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SocInfo {
  pub mac_model: String,
  pub chip_name: String,
  pub memory_gb: u8,
  pub cpu_cores_total: u16,
  pub cpu_domains: Vec<CpuDomainInfo>,
  pub gpu_cores: u8,
  pub gpu_freqs_mhz: Vec<u32>,
}

#[repr(C)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum macmon_status_t {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1,
  MACMON_STATUS_INIT_FAILED = 2,
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4,
}

#[repr(C)]
struct macmon_sampler_t {
  _private: [u8; 0],
}

#[repr(C)]
struct macmon_usage_entry_t {
  name: *const c_char,
  freq_mhz: u32,
  usage: f32,
  units: u32,
}

#[repr(C)]
#[derive(Default)]
struct macmon_usage_list_t {
  len: usize,
  ptr: *mut macmon_usage_entry_t,
}

#[repr(C)]
#[derive(Default)]
struct macmon_power_metrics_t {
  cpu: f32,
  gpu: f32,
  ram: f32,
  sys: f32,
  gpu_ram: f32,
  ane: f32,
  all: f32,
}

#[repr(C)]
#[derive(Default)]
struct macmon_mem_metrics_t {
  ram_total: u64,
  ram_usage: u64,
  swap_total: u64,
  swap_usage: u64,
}

#[repr(C)]
#[derive(Default)]
struct macmon_temp_metrics_t {
  cpu_temp_avg: f32,
  gpu_temp_avg: f32,
}

#[repr(C)]
#[derive(Default)]
struct macmon_metrics_t {
  cpu: macmon_usage_list_t,
  gpu: macmon_usage_list_t,
  power: macmon_power_metrics_t,
  memory: macmon_mem_metrics_t,
  temp: macmon_temp_metrics_t,
}

#[repr(C)]
struct macmon_cpu_domain_t {
  name: *const c_char,
  units: u32,
  freqs_len: usize,
  freqs_mhz: *mut u32,
}

#[repr(C)]
#[derive(Default)]
struct macmon_soc_info_t {
  mac_model: *const c_char,
  chip_name: *const c_char,
  memory_gb: u8,
  cpu_cores_total: u16,
  cpu_domains_len: usize,
  cpu_domains: *mut macmon_cpu_domain_t,
  gpu_cores: u8,
  gpu_freqs_len: usize,
  gpu_freqs_mhz: *mut u32,
}

unsafe extern "C" {
  fn macmon_sampler_new(out_sampler: *mut *mut macmon_sampler_t) -> macmon_status_t;
  fn macmon_sampler_free(sampler: *mut macmon_sampler_t);

  fn macmon_sampler_get_soc_info(
    sampler: *mut macmon_sampler_t,
    out_info: *mut macmon_soc_info_t,
  ) -> macmon_status_t;
  fn macmon_soc_info_free(info: *mut macmon_soc_info_t);

  fn macmon_sampler_get_metrics(
    sampler: *mut macmon_sampler_t,
    duration_ms: u32,
    out_metrics: *mut macmon_metrics_t,
  ) -> macmon_status_t;
  fn macmon_metrics_free(metrics: *mut macmon_metrics_t);

  fn macmon_last_error_message() -> *const c_char;
}

#[derive(Debug)]
struct FfiError(String);

impl Display for FfiError {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.0)
  }
}

impl Error for FfiError {}

fn cstr_to_string(ptr: *const c_char) -> String {
  if ptr.is_null() {
    return String::new();
  }

  unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

fn last_error_message() -> String {
  unsafe { cstr_to_string(macmon_last_error_message()) }
}

fn check_status(status: macmon_status_t, fallback: &str) -> WithError<()> {
  if status == macmon_status_t::MACMON_STATUS_OK {
    return Ok(());
  }

  let message = last_error_message();
  let message = if message.is_empty() { fallback.to_string() } else { message };
  Err(Box::new(FfiError(message)))
}

fn copy_usage_list(list: &macmon_usage_list_t) -> Vec<UsageEntry> {
  if list.ptr.is_null() || list.len == 0 {
    return Vec::new();
  }

  unsafe { std::slice::from_raw_parts(list.ptr, list.len) }
    .iter()
    .map(|entry| UsageEntry {
      name: cstr_to_string(entry.name),
      freq_mhz: entry.freq_mhz,
      usage: entry.usage,
      units: entry.units,
    })
    .collect()
}

fn copy_soc_info(raw: &macmon_soc_info_t) -> SocInfo {
  let cpu_domains = if raw.cpu_domains.is_null() || raw.cpu_domains_len == 0 {
    Vec::new()
  } else {
    unsafe { std::slice::from_raw_parts(raw.cpu_domains, raw.cpu_domains_len) }
      .iter()
      .map(|domain| CpuDomainInfo {
        name: cstr_to_string(domain.name),
        units: domain.units,
        freqs_mhz: if domain.freqs_mhz.is_null() || domain.freqs_len == 0 {
          Vec::new()
        } else {
          unsafe { std::slice::from_raw_parts(domain.freqs_mhz, domain.freqs_len) }.to_vec()
        },
      })
      .collect()
  };

  let gpu_freqs_mhz = if raw.gpu_freqs_mhz.is_null() || raw.gpu_freqs_len == 0 {
    Vec::new()
  } else {
    unsafe { std::slice::from_raw_parts(raw.gpu_freqs_mhz, raw.gpu_freqs_len) }.to_vec()
  };

  SocInfo {
    mac_model: cstr_to_string(raw.mac_model),
    chip_name: cstr_to_string(raw.chip_name),
    memory_gb: raw.memory_gb,
    cpu_cores_total: raw.cpu_cores_total,
    cpu_domains,
    gpu_cores: raw.gpu_cores,
    gpu_freqs_mhz,
  }
}

fn copy_metrics(raw: &macmon_metrics_t) -> Metrics {
  Metrics {
    usage: UsageMetrics { cpu: copy_usage_list(&raw.cpu), gpu: copy_usage_list(&raw.gpu) },
    power: PowerMetrics {
      cpu: raw.power.cpu,
      gpu: raw.power.gpu,
      ram: raw.power.ram,
      sys: raw.power.sys,
      gpu_ram: raw.power.gpu_ram,
      ane: raw.power.ane,
      all: raw.power.all,
    },
    memory: MemMetrics {
      ram_total: raw.memory.ram_total,
      ram_usage: raw.memory.ram_usage,
      swap_total: raw.memory.swap_total,
      swap_usage: raw.memory.swap_usage,
    },
    temp: TempMetrics {
      cpu_temp_avg: raw.temp.cpu_temp_avg,
      gpu_temp_avg: raw.temp.gpu_temp_avg,
    },
  }
}

pub struct Sampler {
  raw: *mut macmon_sampler_t,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let mut raw = ptr::null_mut();
    check_status(
      unsafe { macmon_sampler_new(&mut raw) },
      "failed to initialize sampler",
    )?;
    Ok(Self { raw })
  }

  pub fn get_soc_info(&mut self) -> WithError<SocInfo> {
    let mut raw = macmon_soc_info_t::default();
    check_status(
      unsafe { macmon_sampler_get_soc_info(self.raw, &mut raw) },
      "failed to fetch soc info",
    )?;
    let info = copy_soc_info(&raw);
    unsafe { macmon_soc_info_free(&mut raw) };
    Ok(info)
  }

  pub fn get_metrics(&mut self, duration_ms: u32) -> WithError<Metrics> {
    let mut raw = macmon_metrics_t::default();
    check_status(
      unsafe { macmon_sampler_get_metrics(self.raw, duration_ms.max(100), &mut raw) },
      "failed to fetch metrics",
    )?;
    let metrics = copy_metrics(&raw);
    unsafe { macmon_metrics_free(&mut raw) };
    Ok(metrics)
  }
}

impl Drop for Sampler {
  fn drop(&mut self) {
    if self.raw.is_null() {
      return;
    }

    unsafe { macmon_sampler_free(self.raw) };
    self.raw = ptr::null_mut();
  }
}
