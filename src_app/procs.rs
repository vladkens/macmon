//! Per-process resource usage (CPU, memory, energy) sampled without sudo.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::mem;
use std::time::Instant;

const RUSAGE_INFO_V4: c_int = 4;
const RUSAGE_INFO_V6: c_int = 6;

/// `struct rusage_info_v6` from the macOS SDK `sys/resource.h`. `rusage_info_v4` is a prefix of
/// it, so the same buffer serves both flavors.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)] // FFI layout, most fields are unused
pub struct rusage_info_v6 {
  pub ri_uuid: [u8; 16],
  pub ri_user_time: u64,
  pub ri_system_time: u64,
  pub ri_pkg_idle_wkups: u64,
  pub ri_interrupt_wkups: u64,
  pub ri_pageins: u64,
  pub ri_wired_size: u64,
  pub ri_resident_size: u64,
  pub ri_phys_footprint: u64,
  pub ri_proc_start_abstime: u64,
  pub ri_proc_exit_abstime: u64,
  pub ri_child_user_time: u64,
  pub ri_child_system_time: u64,
  pub ri_child_pkg_idle_wkups: u64,
  pub ri_child_interrupt_wkups: u64,
  pub ri_child_pageins: u64,
  pub ri_child_elapsed_abstime: u64,
  pub ri_diskio_bytesread: u64,
  pub ri_diskio_byteswritten: u64,
  pub ri_cpu_time_qos_default: u64,
  pub ri_cpu_time_qos_maintenance: u64,
  pub ri_cpu_time_qos_background: u64,
  pub ri_cpu_time_qos_utility: u64,
  pub ri_cpu_time_qos_legacy: u64,
  pub ri_cpu_time_qos_user_initiated: u64,
  pub ri_cpu_time_qos_user_interactive: u64,
  pub ri_billed_system_time: u64,
  pub ri_serviced_system_time: u64,
  pub ri_logical_writes: u64,
  pub ri_lifetime_max_phys_footprint: u64,
  pub ri_instructions: u64,
  pub ri_cycles: u64,
  pub ri_billed_energy: u64,
  pub ri_serviced_energy: u64,
  pub ri_interval_max_phys_footprint: u64,
  pub ri_runnable_time: u64,
  pub ri_flags: u64,
  pub ri_user_ptime: u64,
  pub ri_system_ptime: u64,
  pub ri_pinstructions: u64,
  pub ri_pcycles: u64,
  pub ri_energy_nj: u64,
  pub ri_penergy_nj: u64,
  pub ri_secure_time_in_system: u64,
  pub ri_secure_ptime_in_system: u64,
  pub ri_neural_footprint: u64,
  pub ri_lifetime_max_neural_footprint: u64,
  pub ri_interval_max_neural_footprint: u64,
  pub ri_conclave_footprint: u64,
  pub ri_page_wait_time_mach: u64,
  pub ri_page_cache_hits: u64,
  pub ri_reserved: [u64; 6],
}

#[repr(C)]
#[derive(Default)]
struct MachTimebaseInfo {
  numer: u32,
  denom: u32,
}

unsafe extern "C" {
  fn mach_timebase_info(info: *mut MachTimebaseInfo) -> c_int;
}

/// One row of the process list.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
  pub pid: i32,
  pub ppid: i32,
  pub name: String,
  pub user: String,
  pub cpu_pct: f32,         // 100% = one fully busy core, as in Activity Monitor.
  pub mem_bytes: u64,       // Physical footprint, as Activity Monitor's "Memory".
  pub power_w: Option<f32>, // None when the energy counter isn't readable.
  pub gpu_pct: f32,
}

/// Cumulative counters of one process; rates come from two snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Counters {
  start: u64,             // Process start time, tells a reused pid apart.
  cpu_ns: u64,            // User + system CPU time.
  energy_nj: Option<u64>, // Lifetime energy, None when unavailable.
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Usage {
  cpu_pct: f32,
  power_w: Option<f32>,
}

