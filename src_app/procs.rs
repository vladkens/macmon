//! Per-process resource usage (CPU, memory, energy) sampled without sudo.
//!
//! libproc reads processes of the current user (all of them as root). Other users' processes come
//! from the setuid `/bin/ps`, which has CPU time and RSS but no energy counter. GPU time of every
//! process comes from the GPU's user clients in the IORegistry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem;
use std::process::{Command, Stdio};
use std::time::Instant;

use core_foundation::array::CFArray;
use core_foundation::base::{CFAllocatorRef, CFType, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

const RUSAGE_INFO_V4: c_int = 4;
const RUSAGE_INFO_V6: c_int = 6;

/// `ps` output columns; `comm` goes last as it may contain spaces.
const PS_COLUMNS: &str = "pid=,uid=,rss=,time=,comm=";
/// `ps` prints CPU time in 10 ms steps, 1 % of a one-second interval: CPU % of `ps` rows is
/// averaged over this many intervals, so idle processes don't jump between 0 % and 1 %.
const PS_CPU_INTERVALS: usize = 3;

/// `struct rusage_info_v6` from the macOS SDK `sys/resource.h`. `rusage_info_v4` is a prefix of
/// it, so the same buffer serves both flavors.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)] // FFI layout, most fields are unused
struct rusage_info_v6 {
  ri_uuid: [u8; 16],
  ri_user_time: u64,
  ri_system_time: u64,
  ri_pkg_idle_wkups: u64,
  ri_interrupt_wkups: u64,
  ri_pageins: u64,
  ri_wired_size: u64,
  ri_resident_size: u64,
  ri_phys_footprint: u64,
  ri_proc_start_abstime: u64,
  ri_proc_exit_abstime: u64,
  ri_child_user_time: u64,
  ri_child_system_time: u64,
  ri_child_pkg_idle_wkups: u64,
  ri_child_interrupt_wkups: u64,
  ri_child_pageins: u64,
  ri_child_elapsed_abstime: u64,
  ri_diskio_bytesread: u64,
  ri_diskio_byteswritten: u64,
  ri_cpu_time_qos_default: u64,
  ri_cpu_time_qos_maintenance: u64,
  ri_cpu_time_qos_background: u64,
  ri_cpu_time_qos_utility: u64,
  ri_cpu_time_qos_legacy: u64,
  ri_cpu_time_qos_user_initiated: u64,
  ri_cpu_time_qos_user_interactive: u64,
  ri_billed_system_time: u64,
  ri_serviced_system_time: u64,
  ri_logical_writes: u64,
  ri_lifetime_max_phys_footprint: u64,
  ri_instructions: u64,
  ri_cycles: u64,
  ri_billed_energy: u64,
  ri_serviced_energy: u64,
  ri_interval_max_phys_footprint: u64,
  ri_runnable_time: u64,
  ri_flags: u64,
  ri_user_ptime: u64,
  ri_system_ptime: u64,
  ri_pinstructions: u64,
  ri_pcycles: u64,
  ri_energy_nj: u64,
  ri_penergy_nj: u64,
  ri_secure_time_in_system: u64,
  ri_secure_ptime_in_system: u64,
  ri_neural_footprint: u64,
  ri_lifetime_max_neural_footprint: u64,
  ri_interval_max_neural_footprint: u64,
  ri_conclave_footprint: u64,
  ri_page_wait_time_mach: u64,
  ri_page_cache_hits: u64,
  ri_reserved: [u64; 6],
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

#[link(name = "IOKit", kind = "framework")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOServiceMatching(name: *const c_char) -> CFDictionaryRef;
  fn IOServiceGetMatchingServices(main_port: u32, matching: CFDictionaryRef, existing: *mut u32) -> c_int;
  fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iterator: *mut u32) -> c_int;
  fn IORegistryEntryCreateCFProperty(entry: u32, key: CFStringRef, allocator: CFAllocatorRef, options: u32) -> CFTypeRef;
  fn IOIteratorNext(iterator: u32) -> u32;
  fn IOIteratorIsValid(iterator: u32) -> c_int;
  fn IOObjectRelease(object: u32) -> u32;
}

/// One row of the process list.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
  pub pid: i32,
  pub name: String,
  /// Executable path; empty when it isn't readable.
  pub path: String,
  pub user: String,
  /// 100% = one fully busy core, as in Activity Monitor.
  pub cpu_pct: f32,
  /// Physical footprint (Activity Monitor's "Memory") of the processes libproc reads: the current
  /// user's, every process as root. Other users' processes come from `ps`, which has only the
  /// resident size: it counts shared pages and leaves out compressed memory, so the two compare
  /// only roughly.
  pub mem_bytes: u64,
  /// `None` when the energy counter isn't readable: other users' processes without root, and
  /// every process before macOS 13 (no `rusage_info_v6`).
  pub power_w: Option<f32>,
  /// Share of the interval the GPU spent on the process, 0..=100.
  pub gpu_pct: f32,
}

