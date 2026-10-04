//! Per-process resource usage (CPU, memory, energy) sampled without sudo.
//!
//! libproc reads processes of the current user (all of them as root). Other users' processes come
//! from the setuid `/bin/ps`, which has CPU time and RSS but no energy counter.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem;
use std::process::{Command, Stdio};
use std::time::Instant;

const RUSAGE_INFO_V4: c_int = 4;
const RUSAGE_INFO_V6: c_int = 6;

/// `ps` output columns; `comm` goes last as it may contain spaces.
const PS_COLUMNS: &str = "pid=,ppid=,uid=,rss=,time=,comm=";

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

/// Process state read from libproc or `ps`, before rates are computed.
#[derive(Debug, Clone, PartialEq)]
struct Raw {
  pid: i32,
  ppid: i32,
  uid: u32,
  mem_bytes: u64,
  counters: Counters,
  comm: String,     // Changes on exec; with the start time tells a reused pid apart.
  fallback: String, // Name shown when the executable path isn't readable.
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

/// Executable basename; the path is readable for processes of any user.
fn path_name(pid: i32) -> Option<String> {
  let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
  let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
  if len <= 0 {
    return None;
  }

  let path = String::from_utf8_lossy(&buf[..len as usize]);
  basename(&path).map(str::to_string)
}

/// Reads a process through libproc; fails for other users' processes unless running as root.
fn read_libproc(pid: i32, flavor: c_int, (numer, denom): (u32, u32)) -> Option<Raw> {
  let info = bsd_info(pid)?;
  let ru = rusage(pid, flavor)?;
  let comm = c_chars_to_string(&info.pbi_comm);
  let name = c_chars_to_string(&info.pbi_name);

  Some(Raw {
    pid,
    ppid: info.pbi_ppid as i32,
    uid: info.pbi_uid,
    mem_bytes: ru.ri_phys_footprint,
    counters: Counters {
      start: ru.ri_proc_start_abstime,
      cpu_ns: ticks_to_ns(ru.ri_user_time + ru.ri_system_time, numer, denom),
      energy_nj: (flavor == RUSAGE_INFO_V6).then_some(ru.ri_energy_nj),
    },
    fallback: if name.is_empty() { comm.clone() } else { name },
    comm,
  })
}

/// Number made of ASCII digits only (`str::parse` also accepts a leading `+`).
fn digits(s: &str) -> Option<u64> {
  if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  s.parse().ok()
}

/// CPU time from `ps -o time` in nanoseconds: `[[dd-]hh:]mm:ss[.ss]`. macOS prints `mm:ss.ss`
/// with minutes growing past 59, procps the `[dd-]hh:mm:ss` form.
fn parse_ps_time(s: &str) -> Option<u64> {
  let (days, hms) = match s.split_once('-') {
    Some((days, hms)) => (Some(digits(days)?), hms),
    None => (None, s),
  };

  let fields: Vec<&str> = hms.split(':').collect();
  let (hours, mins, secs) = match fields[..] {
    [mins, secs] if days.is_none() => (None, digits(mins)?, secs),
    [hours, mins, secs] => (Some(digits(hours)?), digits(mins)?, secs),
    _ => return None,
  };

  let (secs, frac_ns) = match secs.split_once('.') {
    Some((secs, frac)) if frac.len() <= 9 => {
      (secs, digits(frac)? * 10u64.pow(9 - frac.len() as u32))
    }
    Some(_) => return None,
    None => (secs, 0),
  };
  let secs = digits(secs)?;

  // Only the leading field may exceed its usual range.
  let hours_overflow = days.is_some() && hours.is_some_and(|hours| hours >= 24);
  if secs >= 60 || (hours.is_some() && mins >= 60) || hours_overflow {
    return None;
  }

  let total = days.unwrap_or(0).checked_mul(24)?.checked_add(hours.unwrap_or(0))?;
  let total = total.checked_mul(60)?.checked_add(mins)?.checked_mul(60)?.checked_add(secs)?;
  total.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

/// One line of `ps -o pid=,ppid=,uid=,rss=,time=,comm=`; the command is the rest of the line.
fn parse_ps_line(line: &str) -> Option<Raw> {
  let mut rest = line.trim();
  let mut field = || {
    let (field, tail) = rest.split_once(char::is_whitespace)?;
    rest = tail.trim_start();
    Some(field)
  };

  let pid = field()?.parse().ok()?;
  let ppid = field()?.parse().ok()?;
  let uid = field()?.parse().ok()?;
  let rss_kib = digits(field()?)?;
  let cpu_ns = parse_ps_time(field()?)?;
  let comm = rest.to_string();

  Some(Raw {
    pid,
    ppid,
    uid,
    mem_bytes: rss_kib.saturating_mul(1024),
    // ps has no start time, a reused pid is told apart by its command.
    counters: Counters { start: 0, cpu_ns, energy_nj: None },
    fallback: basename(&comm).unwrap_or(&comm).to_string(),
    comm,
  })
}

fn parse_ps(out: &str) -> Vec<Raw> {
  out.lines().filter_map(parse_ps_line).collect()
}

/// Processes of all users from `/bin/ps` (setuid root), without the `ps` process itself.
fn run_ps() -> Vec<Raw> {
  let child = Command::new("/bin/ps")
    .args(["-A", "-o", PS_COLUMNS])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn();
  let Ok(child) = child else { return Vec::new() };

  let ps_pid = child.id() as i32;
  let Ok(out) = child.wait_with_output() else { return Vec::new() };
  let mut rows = parse_ps(&String::from_utf8_lossy(&out.stdout));
  rows.retain(|raw| raw.pid != ps_pid);
  rows
}

/// Adds `ps` rows for processes libproc couldn't read; libproc rows win as they carry more data.
fn merge(mut rows: Vec<Raw>, ps: Vec<Raw>) -> Vec<Raw> {
  let seen: HashSet<i32> = rows.iter().map(|raw| raw.pid).collect();
  rows.extend(ps.into_iter().filter(|raw| !seen.contains(&raw.pid)));
  rows
}

/// User name of a uid, or the uid itself when it has no passwd entry.
fn user_name(uid: u32) -> String {
  let mut buf: Vec<c_char> = vec![0; 1024];
  loop {
    let mut pwd: libc::passwd = unsafe { mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let ret = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if ret == libc::ERANGE && buf.len() < 1 << 20 {
      buf.resize(buf.len() * 2, 0);
      continue;
    }

    if ret == 0 && !found.is_null() && !pwd.pw_name.is_null() {
      let name = unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy();
      if !name.is_empty() {
        return name.into_owned();
      }
    }
    return uid.to_string();
  }
}

/// A process seen on the previous tick.
struct Known {
  counters: Counters,
  comm: String, // Changes on exec, invalidates the cached `name`.
  name: String,
}

/// Samples every process: libproc for those it can read, `ps` for the rest.
pub struct ProcSampler {
  timebase: (u32, u32),
  flavor: c_int,
  use_ps: bool, // As root libproc reads every process.
  pids: Vec<i32>,
  known: HashMap<i32, Known>,
  users: HashMap<u32, String>,
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

    Self {
      timebase,
      flavor,
      use_ps: unsafe { libc::geteuid() } != 0,
      pids: Vec::new(),
      known: HashMap::new(),
      users: HashMap::new(),
      last: None,
    }
  }

  /// CPU and power are rates since the previous call; the first call reports them as zero.
  pub fn sample(&mut self) -> Vec<ProcInfo> {
    let now = Instant::now();
    let elapsed_ns = self.last.map_or(0, |last| now.duration_since(last).as_nanos() as u64);
    self.last = Some(now);

    list_pids(&mut self.pids);
    let mut rows = Vec::with_capacity(self.pids.len());
    let mut unreadable = false;
    for &pid in &self.pids {
      match read_libproc(pid, self.flavor, self.timebase) {
        Some(raw) => rows.push(raw),
        None => unreadable = true,
      }
    }

    if unreadable && self.use_ps {
      rows = merge(rows, run_ps());
    }
    self.update(rows, elapsed_ns)
  }

  /// Rates against the previous tick; a process is the same while its pid, start time and command
  /// stay the same.
  fn update(&mut self, rows: Vec<Raw>, elapsed_ns: u64) -> Vec<ProcInfo> {
    let mut known = HashMap::with_capacity(rows.len());
    let mut procs = Vec::with_capacity(rows.len());

    for raw in rows {
      let prev = self
        .known
        .remove(&raw.pid)
        .filter(|prev| prev.counters.start == raw.counters.start && prev.comm == raw.comm);
      let usage = usage(prev.as_ref().map(|prev| &prev.counters), &raw.counters, elapsed_ns);
      let name = match prev {
        Some(prev) => prev.name,
        None => path_name(raw.pid).unwrap_or(raw.fallback),
      };
      let user = self.users.entry(raw.uid).or_insert_with(|| user_name(raw.uid)).clone();

      procs.push(ProcInfo {
        pid: raw.pid,
        ppid: raw.ppid,
        name: name.clone(),
        user,
        cpu_pct: usage.cpu_pct,
        mem_bytes: raw.mem_bytes,
        power_w: usage.power_w,
        gpu_pct: 0.0,
      });
      known.insert(raw.pid, Known { counters: raw.counters, comm: raw.comm, name });
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
    assert_eq!(me.user, user_name(unsafe { libc::geteuid() }));
    assert!(me.mem_bytes > 0);
    assert_eq!(me.cpu_pct, 0.0); // no baseline yet
    assert_eq!(sampler.known.len(), first.len()); // pids are unique

    // launchd belongs to root: without root it's only readable through ps.
    let launchd = first.iter().find(|p| p.pid == 1).expect("launchd is sampled");
    assert_eq!(launchd.name, "launchd");
    assert_eq!(launchd.user, "root");
    assert!(launchd.mem_bytes > 0);
    if sampler.use_ps {
      assert_eq!(launchd.power_w, None);
    }

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

  #[test]
  fn ps_time_formats() {
    let ms = |ms: u64| Some(ms * 1_000_000);
    assert_eq!(parse_ps_time("0:00.07"), ms(70));
    assert_eq!(parse_ps_time("38:23.50"), ms((38 * 60 + 23) * 1000 + 500));
    assert_eq!(parse_ps_time("1234:56.78"), ms((1234 * 60 + 56) * 1000 + 780)); // macOS minutes
    assert_eq!(parse_ps_time("0:05"), ms(5000));
    assert_eq!(parse_ps_time("0:00.5"), ms(500));
    assert_eq!(parse_ps_time("0:00.123456789"), Some(123_456_789));
    assert_eq!(parse_ps_time("1:02:03"), ms(3723 * 1000));
    assert_eq!(parse_ps_time("25:00:00"), ms(25 * 3600 * 1000));
    assert_eq!(parse_ps_time("2-03:04:05"), ms((2 * 86400 + 3 * 3600 + 4 * 60 + 5) * 1000));
    assert_eq!(parse_ps_time("2-03:04:05.25"), ms((2 * 86400 + 11045) * 1000 + 250));
  }

  #[test]
  fn ps_time_malformed() {
    let bad = [
      "",
      "abc",
      "5",
      ":30",
      "1:",
      "1:2:3:4",
      "1:60.00",                 // seconds out of range
      "1:60:00",                 // minutes out of range after hours
      "1-24:00:00",              // hours out of range after days
      "1-02:03",                 // days need hours
      "-01:02:03",               // empty days
      "1:02.",                   // empty fraction
      "1:02.x",                  // bad fraction
      "1:02.1234567890",         // fraction past nanoseconds
      "+1:02",                   // sign
      "1: 02",                   // space
      "9999999999999999999:00",  // overflow
      "99999999999999999999:00", // doesn't fit u64
    ];
    for s in bad {
      assert_eq!(parse_ps_time(s), None, "{s:?}");
    }
  }

  #[test]
  fn ps_lines() {
    let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper (GPU)";
    let line = format!(" 2301  2002   501 182976   1:02.50 {chrome}");
    let raw = parse_ps_line(&line).expect("valid line");
    assert_eq!(raw.pid, 2301);
    assert_eq!(raw.ppid, 2002);
    assert_eq!(raw.uid, 501);
    assert_eq!(raw.mem_bytes, 182976 * 1024);
    assert_eq!(raw.counters, Counters { start: 0, cpu_ns: 62_500_000_000, energy_nj: None });
    assert_eq!(raw.comm, chrome);
    assert_eq!(raw.fallback, "Google Chrome Helper (GPU)");

    let raw = parse_ps_line("574\t1 0 2624 0:00.03 endpointsecurityd\n").expect("valid line");
    assert_eq!((raw.pid, raw.ppid, raw.uid), (574, 1, 0));
    assert_eq!(raw.fallback, "endpointsecurityd");

    let raw = parse_ps_line("1 0 0 100 0:00.01 my  daemon ").expect("valid line");
    assert_eq!(raw.comm, "my  daemon");
  }

  #[test]
  fn ps_lines_malformed() {
    let bad = [
      "",
      "   ",
      "garbage",
      "1 0 0 100 0:00.01",          // no command
      "x 0 0 100 0:00.01 cmd",      // pid
      "1 0 -1 100 0:00.01 cmd",     // uid
      "1 0 0 -100 0:00.01 cmd",     // rss
      "1 0 0 100 0:61.00 cmd",      // time
      "1 0 0 100 cmd",              // missing column
      "PID PPID UID RSS TIME COMM", // header
    ];
    for line in bad {
      assert_eq!(parse_ps_line(line), None, "{line:?}");
    }

    let out = "  1 0 0 10 0:01.00 /sbin/launchd\nbad line\n\n 88 1 88 20 0:02.00 /usr/sbin/a b\n";
    let pids: Vec<i32> = parse_ps(out).iter().map(|raw| raw.pid).collect();
    assert_eq!(pids, [1, 88]);
    assert!(parse_ps("").is_empty());
  }

  fn row(pid: i32, cpu_ns: u64, energy_nj: Option<u64>, comm: &str) -> Raw {
    Raw {
      pid,
      ppid: 1,
      uid: 0,
      mem_bytes: 1024,
      counters: Counters { start: 0, cpu_ns, energy_nj },
      comm: comm.to_string(),
      fallback: comm.to_string(),
    }
  }

  #[test]
  fn merge_prefers_libproc() {
    let libproc = vec![row(10, SEC, Some(SEC), "libproc"), row(11, 0, Some(0), "own")];
    let ps = vec![row(1, 0, None, "launchd"), row(10, 5 * SEC, None, "ps"), row(12, 0, None, "x")];
    let merged = merge(libproc.clone(), ps);

    let pids: Vec<i32> = merged.iter().map(|raw| raw.pid).collect();
    assert_eq!(pids, [10, 11, 1, 12]);
    assert_eq!(merged[..2], libproc[..]);

    assert_eq!(merge(Vec::new(), vec![row(1, 0, None, "a")]).len(), 1);
    assert_eq!(merge(libproc.clone(), Vec::new()), libproc);
  }

  #[test]
  fn update_rates_names_and_users() {
    // pids above the macOS limit (99999) don't exist, so names come from the fallback.
    const A: i32 = 1_000_001;
    const B: i32 = 1_000_002;
    let mut sampler = ProcSampler::new();

    let first = sampler.update(vec![row(A, SEC, None, "a"), row(B, 0, Some(0), "b")], 0);
    assert_eq!(first.len(), 2);
    assert_eq!((first[0].cpu_pct, first[0].power_w), (0.0, None));
    assert_eq!((first[1].cpu_pct, first[1].power_w), (0.0, Some(0.0)));
    assert_eq!(first[0].name, "a");
    assert_eq!(first[0].user, "root");
    assert_eq!(first[0].mem_bytes, 1024);

    let second = sampler.update(vec![row(A, 2 * SEC, None, "a"), row(B, SEC, Some(SEC), "b")], SEC);
    assert_eq!((second[0].cpu_pct, second[0].power_w), (100.0, None)); // ps row: no power
    assert_eq!((second[1].cpu_pct, second[1].power_w), (100.0, Some(1.0)));

    // A new command under the same pid is a new process: no spike, fresh name.
    let third = sampler.update(vec![row(A, 9 * SEC, None, "c")], SEC);
    assert_eq!(third[0].cpu_pct, 0.0);
    assert_eq!(third[0].name, "c");
    assert_eq!(sampler.known.len(), 1); // gone processes are forgotten
    assert_eq!(sampler.users.get(&0).map(String::as_str), Some("root"));
  }

  #[test]
  fn user_names() {
    assert_eq!(user_name(0), "root");
    assert_eq!(user_name(1_999_999_999), "1999999999");
  }
}