/// Rates between two snapshots taken `elapsed_ns` apart. A first sample, a reused pid (different
/// start time) or a counter going backwards reads as idle instead of a spike.
fn usage(prev: Option<&Counters>, cur: &Counters, elapsed_ns: u64) -> Usage {
  let idle = Usage { cpu_pct: 0.0, power_w: cur.energy_nj.map(|_| 0.0) };
  let Some(prev) = prev else { return idle };
  if elapsed_ns == 0 || prev.start != cur.start || cur.cpu_ns < prev.cpu_ns {
    return idle;
  }

  let energy_nj = match (prev.energy_nj, cur.energy_nj) {
    (Some(prev), Some(cur)) if cur < prev => return idle,
    (Some(prev), Some(cur)) => Some(cur - prev),
    (None, Some(_)) => Some(0),
    (_, None) => None,
  };

  let elapsed_ns = elapsed_ns as f64;
  Usage {
    cpu_pct: ((cur.cpu_ns - prev.cpu_ns) as f64 / elapsed_ns * 100.0) as f32,
    power_w: energy_nj.map(|nj| (nj as f64 / elapsed_ns) as f32), // nJ per ns = W
  }
}

/// Converts mach absolute time units to nanoseconds.
fn ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
  (ticks as u128 * numer as u128 / denom.max(1) as u128) as u64
}

/// String from a fixed-size C buffer that may lack the trailing NUL.
fn c_chars_to_string(chars: &[c_char]) -> String {
  let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
  String::from_utf8_lossy(&bytes).into_owned()
}

fn basename(path: &str) -> Option<&str> {
  path.rsplit('/').next().filter(|name| !name.is_empty())
}

fn list_pids(pids: &mut Vec<i32>) {
  pids.clear();
  // With a NULL buffer it returns the number of pids; reserve extra for processes spawned between
  // the two calls.
  let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
  if count <= 0 {
    return;
  }

  pids.resize(count as usize + 64, 0);
  let size = (pids.len() * mem::size_of::<i32>()) as c_int;
  let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut c_void, size) };
  pids.truncate(count.max(0) as usize);
}

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
  let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
  let size = mem::size_of::<libc::proc_bsdinfo>() as c_int;
  let ptr = &mut info as *mut libc::proc_bsdinfo as *mut c_void;
  let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, ptr, size) };
  (read == size).then_some(info)
}

fn rusage(pid: i32, flavor: c_int) -> Option<rusage_info_v6> {
  let mut info = rusage_info_v6::default();
  let ptr = &mut info as *mut rusage_info_v6 as *mut libc::rusage_info_t;
  let ret = unsafe { libc::proc_pid_rusage(pid, flavor, ptr) };
  (ret == 0).then_some(info)
}

/// Executable basename, falling back to the kernel's process name.
fn process_name(pid: i32, info: &libc::proc_bsdinfo) -> String {
  let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
  let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
  if len > 0 {
    let path = String::from_utf8_lossy(&buf[..len as usize]);
    if let Some(name) = basename(&path) {
      return name.to_string();
    }
  }

  let name = c_chars_to_string(&info.pbi_name);
  if name.is_empty() { c_chars_to_string(&info.pbi_comm) } else { name }
}

/// A process seen on the previous tick.
struct Known {
  counters: Counters,
  comm: String, // Kernel name, changes on exec and invalidates the cached `name`.
  name: String,
}

/// Samples processes readable through libproc: those of the current user, or all when root.
pub struct ProcSampler {
  timebase: (u32, u32),
  flavor: c_int,
  pids: Vec<i32>,
  known: HashMap<i32, Known>,
  last: Option<Instant>,
}