/// Cumulative counters of one process; rates come from two snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Counters {
  /// Process start time, tells a reused pid apart.
  start: u64,
  /// User + system CPU time.
  cpu_ns: u64,
  /// Lifetime energy, None when unavailable.
  energy_nj: Option<u64>,
  /// GPU time of the process' GPU clients, None without clients.
  gpu_ns: Option<u64>,
}

/// Process state read from libproc or `ps`, before rates are computed.
#[derive(Debug, Clone, PartialEq)]
struct Raw {
  pid: i32,
  uid: u32,
  mem_bytes: u64,
  counters: Counters,
  /// Changes on exec; with the start time tells a reused pid apart.
  comm: String,
  /// Name shown when the executable path isn't readable.
  fallback: String,
  /// Read from `ps`: CPU time in 10 ms steps.
  ps: bool,
}

// MARK: Rates

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Usage {
  cpu_pct: f32,
  power_w: Option<f32>,
  gpu_pct: f32,
}

/// GPU share from GPU time snapshots `elapsed_ns` apart. Missing time on either side (no GPU
/// clients, unreadable IORegistry) and time going backwards (a client closed) read as idle.
fn gpu_pct(prev: Option<u64>, cur: Option<u64>, elapsed_ns: u64) -> f32 {
  match (prev, cur) {
    (Some(prev), Some(cur)) if cur > prev && elapsed_ns > 0 => {
      ((cur - prev) as f64 / elapsed_ns as f64 * 100.0).min(100.0) as f32
    }
    _ => 0.0,
  }
}

/// Rates between two snapshots taken `elapsed_ns` apart. A first sample, a reused pid (different
/// start time) or a counter going backwards reads as idle instead of a spike.
fn usage(prev: Option<&Counters>, cur: &Counters, elapsed_ns: u64) -> Usage {
  let idle = Usage { cpu_pct: 0.0, power_w: cur.energy_nj.map(|_| 0.0), gpu_pct: 0.0 };
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

  let gpu_pct = gpu_pct(prev.gpu_ns, cur.gpu_ns, elapsed_ns);
  let elapsed_ns = elapsed_ns as f64;
  Usage {
    cpu_pct: ((cur.cpu_ns - prev.cpu_ns) as f64 / elapsed_ns * 100.0) as f32,
    power_w: energy_nj.map(|nj| (nj as f64 / elapsed_ns) as f32), // nJ per ns = W
    gpu_pct,
  }
}

// MARK: libproc

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

