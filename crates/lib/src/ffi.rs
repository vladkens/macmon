use crate::metrics::{CpuUsageEntry, GpuUsageEntry, Metrics, Sampler};
use crate::sources::{SocInfo, get_soc_info};
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
pub struct macmon_cpu_usage_t {
  pub name: *const c_char,
  pub units: u32,
  pub freq_mhz: u32,
  pub usage: f32,
  pub cores_freq_mhz: *mut u32,
  pub cores_usage: *mut f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_cpu_usage_list_t {
  pub len: usize,
  pub ptr: *mut macmon_cpu_usage_t,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_gpu_usage_t {
  pub name: *const c_char,
  pub units: u32,
  pub freq_mhz: u32,
  pub usage: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_gpu_usage_list_t {
  pub len: usize,
  pub ptr: *mut macmon_gpu_usage_t,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_power_metrics_t {
  pub package: f32, // SoC/package power.
  pub cpu: f32,     // CPU power within `package`.
  pub gpu: f32,     // GPU power within `package`.
  pub ram: f32,     // DRAM power within `package`.
  pub gpu_ram: f32, // GPU SRAM power within `package`.
  pub ane: f32,     // ANE power within `package`.
  pub board: f32,   // System Total (`PSTR`).
  pub battery: f32, // Battery rail power (`PPBR`).
  pub dc_in: f32,   // DC input power (`PDTR`).
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
  pub cpu_avg: f32,
  pub gpu_avg: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_metrics_t {
  pub cpu_usage: macmon_cpu_usage_list_t,
  pub gpu_usage: macmon_gpu_usage_list_t,
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

fn ffi_cpu_usage_list(items: &[CpuUsageEntry]) -> macmon_cpu_usage_list_t {
  let entries = items
    .iter()
    .map(|entry| {
      let core_freqs =
        entry.cores.iter().map(|core| core.freq_mhz).collect::<Vec<_>>().into_boxed_slice();
      let core_usages =
        entry.cores.iter().map(|core| core.usage).collect::<Vec<_>>().into_boxed_slice();
      macmon_cpu_usage_t {
        name: ffi_string(&entry.name),
        units: entry.cores.len() as u32,
        freq_mhz: entry.freq_mhz,
        usage: entry.usage,
        cores_freq_mhz: Box::into_raw(core_freqs) as *mut u32,
        cores_usage: Box::into_raw(core_usages) as *mut f32,
      }
    })
    .collect::<Vec<_>>()
    .into_boxed_slice();

  macmon_cpu_usage_list_t {
    len: entries.len(),
    ptr: Box::into_raw(entries) as *mut macmon_cpu_usage_t,
  }
}

fn ffi_gpu_usage_list(items: &[GpuUsageEntry]) -> macmon_gpu_usage_list_t {
  let entries = items
    .iter()
    .map(|entry| macmon_gpu_usage_t {
      name: ffi_string(&entry.name),
      units: entry.units,
      freq_mhz: entry.freq_mhz,
      usage: entry.usage,
    })
    .collect::<Vec<_>>()
    .into_boxed_slice();

  macmon_gpu_usage_list_t {
    len: entries.len(),
    ptr: Box::into_raw(entries) as *mut macmon_gpu_usage_t,
  }
}

fn ffi_metrics(metrics: Metrics) -> macmon_metrics_t {
  macmon_metrics_t {
    cpu_usage: ffi_cpu_usage_list(&metrics.cpu_usage),
    gpu_usage: ffi_gpu_usage_list(&metrics.gpu_usage),
    power: macmon_power_metrics_t {
      package: metrics.power.package,
      cpu: metrics.power.cpu,
      gpu: metrics.power.gpu,
      ram: metrics.power.ram,
      gpu_ram: metrics.power.gpu_ram,
      ane: metrics.power.ane,
      board: metrics.power.board,
      battery: metrics.power.battery,
      dc_in: metrics.power.dc_in,
    },
    memory: macmon_mem_metrics_t {
      ram_total: metrics.memory.ram_total,
      ram_usage: metrics.memory.ram_usage,
      swap_total: metrics.memory.swap_total,
      swap_usage: metrics.memory.swap_usage,
    },
    temp: macmon_temp_metrics_t { cpu_avg: metrics.temp.cpu_avg, gpu_avg: metrics.temp.gpu_avg },
  }
}

fn ffi_soc_info(info: SocInfo) -> macmon_soc_info_t {
  let domains = info
    .cpu_domains
    .into_iter()
    .map(|domain| {
      let (freqs_mhz, freqs_len) = ffi_u32_array(&domain.freqs_mhz);
      macmon_cpu_domain_t {
        name: ffi_string(&domain.name),
        units: domain.units,
        freqs_len,
        freqs_mhz,
      }
    })
    .collect::<Vec<_>>()
    .into_boxed_slice();
  let (gpu_freqs_mhz, gpu_freqs_len) = ffi_u32_array(&info.gpu_freqs_mhz);

  macmon_soc_info_t {
    mac_model: ffi_string(&info.mac_model),
    chip_name: ffi_string(&info.chip_name),
    memory_gb: info.memory_gb,
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

unsafe fn free_cpu_usage_list(list: &mut macmon_cpu_usage_list_t) {
  if !list.ptr.is_null() {
    let slice_ptr = ptr::slice_from_raw_parts_mut(list.ptr, list.len);
    let entries = unsafe { Box::from_raw(slice_ptr) };
    for entry in entries.iter() {
      unsafe { free_c_string(entry.name) };
      unsafe { free_u32_array(entry.cores_freq_mhz, entry.units as usize) };
      if !entry.cores_usage.is_null() {
        let raw = ptr::slice_from_raw_parts_mut(entry.cores_usage, entry.units as usize);
        unsafe { drop(Box::from_raw(raw)) };
      }
    }
  }

  *list = macmon_cpu_usage_list_t::default();
}

unsafe fn free_gpu_usage_list(list: &mut macmon_gpu_usage_list_t) {
  if !list.ptr.is_null() {
    let slice_ptr = ptr::slice_from_raw_parts_mut(list.ptr, list.len);
    let entries = unsafe { Box::from_raw(slice_ptr) };
    for entry in entries.iter() {
      unsafe { free_c_string(entry.name) };
    }
  }

  *list = macmon_gpu_usage_list_t::default();
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `out_sampler` must be a valid, writable pointer to a `macmon_sampler_t*` slot.
pub unsafe extern "C" fn macmon_sampler_new(
  out_sampler: *mut *mut macmon_sampler_t,
) -> macmon_status_t {
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

    let sampler = Box::new(
      Sampler::new()
        .map_err(|err| ffi_error(macmon_status_t::MACMON_STATUS_INIT_FAILED, err.to_string()))?,
    );

    unsafe {
      *out_sampler = Box::into_raw(sampler) as *mut macmon_sampler_t;
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `sampler` must either be null or a pointer previously returned by
/// `macmon_sampler_new` that has not already been freed.
pub unsafe extern "C" fn macmon_sampler_free(sampler: *mut macmon_sampler_t) {
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
/// # Safety
///
/// `out_info` must be a valid, writable pointer to an initialized
/// `macmon_soc_info_t` slot.
pub unsafe extern "C" fn macmon_get_soc_info(out_info: *mut macmon_soc_info_t) -> macmon_status_t {
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

    let info = ffi_soc_info(
      get_soc_info()
        .map_err(|err| ffi_error(macmon_status_t::MACMON_STATUS_INIT_FAILED, err.to_string()))?,
    );

    unsafe {
      ptr::write(out_info, info);
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `info` must either be null or a pointer previously initialized by
/// `macmon_get_soc_info` and not already freed with `macmon_soc_info_free`.
pub unsafe extern "C" fn macmon_soc_info_free(info: *mut macmon_soc_info_t) {
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
/// # Safety
///
/// `sampler` must be a valid pointer returned by `macmon_sampler_new`.
/// `out_metrics` must be a valid, writable pointer to an initialized
/// `macmon_metrics_t` slot.
pub unsafe extern "C" fn macmon_sampler_get_metrics(
  sampler: *mut macmon_sampler_t,
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
    let metrics = sampler
      .get_metrics()
      .map_err(|err| ffi_error(macmon_status_t::MACMON_STATUS_SAMPLE_FAILED, err.to_string()))?;

    unsafe {
      ptr::write(out_metrics, ffi_metrics(metrics));
    }
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `metrics` must either be null or a pointer previously initialized by
/// `macmon_sampler_get_metrics` and not already freed with `macmon_metrics_free`.
pub unsafe extern "C" fn macmon_metrics_free(metrics: *mut macmon_metrics_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if metrics.is_null() {
      return;
    }

    let metrics = unsafe { &mut *metrics };
    unsafe {
      free_cpu_usage_list(&mut metrics.cpu_usage);
      free_gpu_usage_list(&mut metrics.gpu_usage);
    }
    *metrics = macmon_metrics_t::default();
  }));

  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_last_error_message() -> *const c_char {
  LAST_ERROR
    .with(|slot| slot.borrow().as_ref().map_or(ptr::null(), |msg| msg.as_ptr() as *const c_char))
}

#[cfg(test)]
#[path = "ffi_test.rs"]
mod tests;
