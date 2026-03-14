use crate::metrics::{Metrics, Sampler, UsageEntry};
use crate::sources::SocInfo;
use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

thread_local! {
  static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum macmon_status_t {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1,
  MACMON_STATUS_INIT_FAILED = 2,
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4,
}

#[repr(C)]
pub struct macmon_sampler_t {
  _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_usage_entry_t {
  pub name: *const c_char,
  pub freq_mhz: u32,
  pub usage: f32,
  pub units: u32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_usage_list_t {
  pub len: usize,
  pub ptr: *mut macmon_usage_entry_t,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_power_metrics_t {
  pub cpu: f32,
  pub gpu: f32,
  pub ram: f32,
  pub sys: f32,
  pub gpu_ram: f32,
  pub ane: f32,
  pub all: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_mem_metrics_t {
  pub ram_total: u64,
  pub ram_usage: u64,
  pub swap_total: u64,
  pub swap_usage: u64,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_temp_metrics_t {
  pub cpu_temp_avg: f32,
  pub gpu_temp_avg: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_metrics_t {
  pub cpu: macmon_usage_list_t,
  pub gpu: macmon_usage_list_t,
  pub power: macmon_power_metrics_t,
  pub memory: macmon_mem_metrics_t,
  pub temp: macmon_temp_metrics_t,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_cpu_domain_t {
  pub name: *const c_char,
  pub units: u32,
  pub freqs_len: usize,
  pub freqs_mhz: *mut u32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_soc_info_t {
  pub mac_model: *const c_char,
  pub chip_name: *const c_char,
  pub memory_gb: u8,
  pub cpu_cores_total: u16,
  pub cpu_domains_len: usize,
  pub cpu_domains: *mut macmon_cpu_domain_t,
  pub gpu_cores: u8,
  pub gpu_freqs_len: usize,
  pub gpu_freqs_mhz: *mut u32,
}

#[derive(Debug)]
struct FfiError {
  status: macmon_status_t,
  message: String,
}

type FfiResult<T> = Result<T, FfiError>;

fn ffi_error(status: macmon_status_t, message: impl Into<String>) -> FfiError {
  FfiError { status, message: message.into() }
}

fn make_c_string(value: &str) -> CString {
  CString::new(value).unwrap_or_else(|_| {
    let sanitized = value.replace('\0', " ");
    CString::new(sanitized).expect("sanitized string must not contain interior nul bytes")
  })
}

fn set_last_error(message: impl AsRef<str>) {
  LAST_ERROR.with(|slot| {
    *slot.borrow_mut() = Some(make_c_string(message.as_ref()));
  });
}

fn clear_last_error() {
  LAST_ERROR.with(|slot| {
    *slot.borrow_mut() = None;
  });
}

fn ffi_string(value: &str) -> *const c_char {
  make_c_string(value).into_raw()
}

fn ffi_u32_array(values: &[u32]) -> (*mut u32, usize) {
  let slice = values.to_vec().into_boxed_slice();
  let len = slice.len();
  let ptr = Box::into_raw(slice) as *mut u32;
  (ptr, len)
}

fn ffi_usage_list(items: &[UsageEntry]) -> macmon_usage_list_t {
  let entries = items
    .iter()
    .map(|entry| macmon_usage_entry_t {
      name: ffi_string(&entry.name),
      freq_mhz: entry.freq_mhz,
      usage: entry.usage,
      units: entry.units,
    })
    .collect::<Vec<_>>()
    .into_boxed_slice();

  macmon_usage_list_t { len: entries.len(), ptr: Box::into_raw(entries) as *mut macmon_usage_entry_t }
}

fn ffi_metrics(metrics: Metrics) -> macmon_metrics_t {
  macmon_metrics_t {
    cpu: ffi_usage_list(&metrics.usage.cpu),
    gpu: ffi_usage_list(&metrics.usage.gpu),
    power: macmon_power_metrics_t {
      cpu: metrics.power.cpu,
      gpu: metrics.power.gpu,
      ram: metrics.power.ram,
      sys: metrics.power.sys,
      gpu_ram: metrics.power.gpu_ram,
      ane: metrics.power.ane,
      all: metrics.power.all,
    },
    memory: macmon_mem_metrics_t {
      ram_total: metrics.memory.ram_total,
      ram_usage: metrics.memory.ram_usage,
      swap_total: metrics.memory.swap_total,
      swap_usage: metrics.memory.swap_usage,
    },
    temp: macmon_temp_metrics_t {
      cpu_temp_avg: metrics.temp.cpu_temp_avg,
      gpu_temp_avg: metrics.temp.gpu_temp_avg,
    },
  }
}

fn ffi_soc_info(info: SocInfo) -> macmon_soc_info_t {
  let domains = info
    .cpu_domains
    .into_iter()
    .map(|domain| {
      let (freqs_mhz, freqs_len) = ffi_u32_array(&domain.freqs);
      macmon_cpu_domain_t {
        name: ffi_string(&domain.channel_prefix),
        units: domain.units,
        freqs_len,
        freqs_mhz,
      }
    })
    .collect::<Vec<_>>()
    .into_boxed_slice();
  let (gpu_freqs_mhz, gpu_freqs_len) = ffi_u32_array(&info.gpu_freqs);

  macmon_soc_info_t {
    mac_model: ffi_string(&info.mac_model),
    chip_name: ffi_string(&info.chip_name),
    memory_gb: info.memory_gb,
    cpu_cores_total: info.cpu_cores_total,
    cpu_domains_len: domains.len(),
    cpu_domains: Box::into_raw(domains) as *mut macmon_cpu_domain_t,
    gpu_cores: info.gpu_cores,
    gpu_freqs_len,
    gpu_freqs_mhz,
  }
}

fn ffi_status<F>(f: F) -> macmon_status_t
where
  F: FnOnce() -> FfiResult<()> + std::panic::UnwindSafe,
{
  match catch_unwind(AssertUnwindSafe(f)) {
    Ok(Ok(())) => {
      clear_last_error();
      macmon_status_t::MACMON_STATUS_OK
    }
    Ok(Err(err)) => {
      set_last_error(err.message);
      err.status
    }
    Err(_) => {
      set_last_error("panic across FFI boundary");
      macmon_status_t::MACMON_STATUS_PANIC
    }
  }
}

unsafe fn sampler_mut<'a>(sampler: *mut macmon_sampler_t) -> FfiResult<&'a mut Sampler> {
  if sampler.is_null() {
    return Err(ffi_error(
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT,
      "sampler must not be null",
    ));
  }

  Ok(unsafe { &mut *(sampler as *mut Sampler) })
}

unsafe fn free_c_string(ptr: *const c_char) {
  if !ptr.is_null() {
    unsafe { drop(CString::from_raw(ptr as *mut c_char)) };
  }
}

unsafe fn free_u32_array(ptr: *mut u32, len: usize) {
  if !ptr.is_null() {
    let raw = ptr::slice_from_raw_parts_mut(ptr, len);
    unsafe { drop(Box::from_raw(raw)) };
  }
}

unsafe fn free_usage_list(list: &mut macmon_usage_list_t) {
  if !list.ptr.is_null() {
    let slice_ptr = ptr::slice_from_raw_parts_mut(list.ptr, list.len);
    let entries = unsafe { Box::from_raw(slice_ptr) };
    for entry in entries.iter() {
      unsafe { free_c_string(entry.name) };
    }
  }

  *list = macmon_usage_list_t::default();
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_sampler_new(out_sampler: *mut *mut macmon_sampler_t) -> macmon_status_t {
  ffi_status(|| {
    if out_sampler.is_null() {
      return Err(ffi_error(
        macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT,
        "out_sampler must not be null",
      ));
    }

    unsafe {
      *out_sampler = ptr::null_mut();
    }

    let sampler = Box::new(Sampler::new().map_err(|err| {
      ffi_error(macmon_status_t::MACMON_STATUS_INIT_FAILED, err.to_string())
    })?);

    unsafe {
      *out_sampler = Box::into_raw(sampler) as *mut macmon_sampler_t;
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_sampler_free(sampler: *mut macmon_sampler_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if sampler.is_null() {
      return;
    }

    unsafe {
      drop(Box::from_raw(sampler as *mut Sampler));
    }
  }));

  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_sampler_get_soc_info(
  sampler: *mut macmon_sampler_t,
  out_info: *mut macmon_soc_info_t,
) -> macmon_status_t {
  ffi_status(|| {
    if out_info.is_null() {
      return Err(ffi_error(
        macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT,
        "out_info must not be null",
      ));
    }

    unsafe {
      ptr::write(out_info, macmon_soc_info_t::default());
    }

    let sampler = unsafe { sampler_mut(sampler)? };
    let info = ffi_soc_info(sampler.get_soc_info().clone());

    unsafe {
      ptr::write(out_info, info);
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_soc_info_free(info: *mut macmon_soc_info_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if info.is_null() {
      return;
    }

    let info = unsafe { &mut *info };
    unsafe {
      free_c_string(info.mac_model);
      free_c_string(info.chip_name);

      if !info.cpu_domains.is_null() {
        let domains_ptr = ptr::slice_from_raw_parts_mut(info.cpu_domains, info.cpu_domains_len);
        let domains = Box::from_raw(domains_ptr);
        for domain in domains.iter() {
          free_c_string(domain.name);
          free_u32_array(domain.freqs_mhz, domain.freqs_len);
        }
      }

      free_u32_array(info.gpu_freqs_mhz, info.gpu_freqs_len);
    }

    *info = macmon_soc_info_t::default();
  }));

  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_sampler_get_metrics(
  sampler: *mut macmon_sampler_t,
  duration_ms: u32,
  out_metrics: *mut macmon_metrics_t,
) -> macmon_status_t {
  ffi_status(|| {
    if out_metrics.is_null() {
      return Err(ffi_error(
        macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT,
        "out_metrics must not be null",
      ));
    }

    unsafe {
      ptr::write(out_metrics, macmon_metrics_t::default());
    }

    let sampler = unsafe { sampler_mut(sampler)? };
    let metrics = sampler.get_metrics(duration_ms.max(100)).map_err(|err| {
      ffi_error(macmon_status_t::MACMON_STATUS_SAMPLE_FAILED, err.to_string())
    })?;

    unsafe {
      ptr::write(out_metrics, ffi_metrics(metrics));
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_metrics_free(metrics: *mut macmon_metrics_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if metrics.is_null() {
      return;
    }

    let metrics = unsafe { &mut *metrics };
    unsafe {
      free_usage_list(&mut metrics.cpu);
      free_usage_list(&mut metrics.gpu);
    }
    *metrics = macmon_metrics_t::default();
  }));

  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_last_error_message() -> *const c_char {
  LAST_ERROR.with(|slot| {
    slot.borrow().as_ref().map_or(ptr::null(), |msg| msg.as_ptr() as *const c_char)
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::metrics::{MemMetrics, PowerMetrics, TempMetrics, UsageMetrics};
  use crate::sources::CpuDomainInfo;
  use std::ffi::CStr;
  use std::mem;

  fn read_c_str(ptr: *const c_char) -> String {
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
  }

  fn test_metrics() -> Metrics {
    Metrics {
      usage: UsageMetrics {
        cpu: vec![
          UsageEntry { name: "ECPU0".to_string(), freq_mhz: 1200, usage: 0.25, units: 4 },
          UsageEntry { name: "PCPU0".to_string(), freq_mhz: 3200, usage: 0.75, units: 8 },
        ],
        gpu: vec![UsageEntry { name: "GFX0".to_string(), freq_mhz: 900, usage: 0.5, units: 10 }],
      },
      power: PowerMetrics { cpu: 1.0, gpu: 2.0, ram: 3.0, sys: 4.0, gpu_ram: 5.0, ane: 6.0, all: 17.0 },
      memory: MemMetrics { ram_total: 10, ram_usage: 11, swap_total: 12, swap_usage: 13 },
      temp: TempMetrics { cpu_temp_avg: 50.0, gpu_temp_avg: 51.0 },
    }
  }

  fn test_soc_info() -> SocInfo {
    SocInfo {
      mac_model: "Mac16,1".to_string(),
      chip_name: "Apple M4".to_string(),
      memory_gb: 24,
      cpu_cores_total: 10,
      cpu_domains: vec![
        CpuDomainInfo {
          id: "cpu0".to_string(),
          units: 4,
          freqs: vec![1000, 2000],
          channel_prefix: "ECPU".to_string(),
          core_prefix: "ECPU".to_string(),
        },
        CpuDomainInfo {
          id: "cpu1".to_string(),
          units: 6,
          freqs: vec![3000, 4000],
          channel_prefix: "PCPU".to_string(),
          core_prefix: "PCPU".to_string(),
        },
      ],
      gpu_cores: 10,
      gpu_freqs: vec![500, 1000],
    }
  }

  #[test]
  fn metrics_conversion_preserves_names_and_values() {
    let mut ffi = ffi_metrics(test_metrics());

    assert_eq!(ffi.cpu.len, 2);
    assert_eq!(ffi.gpu.len, 1);

    let cpu = unsafe { std::slice::from_raw_parts(ffi.cpu.ptr, ffi.cpu.len) };
    assert_eq!(read_c_str(cpu[0].name), "ECPU0");
    assert_eq!(cpu[0].freq_mhz, 1200);
    assert_eq!(cpu[0].usage, 0.25);
    assert_eq!(cpu[0].units, 4);
    assert_eq!(read_c_str(cpu[1].name), "PCPU0");

    let gpu = unsafe { std::slice::from_raw_parts(ffi.gpu.ptr, ffi.gpu.len) };
    assert_eq!(read_c_str(gpu[0].name), "GFX0");
    assert_eq!(ffi.power.all, 17.0);
    assert_eq!(ffi.memory.swap_usage, 13);
    assert_eq!(ffi.temp.gpu_temp_avg, 51.0);

    macmon_metrics_free(&mut ffi);
    assert!(ffi.cpu.ptr.is_null());
    assert!(ffi.gpu.ptr.is_null());
  }

  #[test]
  fn soc_info_conversion_preserves_layout() {
    let mut ffi = ffi_soc_info(test_soc_info());

    assert_eq!(read_c_str(ffi.mac_model), "Mac16,1");
    assert_eq!(read_c_str(ffi.chip_name), "Apple M4");
    assert_eq!(ffi.cpu_domains_len, 2);
    assert_eq!(ffi.gpu_freqs_len, 2);

    let domains = unsafe { std::slice::from_raw_parts(ffi.cpu_domains, ffi.cpu_domains_len) };
    assert_eq!(read_c_str(domains[0].name), "ECPU");
    assert_eq!(domains[0].units, 4);
    let freqs = unsafe { std::slice::from_raw_parts(domains[1].freqs_mhz, domains[1].freqs_len) };
    assert_eq!(freqs, &[3000, 4000]);

    macmon_soc_info_free(&mut ffi);
    assert!(ffi.cpu_domains.is_null());
    assert!(ffi.gpu_freqs_mhz.is_null());
  }

  #[test]
  fn free_functions_accept_zero_initialized_structs() {
    let mut metrics = macmon_metrics_t::default();
    let mut info = macmon_soc_info_t::default();

    macmon_metrics_free(&mut metrics);
    macmon_soc_info_free(&mut info);
  }

  #[test]
  fn null_out_arguments_return_invalid_argument() {
    assert_eq!(
      macmon_sampler_new(ptr::null_mut()),
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
      macmon_sampler_get_soc_info(ptr::null_mut(), ptr::null_mut()),
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
      macmon_sampler_get_metrics(ptr::null_mut(), 0, ptr::null_mut()),
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
  }

  #[test]
  fn last_error_message_updates_after_failure() {
    let status = macmon_sampler_get_metrics(ptr::null_mut(), 0, ptr::null_mut());
    assert_eq!(status, macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT);

    let message = macmon_last_error_message();
    assert!(!message.is_null());
    assert!(read_c_str(message).contains("out_metrics"));
  }

  #[test]
  fn ffi_status_translates_panics() {
    let status = ffi_status(|| -> FfiResult<()> {
      panic!("boom");
    });

    assert_eq!(status, macmon_status_t::MACMON_STATUS_PANIC);
    assert!(read_c_str(macmon_last_error_message()).contains("panic"));
  }

  #[test]
  fn free_functions_zero_out_structs_after_owned_allocations() {
    let mut metrics = ffi_metrics(test_metrics());
    let mut info = ffi_soc_info(test_soc_info());

    macmon_metrics_free(&mut metrics);
    macmon_soc_info_free(&mut info);

    assert_eq!(metrics.cpu.len, 0);
    assert_eq!(info.cpu_domains_len, 0);
    assert_eq!(info.gpu_freqs_len, 0);
  }

  #[test]
  fn default_ffi_structs_match_zeroed_layout() {
    let zeroed_metrics: macmon_metrics_t = unsafe { mem::zeroed() };
    let zeroed_info: macmon_soc_info_t = unsafe { mem::zeroed() };

    assert_eq!(zeroed_metrics.cpu.len, 0);
    assert!(zeroed_metrics.cpu.ptr.is_null());
    assert_eq!(zeroed_info.cpu_domains_len, 0);
    assert!(zeroed_info.cpu_domains.is_null());
  }

  #[cfg(target_os = "macos")]
  #[test]
  #[ignore = "requires access to macOS sensor APIs"]
  fn smoke_sampler_roundtrip() {
    let mut sampler = ptr::null_mut();
    assert_eq!(macmon_sampler_new(&mut sampler), macmon_status_t::MACMON_STATUS_OK);
    assert!(!sampler.is_null());

    let mut info = macmon_soc_info_t::default();
    assert_eq!(macmon_sampler_get_soc_info(sampler, &mut info), macmon_status_t::MACMON_STATUS_OK);

    let mut metrics = macmon_metrics_t::default();
    assert_eq!(
      macmon_sampler_get_metrics(sampler, 100, &mut metrics),
      macmon_status_t::MACMON_STATUS_OK
    );

    macmon_metrics_free(&mut metrics);
    macmon_soc_info_free(&mut info);
    macmon_sampler_free(sampler);
  }
}
