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
    temp: TempMetrics { cpu_avg: 50.0, gpu_avg: 51.0 },
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
        name: "ECPU".to_string(),
        units: 4,
        freqs: vec![1000, 2000],
        core_prefix: "ECPU".to_string(),
      },
      CpuDomainInfo {
        name: "PCPU".to_string(),
        units: 6,
        freqs: vec![3000, 4000],
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
  assert_eq!(ffi.temp.gpu_avg, 51.0);

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
