use super::*;
use crate::metrics::{
  CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, MemMetrics, PowerMetrics, TempMetrics,
};
use crate::sources::CpuDomainInfo;
use std::ffi::CStr;
use std::mem;

fn read_c_str(ptr: *const c_char) -> String {
  unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

fn test_metrics() -> Metrics {
  Metrics {
    cpu_usage: vec![
      CpuUsageEntry {
        name: "ECPU".to_string(),
        freq_mhz: 1200,
        usage: 0.25,
        cores: vec![
          CoreUsageEntry { freq_mhz: 1100, usage: 0.2 },
          CoreUsageEntry { freq_mhz: 1300, usage: 0.3 },
        ],
      },
      CpuUsageEntry { name: "PCPU".to_string(), freq_mhz: 3200, usage: 0.75, cores: Vec::new() },
    ],
    gpu_usage: vec![GpuUsageEntry {
      name: "GFX0".to_string(),
      freq_mhz: 900,
      usage: 0.5,
      units: 10,
    }],
    power: PowerMetrics {
      package: 17.0,
      cpu: 1.0,
      gpu: 2.0,
      ram: 3.0,
      gpu_ram: 5.0,
      ane: 6.0,
      board: 4.0,
      battery: 7.0,
      dc_in: 8.0,
    },
    memory: MemMetrics { ram_total: 10, ram_usage: 11, swap_total: 12, swap_usage: 13 },
    temp: TempMetrics { cpu_avg: 50.0, gpu_avg: 51.0 },
  }
}

fn test_soc_info() -> SocInfo {
  SocInfo {
    mac_model: "Mac16,1".to_string(),
    chip_name: "Apple M4".to_string(),
    memory_gb: 24,
    cpu_domains: vec![
      CpuDomainInfo { name: "ECPU".to_string(), units: 4, freqs_mhz: vec![1000, 2000] },
      CpuDomainInfo { name: "PCPU".to_string(), units: 6, freqs_mhz: vec![3000, 4000] },
    ],
    gpu_cores: 10,
    gpu_freqs_mhz: vec![500, 1000],
  }
}

#[test]
fn metrics_conversion_preserves_names_and_values() {
  let mut ffi = ffi_metrics(test_metrics());

  assert_eq!(ffi.cpu_usage.len, 2);
  assert_eq!(ffi.gpu_usage.len, 1);

  let cpu = unsafe { std::slice::from_raw_parts(ffi.cpu_usage.ptr, ffi.cpu_usage.len) };
  assert_eq!(read_c_str(cpu[0].name), "ECPU");
  assert_eq!(cpu[0].units, 2);
  assert_eq!(cpu[0].freq_mhz, 1200);
  assert_eq!(cpu[0].usage, 0.25);
  assert_eq!(read_c_str(cpu[1].name), "PCPU");
  assert_eq!(cpu[1].units, 0);

  let cpu_core_freqs =
    unsafe { std::slice::from_raw_parts(cpu[0].cores_freq_mhz, cpu[0].units as usize) };
  let cpu_core_usages =
    unsafe { std::slice::from_raw_parts(cpu[0].cores_usage, cpu[0].units as usize) };
  assert_eq!(cpu_core_freqs[0], 1100);
  assert_eq!(cpu_core_usages[0], 0.2);
  assert_eq!(cpu_core_freqs[1], 1300);

  let gpu = unsafe { std::slice::from_raw_parts(ffi.gpu_usage.ptr, ffi.gpu_usage.len) };
  assert_eq!(read_c_str(gpu[0].name), "GFX0");
  assert_eq!(gpu[0].units, 10);
  assert_eq!(gpu[0].freq_mhz, 900);
  assert_eq!(ffi.power.package, 17.0);
  assert_eq!(ffi.power.board, 4.0);
  assert_eq!(ffi.power.battery, 7.0);
  assert_eq!(ffi.power.dc_in, 8.0);
  assert_eq!(ffi.memory.swap_usage, 13);
  assert_eq!(ffi.temp.gpu_avg, 51.0);

  unsafe { macmon_metrics_free(&mut ffi) };
  assert!(ffi.cpu_usage.ptr.is_null());
  assert!(ffi.gpu_usage.ptr.is_null());
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

  unsafe { macmon_soc_info_free(&mut ffi) };
  assert!(ffi.cpu_domains.is_null());
  assert!(ffi.gpu_freqs_mhz.is_null());
}

#[test]
fn free_functions_accept_zero_initialized_structs() {
  let mut metrics = macmon_metrics_t::default();
  let mut info = macmon_soc_info_t::default();

  unsafe { macmon_metrics_free(&mut metrics) };
  unsafe { macmon_soc_info_free(&mut info) };
}

#[test]
fn null_out_arguments_return_invalid_argument() {
  assert_eq!(
    unsafe { macmon_sampler_new(ptr::null_mut()) },
    macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
  );
  assert_eq!(
    unsafe { macmon_get_soc_info(ptr::null_mut()) },
    macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
  );
  assert_eq!(
    unsafe { macmon_sampler_get_metrics(ptr::null_mut(), ptr::null_mut()) },
    macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
  );
}

#[test]
fn last_error_message_updates_after_failure() {
  let status = unsafe { macmon_sampler_get_metrics(ptr::null_mut(), ptr::null_mut()) };
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

  unsafe { macmon_metrics_free(&mut metrics) };
  unsafe { macmon_soc_info_free(&mut info) };

  assert_eq!(metrics.cpu_usage.len, 0);
  assert_eq!(info.cpu_domains_len, 0);
  assert_eq!(info.gpu_freqs_len, 0);
}

#[test]
fn default_ffi_structs_match_zeroed_layout() {
  let zeroed_metrics: macmon_metrics_t = unsafe { mem::zeroed() };
  let zeroed_info: macmon_soc_info_t = unsafe { mem::zeroed() };

  assert_eq!(zeroed_metrics.cpu_usage.len, 0);
  assert!(zeroed_metrics.cpu_usage.ptr.is_null());
  assert_eq!(zeroed_info.cpu_domains_len, 0);
  assert!(zeroed_info.cpu_domains.is_null());
}

#[test]
fn smoke_sampler_roundtrip() {
  let mut sampler = ptr::null_mut();
  let status = unsafe { macmon_sampler_new(&mut sampler) };
  if status == macmon_status_t::MACMON_STATUS_INIT_FAILED {
    eprintln!("skipping env-dependent smoke test: sampler init failed on this host");
    return;
  }
  assert_eq!(status, macmon_status_t::MACMON_STATUS_OK);
  assert!(!sampler.is_null());

  let mut info = macmon_soc_info_t::default();
  assert_eq!(unsafe { macmon_get_soc_info(&mut info) }, macmon_status_t::MACMON_STATUS_OK);

  let mut metrics = macmon_metrics_t::default();
  assert_eq!(
    unsafe { macmon_sampler_get_metrics(sampler, &mut metrics) },
    macmon_status_t::MACMON_STATUS_OK
  );

  unsafe { macmon_metrics_free(&mut metrics) };
  unsafe { macmon_soc_info_free(&mut info) };
  unsafe { macmon_sampler_free(sampler) };
}