/// Executable path; readable for processes of any user.
fn exe_path(pid: i32) -> Option<String> {
  let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
  let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
  if len <= 0 {
    return None;
  }

  Some(String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

/// Reads a process through libproc; fails for other users' processes unless running as root.
fn read_libproc(pid: i32, flavor: c_int, (numer, denom): (u32, u32)) -> Option<Raw> {
  let info = bsd_info(pid)?;
  let ru = rusage(pid, flavor)?;
  let comm = c_chars_to_string(&info.pbi_comm);
  let name = c_chars_to_string(&info.pbi_name);

  Some(Raw {
    pid,
    uid: info.pbi_uid,
    mem_bytes: ru.ri_phys_footprint,
    counters: Counters {
      start: ru.ri_proc_start_abstime,
      cpu_ns: ticks_to_ns(ru.ri_user_time + ru.ri_system_time, numer, denom),
      energy_nj: (flavor == RUSAGE_INFO_V6).then_some(ru.ri_energy_nj),
      gpu_ns: None,
    },
    fallback: if name.is_empty() { comm.clone() } else { name },
    comm,
    ps: false,
  })
}

// MARK: ps

/// Number made of ASCII digits only (`str::parse` also accepts a leading `+`).
fn digits(s: &str) -> Option<u64> {
  if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  s.parse().ok()
}

/// CPU time from macOS `ps -o time` in nanoseconds: `mm:ss.ss`, the minutes growing past 59.
fn parse_ps_time(s: &str) -> Option<u64> {
  let (mins, secs) = s.split_once(':')?;
  let (secs, frac_ns) = match secs.split_once('.') {
    Some((secs, frac)) if frac.len() <= 9 => {
      (secs, digits(frac)? * 10u64.pow(9 - frac.len() as u32))
    }
    Some(_) => return None,
    None => (secs, 0),
  };

  let (mins, secs) = (digits(mins)?, digits(secs)?);
  if secs >= 60 {
    return None;
  }

  let total = mins.checked_mul(60)?.checked_add(secs)?;
  total.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

/// One line of `ps -o pid=,uid=,rss=,time=,comm=`; the command is the rest of the line.
fn parse_ps_line(line: &str) -> Option<Raw> {
  let mut rest = line.trim();
  let mut field = || {
    let (field, tail) = rest.split_once(char::is_whitespace)?;
    rest = tail.trim_start();
    Some(field)
  };

  let pid = field()?.parse().ok()?;
  let uid = field()?.parse().ok()?;
  let rss_kib = digits(field()?)?;
  let cpu_ns = parse_ps_time(field()?)?;
  let comm = rest.to_string();

  Some(Raw {
    pid,
    uid,
    mem_bytes: rss_kib.saturating_mul(1024),
    // ps has no start time, a reused pid is told apart by its command.
    counters: Counters { start: 0, cpu_ns, energy_nj: None, gpu_ns: None },
    fallback: basename(&comm).unwrap_or(&comm).to_string(),
    comm,
    ps: true,
  })
}

fn parse_ps(out: &str) -> Vec<Raw> {
  out.lines().filter_map(parse_ps_line).collect()
}

/// Processes of all users from `/bin/ps` (setuid root), without the `ps` process itself.
fn run_ps() -> Vec<Raw> {
  ps_rows(Command::new("/bin/ps").args(["-A", "-o", PS_COLUMNS]))
}

/// Rows `cmd` prints in the `ps` format, without its own process; none when it can't run.
fn ps_rows(cmd: &mut Command) -> Vec<Raw> {
  let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
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

// MARK: Users

/// User name of a uid, or the uid itself when it has no passwd entry.
fn user_name(uid: u32) -> String {
  user_name_with(uid, 1024)
}

/// `user_name` starting with a `buf_len` byte buffer, grown as `getpwuid_r` asks for it.
fn user_name_with(uid: u32, buf_len: usize) -> String {
  let mut buf: Vec<c_char> = vec![0; buf_len.max(1)];
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

// MARK: GPU

/// IOKit object handle, released on drop.
struct IoObject(u32);

impl Drop for IoObject {
  fn drop(&mut self) {
    if self.0 != 0 {
      unsafe { IOObjectRelease(self.0) };
    }
  }
}

impl IoObject {
  /// Copy of a registry entry property, None when it's missing.
  fn property(&self, key: &CFString) -> Option<CFType> {
    let key = key.as_concrete_TypeRef();
    let value = unsafe { IORegistryEntryCreateCFProperty(self.0, key, kCFAllocatorDefault, 0) };
    (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
  }
}

/// IOKit iterator yielding owned objects.
struct IoIter(IoObject);

impl IoIter {
  /// False when the registry changed during iteration and objects may have been skipped.
  fn is_valid(&self) -> bool {
    unsafe { IOIteratorIsValid(self.0.0) != 0 }
  }
}

impl Iterator for IoIter {
  type Item = IoObject;

  fn next(&mut self) -> Option<IoObject> {
    let next = unsafe { IOIteratorNext(self.0.0) };
    (next != 0).then_some(IoObject(next))
  }
}

/// Services of an IOKit class, its subclasses included.
fn matching_services(class: &CStr) -> Option<IoIter> {
  let matching = unsafe { IOServiceMatching(class.as_ptr()) };
  if matching.is_null() {
    return None;
  }

  let mut iter = 0;
  // Takes ownership of `matching`.
  let ret = unsafe { IOServiceGetMatchingServices(0, matching, &mut iter) };
  (ret == 0).then_some(IoIter(IoObject(iter)))
}

/// Children of a registry entry in the service plane.
fn children(entry: &IoObject) -> Option<IoIter> {
  let mut iter = 0;
  let ret = unsafe { IORegistryEntryGetChildIterator(entry.0, c"IOService".as_ptr(), &mut iter) };
  (ret == 0).then_some(IoIter(IoObject(iter)))
}

/// Pid from a user client's `IOUserClientCreator`: `"pid 631, WindowServer"`.
fn parse_creator(creator: &str) -> Option<i32> {
  let rest = creator.strip_prefix("pid ")?;
  let pid = rest.split_once(',').map_or(rest, |(pid, _)| pid);
  digits(pid).and_then(|pid| i32::try_from(pid).ok())
}

/// Total `accumulatedGPUTime` (ns) of a client's `AppUsage`, an array with one dict per command
/// queue. Entries without a non-negative number are skipped.
fn app_usage_ns(usage: &CFType) -> u64 {
  let Some(entries) = usage.downcast::<CFArray>() else { return 0 };
  let key = CFString::from_static_string("accumulatedGPUTime");

  let entry_ns = |entry: *const c_void| {
    let entry = (!entry.is_null()).then(|| unsafe { CFType::wrap_under_get_rule(entry) })?;
    let entry = entry.downcast_into::<CFDictionary>()?;
    let value = *entry.find(key.as_CFTypeRef())?;
    let value = (!value.is_null()).then(|| unsafe { CFType::wrap_under_get_rule(value) })?;
    u64::try_from(value.downcast_into::<CFNumber>()?.to_i64()?).ok()
  };
  entries.iter().filter_map(|entry| entry_ns(*entry)).fold(0, u64::saturating_add)
}

/// `(pid, GPU time in ns)` of every GPU user client. The clients are unregistered children of
/// the `IOAccelerator` services, so they're only found by walking the service plane.
fn read_gpu_clients() -> Option<Vec<(i32, u64)>> {
  let creator_key = CFString::from_static_string("IOUserClientCreator");
  let usage_key = CFString::from_static_string("AppUsage");
  let mut clients = Vec::new();

  for gpu in matching_services(c"IOAccelerator")? {
    // Clients come and go; a registry change mid-walk invalidates the iterator, so walk again.
    walk_until_consistent(&mut clients, 3, |clients| {
      let mut iter = children(&gpu)?;
      for client in iter.by_ref() {
        let creator = client.property(&creator_key).and_then(|v| v.downcast_into::<CFString>());
        let Some(pid) = creator.and_then(|creator| parse_creator(&creator.to_string())) else {
          continue;
        };
        let gpu_ns = client.property(&usage_key).map_or(0, |usage| app_usage_ns(&usage));
        clients.push((pid, gpu_ns));
      }
      Some(iter.is_valid())
    })?;
  }

  Some(clients)
}

/// Runs `walk` (it adds items to `items` and returns whether the walk was consistent) up to
/// `attempts` times, until a walk is consistent; the items of an inconsistent walk are dropped,
/// except the last one's. `None` from `walk` stops it.
fn walk_until_consistent<T>(
  items: &mut Vec<T>,
  attempts: usize,
  mut walk: impl FnMut(&mut Vec<T>) -> Option<bool>,
) -> Option<()> {
  for attempt in 1..=attempts {
    let start = items.len();
    if walk(items)? || attempt == attempts {
      break;
    }
    items.truncate(start);
  }
  Some(())
}

/// GPU time per pid; a process may own several clients.
fn sum_gpu_times(clients: impl IntoIterator<Item = (i32, u64)>) -> HashMap<i32, u64> {
  let mut times = HashMap::new();
  for (pid, gpu_ns) in clients {
    let total: &mut u64 = times.entry(pid).or_default();
    *total = total.saturating_add(gpu_ns);
  }
  times
}

/// GPU time per pid, empty when the IORegistry isn't readable.
fn gpu_times() -> HashMap<i32, u64> {
  read_gpu_clients().map(sum_gpu_times).unwrap_or_default()
}

// MARK: Sampler

/// CPU % of a `ps` row from its CPU time `cpu_ns` at `now_ns` and its earlier `(time, CPU time)`
/// snapshots, oldest first: the average since the oldest one. Zero without a snapshot, and over
/// the short warm-up of a new sampler (from its first sample, at time 0, to the next), which is
/// skipped; a process that shows up later gets a rate on its second sample, as libproc rows do.
fn averaged_cpu_pct(history: &[(u64, u64)], now_ns: u64, cpu_ns: u64) -> f32 {
  match history {
    [(0, _)] => 0.0,
    [(time, cpu), ..] if now_ns > *time && cpu_ns >= *cpu => {
      ((cpu_ns - cpu) as f64 / (now_ns - time) as f64 * 100.0) as f32
    }
    _ => 0.0,
  }
}

/// A process seen on the previous tick.
struct Known {
  counters: Counters,
  /// Changes on exec, invalidates the cached `name` and `path`.
  comm: String,
  name: String,
  path: String,
  /// `ps` rows: `(time, CPU time)` of the last `PS_CPU_INTERVALS` ticks, oldest first.
  cpu_history: VecDeque<(u64, u64)>,
}

/// Samples every process: libproc for those it can read, `ps` for the rest.
pub struct ProcSampler {
  timebase: (u32, u32),
  flavor: c_int,
  /// Not as root: libproc reads every process then.
  use_ps: bool,
  pids: Vec<i32>,
  known: HashMap<i32, Known>,
  users: HashMap<u32, String>,
  last: Option<Instant>,
  /// Time since the first sample, in ns.
  clock_ns: u64,
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
      clock_ns: 0,
    }
  }

  /// CPU and power are rates since the previous call; the first call reports them as zero.
  pub fn sample(&mut self) -> Vec<ProcInfo> {
    let now = Instant::now();
    let elapsed_ns = self.last.map_or(0, |last| now.duration_since(last).as_nanos() as u64);
    self.last = Some(now);
    let gpu = gpu_times(); // Read next to `now`: the counters carry no timestamp of their own.

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
    for raw in &mut rows {
      raw.counters.gpu_ns = gpu.get(&raw.pid).copied();
    }
    self.update(rows, elapsed_ns)
  }

  /// Rates against the previous tick (CPU of `ps` rows over the last `PS_CPU_INTERVALS` ticks);
  /// a process is the same while its pid, start time and command stay the same.
  fn update(&mut self, rows: Vec<Raw>, elapsed_ns: u64) -> Vec<ProcInfo> {
    let mut known = HashMap::with_capacity(rows.len());
    let mut procs = Vec::with_capacity(rows.len());
    self.clock_ns += elapsed_ns;

    for raw in rows {
      let prev = self
        .known
        .remove(&raw.pid)
        .filter(|prev| prev.counters.start == raw.counters.start && prev.comm == raw.comm);
      let mut usage = usage(prev.as_ref().map(|prev| &prev.counters), &raw.counters, elapsed_ns);
      let (name, path, mut cpu_history) = match prev {
        Some(prev) => (prev.name, prev.path, prev.cpu_history),
        None => {
          let path = exe_path(raw.pid);
          let name = path.as_deref().and_then(basename).map(str::to_string);
          (name.unwrap_or(raw.fallback), path.unwrap_or_default(), VecDeque::new())
        }
      };
      if raw.ps {
        let cpu_ns = raw.counters.cpu_ns;
        usage.cpu_pct = averaged_cpu_pct(cpu_history.make_contiguous(), self.clock_ns, cpu_ns);
        cpu_history.push_back((self.clock_ns, cpu_ns));
        if cpu_history.len() > PS_CPU_INTERVALS {
          cpu_history.pop_front();
        }
      }
      let user = self.users.entry(raw.uid).or_insert_with(|| user_name(raw.uid)).clone();

      procs.push(ProcInfo {
        pid: raw.pid,
        name: name.clone(),
        path: path.clone(),
        user,
        cpu_pct: usage.cpu_pct,
        mem_bytes: raw.mem_bytes,
        power_w: usage.power_w,
        gpu_pct: usage.gpu_pct,
      });
      let counters = raw.counters;
      known.insert(raw.pid, Known { counters, comm: raw.comm, name, path, cpu_history });
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
  /// Pids of made-up processes.
  const A: i32 = 1_000_001;
  const B: i32 = 1_000_002;

  fn counters(cpu_ns: u64, energy_nj: Option<u64>) -> Counters {
    Counters { start: 42, cpu_ns, energy_nj, gpu_ns: None }
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
    assert_eq!(usage, Usage { cpu_pct: 0.0, power_w: Some(0.0), gpu_pct: 0.0 });
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
    let idle = Usage { cpu_pct: 0.0, power_w: Some(0.0), gpu_pct: 0.0 };
    let cpu_back = usage(Some(&counters(2 * SEC, Some(0))), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(cpu_back, idle);

    let energy_back = usage(Some(&counters(0, Some(2 * SEC))), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(energy_back, idle);
  }

  #[test]
  fn new_process_has_no_spike() {
    let cur = counters(100 * SEC, Some(100 * SEC));
    assert_eq!(usage(None, &cur, SEC), Usage { cpu_pct: 0.0, power_w: Some(0.0), gpu_pct: 0.0 });
    assert_eq!(usage(None, &counters(SEC, None), SEC), Usage::default());

    // Same pid, different start time: a new process reusing the pid.
    let reused = Counters { start: 43, ..cur };
    assert_eq!(usage(Some(&counters(0, Some(0))), &reused, SEC).cpu_pct, 0.0);
  }

  #[test]
  fn zero_elapsed_is_zero() {
    let usage = usage(Some(&counters(0, Some(0))), &counters(SEC, Some(SEC)), 0);
    assert_eq!(usage, Usage { cpu_pct: 0.0, power_w: Some(0.0), gpu_pct: 0.0 });
  }

  #[test]
  fn energy_counter_appearing_reads_zero() {
    let usage = usage(Some(&counters(0, None)), &counters(SEC, Some(SEC)), SEC);
    assert_eq!(usage, Usage { cpu_pct: 100.0, power_w: Some(0.0), gpu_pct: 0.0 });
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
    let exe = std::env::current_exe().unwrap();
    assert_eq!(me.path, exe.to_string_lossy(), "the full executable path");
    assert!(me.path.ends_with(&format!("/{}", me.name)));
    assert_eq!(me.user, user_name(unsafe { libc::geteuid() }));
    assert!(me.mem_bytes > 0);
    assert_eq!(me.cpu_pct, 0.0); // no baseline yet
    assert_eq!(sampler.known.len(), first.len()); // pids are unique

    // launchd belongs to root: without root it's only readable through ps.
    let launchd = first.iter().find(|p| p.pid == 1).expect("launchd is sampled");
    assert_eq!((launchd.name.as_str(), launchd.path.as_str()), ("launchd", "/sbin/launchd"));
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
    assert!(second.iter().all(|p| (0.0..=100.0).contains(&p.gpu_pct)));
    assert_eq!(sampler.known.len(), second.len());
  }

  #[test]
  fn ps_time_formats() {
    let ms = |ms: u64| Some(ms * 1_000_000);
    assert_eq!(parse_ps_time("0:00.07"), ms(70));
    assert_eq!(parse_ps_time("38:23.50"), ms((38 * 60 + 23) * 1000 + 500));
    // minutes grow past 59: macOS never prints hours or days
    assert_eq!(parse_ps_time("1234:56.78"), ms((1234 * 60 + 56) * 1000 + 780));
    assert_eq!(parse_ps_time("0:05"), ms(5000));
    assert_eq!(parse_ps_time("0:00.5"), ms(500));
    assert_eq!(parse_ps_time("0:00.123456789"), Some(123_456_789));
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
      "1:02:03",                 // hours: procps, not macOS
      "2-03:04:05",              // days: procps, not macOS
      "-01:02",                  // sign
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
    let line = format!(" 2301   501 182976   1:02.50 {chrome}");
    let raw = parse_ps_line(&line).expect("valid line");
    assert_eq!(raw.pid, 2301);
    assert_eq!(raw.uid, 501);
    assert_eq!(raw.mem_bytes, 182976 * 1024);
    assert_eq!(
      raw.counters,
      Counters { start: 0, cpu_ns: 62_500_000_000, energy_nj: None, gpu_ns: None }
    );
    assert_eq!(raw.comm, chrome);
    assert_eq!(raw.fallback, "Google Chrome Helper (GPU)");
    assert!(raw.ps);

    let raw = parse_ps_line("574\t0 2624 0:00.03 endpointsecurityd\n").expect("valid line");
    assert_eq!((raw.pid, raw.uid), (574, 0));
    assert_eq!(raw.fallback, "endpointsecurityd");

    let raw = parse_ps_line("1 0 100 0:00.01 my  daemon ").expect("valid line");
    assert_eq!(raw.comm, "my  daemon");
  }

  #[test]
  fn ps_lines_malformed() {
    let bad = [
      "",
      "   ",
      "garbage",
      "1 0 100 0:00.01",       // no command
      "x 0 100 0:00.01 cmd",   // pid
      "1 -1 100 0:00.01 cmd",  // uid
      "1 0 -100 0:00.01 cmd",  // rss
      "1 0 100 0:61.00 cmd",   // time
      "1 0 100 cmd",           // missing column
      "PID UID RSS TIME COMM", // header
    ];
    for line in bad {
      assert_eq!(parse_ps_line(line), None, "{line:?}");
    }

    let out = "  1 0 10 0:01.00 /sbin/launchd\nbad line\n\n 88 88 20 0:02.00 /usr/sbin/a b\n";
    let pids: Vec<i32> = parse_ps(out).iter().map(|raw| raw.pid).collect();
    assert_eq!(pids, [1, 88]);
    assert!(parse_ps("").is_empty());
  }

  fn row(pid: i32, cpu_ns: u64, energy_nj: Option<u64>, comm: &str) -> Raw {
    Raw {
      pid,
      uid: 0,
      mem_bytes: 1024,
      counters: Counters { start: 0, cpu_ns, energy_nj, gpu_ns: None },
      comm: comm.to_string(),
      fallback: comm.to_string(),
      ps: false,
    }
  }

  fn ps_row(pid: i32, cpu_ns: u64, comm: &str) -> Raw {
    Raw { ps: true, ..row(pid, cpu_ns, None, comm) }
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
    let mut sampler = ProcSampler::new();

    let first = sampler.update(vec![row(A, SEC, None, "a"), row(B, 0, Some(0), "b")], 0);
    assert_eq!(first.len(), 2);
    assert_eq!((first[0].cpu_pct, first[0].power_w), (0.0, None));
    assert_eq!((first[1].cpu_pct, first[1].power_w), (0.0, Some(0.0)));
    assert_eq!((first[0].name.as_str(), first[0].path.as_str()), ("a", ""));
    assert_eq!(first[0].user, "root");
    assert_eq!(first[0].mem_bytes, 1024);

    let second = sampler.update(vec![row(A, 2 * SEC, None, "a"), row(B, SEC, Some(SEC), "b")], SEC);
    assert_eq!((second[0].cpu_pct, second[0].power_w), (100.0, None)); // no energy counter
    assert_eq!((second[1].cpu_pct, second[1].power_w), (100.0, Some(1.0)));

    // A new command under the same pid is a new process: no spike, fresh name.
    let third = sampler.update(vec![row(A, 9 * SEC, None, "c")], SEC);
    assert_eq!(third[0].cpu_pct, 0.0);
    assert_eq!(third[0].name, "c");
    assert_eq!(sampler.known.len(), 1); // gone processes are forgotten
    assert_eq!(sampler.users.get(&0).map(String::as_str), Some("root"));
  }

  #[test]
  fn ps_rows_average_cpu_over_three_intervals() {
    // pids above the macOS limit (99999) don't exist, so names come from the fallback.
    let mut sampler = ProcSampler::new();
    // (elapsed, CPU time of A from ps, CPU time of B from libproc), all in 10 ms
    let ticks = [(0, 0, 0), (25, 1, 1), (100, 1, 2), (100, 2, 4), (100, 2, 6), (100, 6, 8)];
    let ms = |tens: u64| tens * 10_000_000;
    let cpu: Vec<(f32, f32)> = ticks
      .iter()
      .map(|&(elapsed, a, b)| {
        let rows = vec![ps_row(A, ms(a), "a"), row(B, ms(b), Some(0), "b")];
        let procs = sampler.update(rows, ms(elapsed));
        (procs[0].cpu_pct, procs[1].cpu_pct)
      })
      .collect();

    // libproc: every interval on its own; ps: nothing for the baseline and the 250 ms warm-up
    // (a 10 ms step there reads 4 %), then the average since up to 3 intervals back
    let pct = |tens: u64, over: u64| (tens as f64 / over as f64 * 100.0) as f32;
    let expected =
      [(0.0, 0.0), (0.0, 4.0), (pct(1, 125), 1.0), (pct(2, 225), 2.0), (pct(1, 300), 2.0)];
    assert_eq!(cpu[..5], expected);
    assert_eq!(cpu[5].0, pct(5, 300), "the oldest interval left the average");

    // a reused pid starts over
    let procs = sampler.update(vec![ps_row(A, ms(1), "other")], ms(100));
    assert_eq!(procs[0].cpu_pct, 0.0);
    assert_eq!(sampler.known[&A].cpu_history, [(ms(525), ms(1))]);
    assert!(!sampler.known.contains_key(&B));
    // and, past the warm-up, gets a rate on its second sample, as a libproc row does
    let procs = sampler.update(vec![ps_row(A, ms(3), "other")], ms(100));
    assert_eq!(procs[0].cpu_pct, pct(2, 100));
  }

  #[test]
  fn averaged_cpu_cases() {
    let history = [(0, 0), (SEC, SEC / 2), (2 * SEC, SEC)];
    assert_eq!(averaged_cpu_pct(&history, 3 * SEC, 3 * SEC / 2), 50.0);
    assert_eq!(averaged_cpu_pct(&history[..2], 2 * SEC, SEC), 50.0);
    // one snapshot taken after the sampler's first sample: the rate since it
    assert_eq!(averaged_cpu_pct(&history[1..2], 2 * SEC, SEC), 50.0);
    // only the first sample's snapshot (the warm-up) or none: zero
    assert_eq!(averaged_cpu_pct(&history[..1], SEC, SEC), 0.0);
    assert_eq!(averaged_cpu_pct(&[], SEC, SEC), 0.0);
    // the counter going backwards or no time passed: zero, no spike
    assert_eq!(averaged_cpu_pct(&[(0, SEC), (1, SEC)], SEC, 0), 0.0);
    assert_eq!(averaged_cpu_pct(&[(SEC, 5), (SEC, 6)], SEC, SEC), 0.0);
  }

  #[test]
  fn user_names() {
    assert_eq!(user_name(0), "root");
    assert_eq!(user_name(1_999_999_999), "1999999999");
    // a buffer too small for the passwd entry grows
    assert_eq!(user_name_with(0, 1), "root");
    assert_eq!(user_name_with(0, 0), "root");
  }

  #[test]
  fn ps_rows_skip_their_own_process() {
    // a stand-in for ps that prints its own pid (`$$`) next to another process
    let script = "echo \"$$ 0 100 0:00.01 sh\"; echo '42 0 200 0:00.02 other'";
    let rows = ps_rows(Command::new("/bin/sh").args(["-c", script]));
    let pids: Vec<i32> = rows.iter().map(|raw| raw.pid).collect();
    assert_eq!(pids, [42]);

    // a ps that can't run: no rows
    assert!(ps_rows(&mut Command::new("/nonexistent/ps")).is_empty());
    // a ps that fails: whatever it printed
    assert!(ps_rows(Command::new("/bin/sh").args(["-c", "exit 1"])).is_empty());
  }

  #[test]
  fn walk_retries_until_consistent() {
    // (items, consistent) of each walk
    let run = |walks: Vec<(Vec<u32>, bool)>| {
      let mut walks = walks.into_iter();
      let mut count = 0;
      let mut items = vec![7];
      let result = walk_until_consistent(&mut items, 3, |items| {
        count += 1;
        let (found, consistent) = walks.next()?;
        items.extend(found);
        Some(consistent)
      });
      (result, items, count)
    };

    // consistent at once
    assert_eq!(run(vec![(vec![1, 2], true)]), (Some(()), vec![7, 1, 2], 1));
    // the items of an inconsistent walk are dropped, the earlier items stay
    assert_eq!(run(vec![(vec![1], false), (vec![2, 3], true)]), (Some(()), vec![7, 2, 3], 2));
    // never consistent: the last walk's items after the third try
    let walks = vec![(vec![1], false), (vec![2], false), (vec![3], false), (vec![4], true)];
    assert_eq!(run(walks), (Some(()), vec![7, 3], 3));
    // a failed walk fails the whole read
    assert_eq!(run(vec![(vec![1], false)]).0, None);
  }

  #[test]
  fn creator_strings() {
    assert_eq!(parse_creator("pid 631, WindowServer"), Some(631));
    assert_eq!(parse_creator("pid 2013, Siri AI"), Some(2013));
    assert_eq!(parse_creator("pid 7, a, b"), Some(7));
    assert_eq!(parse_creator("pid 0, kernel_task"), Some(0));
    assert_eq!(parse_creator("pid 42"), Some(42));

    let bad = [
      "",
      "pid",
      "pid ",
      "pid , WindowServer", // missing pid
      "pid -1, x",
      "pid +1, x",
      "pid 12a, x",
      "pid 1 , x",
      "pid 99999999999, x", // doesn't fit i32
      "PID 1, x",
      " pid 1, x",
      "WindowServer",
      "631, WindowServer",
    ];
    for creator in bad {
      assert_eq!(parse_creator(creator), None, "{creator:?}");
    }
  }

  fn app_usage(entries: &[CFType]) -> CFType {
    CFArray::from_CFTypes(entries).into_CFType()
  }

  fn usage_entry(pairs: &[(&str, CFType)]) -> CFType {
    let pairs: Vec<(CFType, CFType)> =
      pairs.iter().map(|(key, value)| (CFString::new(key).into_CFType(), value.clone())).collect();
    CFDictionary::from_CFType_pairs(&pairs).into_CFType()
  }

  #[test]
  fn app_usage_sums_gpu_time() {
    let num = |n: i64| CFNumber::from(n).into_CFType();
    let gpu_time = |ns: i64| {
      let api = CFString::new("Metal").into_CFType();
      usage_entry(&[("API", api), ("lastSubmittedTime", num(9)), ("accumulatedGPUTime", num(ns))])
    };

    let usage = app_usage(&[gpu_time(400), gpu_time(0), gpu_time(6)]);
    assert_eq!(app_usage_ns(&usage), 406);

    let text = |s: &str| CFString::new(s).into_CFType();
    let skipped = [
      gpu_time(-5),                                      // negative
      usage_entry(&[("lastSubmittedTime", num(9))]),     // no GPU time
      usage_entry(&[("accumulatedGPUTime", text("1"))]), // not a number
      text("garbage"),                                   // not a dict
      gpu_time(10),
    ];
    assert_eq!(app_usage_ns(&app_usage(&skipped)), 10);

    assert_eq!(app_usage_ns(&app_usage(&[])), 0);
    let huge = app_usage(&[gpu_time(i64::MAX), gpu_time(i64::MAX), gpu_time(i64::MAX)]);
    assert_eq!(app_usage_ns(&huge), u64::MAX);
    assert_eq!(app_usage_ns(&num(5)), 0); // not an array
    assert_eq!(app_usage_ns(&gpu_time(5)), 0);
  }

  #[test]
  fn gpu_times_per_pid() {
    let times = sum_gpu_times([(631, 10), (735, 5), (631, 7), (1, 0)]);
    assert_eq!(times, HashMap::from([(631, 17), (735, 5), (1, 0)]));
    assert_eq!(sum_gpu_times([(1, u64::MAX), (1, 1)]), HashMap::from([(1, u64::MAX)]));
    assert!(sum_gpu_times([]).is_empty());
  }

  #[test]
  fn gpu_delta() {
    assert_eq!(gpu_pct(Some(0), Some(SEC / 2), SEC), 50.0);
    assert_eq!(gpu_pct(Some(SEC), Some(SEC + SEC / 4), SEC / 2), 50.0);
    assert_eq!(gpu_pct(Some(0), Some(3 * SEC), SEC), 100.0); // several queues busy at once
    assert_eq!(gpu_pct(Some(5), Some(5), SEC), 0.0);
    assert_eq!(gpu_pct(Some(SEC), Some(0), SEC), 0.0); // a client closed
    assert_eq!(gpu_pct(None, Some(SEC), SEC), 0.0); // first GPU client: no baseline
    assert_eq!(gpu_pct(Some(0), None, SEC), 0.0); // GPU clients gone
    assert_eq!(gpu_pct(None, None, SEC), 0.0);
    assert_eq!(gpu_pct(Some(0), Some(SEC), 0), 0.0);

    let gpu = |cpu_ns: u64, gpu_ns: Option<u64>| Counters { gpu_ns, ..counters(cpu_ns, None) };
    let busy = usage(Some(&gpu(0, Some(0))), &gpu(SEC, Some(SEC / 4)), SEC);
    assert_eq!((busy.cpu_pct, busy.gpu_pct), (100.0, 25.0));

    // GPU time going backwards doesn't affect the CPU rate.
    let closed = usage(Some(&gpu(0, Some(SEC))), &gpu(SEC, Some(0)), SEC);
    assert_eq!((closed.cpu_pct, closed.gpu_pct), (100.0, 0.0));

    // New process under the same pid: no spike from its GPU time.
    let reused = Counters { start: 43, ..gpu(SEC, Some(SEC)) };
    assert_eq!(usage(Some(&gpu(0, Some(0))), &reused, SEC).gpu_pct, 0.0);
    assert_eq!(usage(None, &gpu(SEC, Some(SEC)), SEC).gpu_pct, 0.0);
  }

  #[test]
  fn update_gpu_rates() {
    let gpu_row = |gpu_ns: Option<u64>, comm: &str| {
      let mut raw = row(A, 0, None, comm);
      raw.counters.gpu_ns = gpu_ns;
      raw
    };
    let mut sampler = ProcSampler::new();

    assert_eq!(sampler.update(vec![gpu_row(Some(SEC), "a")], 0)[0].gpu_pct, 0.0);
    assert_eq!(sampler.update(vec![gpu_row(Some(2 * SEC), "a")], 2 * SEC)[0].gpu_pct, 50.0);
    assert_eq!(sampler.update(vec![gpu_row(None, "a")], SEC)[0].gpu_pct, 0.0);
    assert_eq!(sampler.update(vec![gpu_row(Some(3 * SEC), "a")], SEC)[0].gpu_pct, 0.0);
    assert_eq!(sampler.update(vec![gpu_row(Some(4 * SEC), "b")], SEC)[0].gpu_pct, 0.0); // exec
  }

  #[test]
  fn reads_gpu_clients() {
    // CI VMs may have no GPU clients at all; the IORegistry walk itself must work.
    let clients = read_gpu_clients().expect("IORegistry is readable");
    assert!(clients.iter().all(|&(pid, _)| pid >= 0), "{clients:?}");

    let pids: HashSet<i32> = clients.iter().map(|&(pid, _)| pid).collect();
    assert_eq!(sum_gpu_times(clients).len(), pids.len());
    for _ in 0..3 {
      assert!(read_gpu_clients().is_some());
    }
  }
}