impl ProcSampler {
  pub fn new() -> Self {
    let mut info = MachTimebaseInfo::default();
    let timebase = match unsafe { mach_timebase_info(&mut info) } {
      0 if info.numer != 0 && info.denom != 0 => (info.numer, info.denom),
      _ => (1, 1),
    };

    // rusage_info_v6 (with the energy counter) needs macOS 13+, older systems get v4.
    let own_pid = std::process::id() as i32;
    let flavor =
      if rusage(own_pid, RUSAGE_INFO_V6).is_some() { RUSAGE_INFO_V6 } else { RUSAGE_INFO_V4 };

    Self { timebase, flavor, pids: Vec::new(), known: HashMap::new(), last: None }
  }

  /// CPU and power are rates since the previous call; the first call reports them as zero.
  pub fn sample(&mut self) -> Vec<ProcInfo> {
    let now = Instant::now();
    let elapsed_ns = self.last.map_or(0, |last| now.duration_since(last).as_nanos() as u64);
    self.last = Some(now);

    list_pids(&mut self.pids);
    let (numer, denom) = self.timebase;
    let mut known = HashMap::with_capacity(self.pids.len());
    let mut procs = Vec::with_capacity(self.pids.len());

    for &pid in &self.pids {
      let Some(info) = bsd_info(pid) else { continue };
      let Some(ru) = rusage(pid, self.flavor) else { continue };

      let counters = Counters {
        start: ru.ri_proc_start_abstime,
        cpu_ns: ticks_to_ns(ru.ri_user_time + ru.ri_system_time, numer, denom),
        energy_nj: (self.flavor == RUSAGE_INFO_V6).then_some(ru.ri_energy_nj),
      };

      let prev = self.known.remove(&pid);
      let usage = usage(prev.as_ref().map(|prev| &prev.counters), &counters, elapsed_ns);
      let comm = c_chars_to_string(&info.pbi_comm);
      let name = match prev {
        Some(prev) if prev.counters.start == counters.start && prev.comm == comm => prev.name,
        _ => process_name(pid, &info),
      };

      procs.push(ProcInfo {
        pid,
        ppid: info.pbi_ppid as i32,
        name: name.clone(),
        user: info.pbi_uid.to_string(),
        cpu_pct: usage.cpu_pct,
        mem_bytes: ru.ri_phys_footprint,
        power_w: usage.power_w,
        gpu_pct: 0.0,
      });
      known.insert(pid, Known { counters, comm, name });
    }

    self.known = known;
    procs
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::hint::black_box;
  use std::time::Duration;

  const SEC: u64 = 1_000_000_000;

  fn counters(cpu_ns: u64, energy_nj: Option<u64>) -> Counters {
    Counters { start: 42, cpu_ns, energy_nj }
  }

  #[test]
  fn rusage_layout_matches_sdk() {
    assert_eq!(mem::size_of::<rusage_info_v6>(), 464);
    assert_eq!(mem::offset_of!(rusage_info_v6, ri_flags), mem::size_of::<libc::rusage_info_v4>());
    assert_eq!(mem::offset_of!(rusage_info_v6, ri_energy_nj), 336);
  }

  #[test]
  fn one_busy_core_is_100_pct() {
    let usage = usage(Some(&counters(5 * SEC, None)), &counters(6 * SEC, None), SEC);
    assert_eq!(usage.cpu_pct, 100.0);
    assert_eq!(usage.power_w, None);

    let two_cores = super::usage(Some(&counters(0, None)), &counters(SEC, None), SEC / 2);
    assert_eq!(two_cores.cpu_pct, 200.0);
  }

  #[test]
  fn idle_is_zero() {
    let usage = usage(Some(&counters(SEC, Some(10))), &counters(SEC, Some(10)), SEC);
    assert_eq!(usage, Usage { cpu_pct: 0.0, power_w: Some(0.0) });
  }

  #[test]
  fn energy_to_watts() {
    let usage = usage(Some(&counters(0, Some(SEC))), &counters(0, Some(2 * SEC)), SEC);
    assert_eq!(usage.power_w, Some(1.0));

    let half = super::usage(Some(&counters(0, Some(0))), &counters(0, Some(SEC)), 2 * SEC);
    assert_eq!(half.power_w, Some(0.5));
  }

  #[test]
  fn negative_delta_is_zero() {
    let idle = Usage { cpu_pct: 0.0, power_w: Some(0.0) };
    let cpu_back = usage(Some(&counters(2 * SEC, Some(0))), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(cpu_back, idle);

    let energy_back = usage(Some(&counters(0, Some(2 * SEC))), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(energy_back, idle);
  }

  #[test]
  fn new_process_has_no_spike() {
    let cur = counters(100 * SEC, Some(100 * SEC));
    assert_eq!(usage(None, &cur, SEC), Usage { cpu_pct: 0.0, power_w: Some(0.0) });
    assert_eq!(usage(None, &counters(SEC, None), SEC), Usage::default());

    // Same pid, different start time: a new process reusing the pid.
    let reused = Counters { start: 43, ..cur };
    assert_eq!(usage(Some(&counters(0, Some(0))), &reused, SEC).cpu_pct, 0.0);
  }

  #[test]
  fn zero_elapsed_is_zero() {
    let usage = usage(Some(&counters(0, Some(0))), &counters(SEC, Some(SEC)), 0);
    assert_eq!(usage, Usage { cpu_pct: 0.0, power_w: Some(0.0) });
  }

  #[test]
  fn energy_counter_appearing_reads_zero() {
    let usage = usage(Some(&counters(0, None)), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(usage, Usage { cpu_pct: 100.0, power_w: Some(0.0) });
  }

  #[test]
  fn mach_ticks_to_ns() {
    assert_eq!(ticks_to_ns(24, 125, 3), 1000); // Apple Silicon timebase
    assert_eq!(ticks_to_ns(1000, 1, 1), 1000); // Intel / fallback timebase
    assert_eq!(ticks_to_ns(u64::MAX / 2, 125, 3), (u64::MAX as u128 / 2 * 125 / 3) as u64);
    assert_eq!(ticks_to_ns(5, 1, 0), 5);
  }

  #[test]
  fn c_strings_and_basenames() {
    let chars = |s: &[u8]| s.iter().map(|&b| b as c_char).collect::<Vec<_>>();
    assert_eq!(c_chars_to_string(&chars(b"zsh\0\0garbage")), "zsh");
    assert_eq!(c_chars_to_string(&chars(b"no-terminator")), "no-terminator");
    assert_eq!(c_chars_to_string(&chars(b"\0")), "");

    assert_eq!(basename("/Applications/Safari.app/Contents/MacOS/Safari"), Some("Safari"));
    assert_eq!(basename("/opt/Chrome Helper (GPU)"), Some("Chrome Helper (GPU)"));
    assert_eq!(basename("launchd"), Some("launchd"));
    assert_eq!(basename("/usr/bin/"), None);
    assert_eq!(basename(""), None);
  }

  #[test]
  fn samples_current_process() {
    let pid = std::process::id() as i32;
    let mut sampler = ProcSampler::new();

    let first = sampler.sample();
    let me = first.iter().find(|p| p.pid == pid).expect("own process is sampled");
    assert!(!me.name.is_empty());
    assert_eq!(me.ppid, unsafe { libc::getppid() });
    assert_eq!(me.user, unsafe { libc::geteuid() }.to_string());
    assert!(me.mem_bytes > 0);
    assert_eq!(me.cpu_pct, 0.0); // no baseline yet
    assert_eq!(sampler.known.len(), first.len());

    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(50) {
      black_box(started);
    }

    let second = sampler.sample();
    let me = second.iter().find(|p| p.pid == pid).expect("own process is sampled");
    assert!(me.cpu_pct > 0.0, "cpu_pct = {}", me.cpu_pct);
    assert!(me.power_w.is_none_or(|w| w >= 0.0));
    assert_eq!(sampler.known.len(), second.len());
  }
}
