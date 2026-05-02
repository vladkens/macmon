#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::{
  collections::HashMap,
  marker::{PhantomData, PhantomPinned},
  mem::{MaybeUninit, size_of},
  os::raw::c_void,
  ptr::null,
  sync::OnceLock,
};

use core_foundation::{
  array::{
    CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef,
    CFMutableArrayRef, kCFTypeArrayCallBacks,
  },
  base::{CFAllocatorRef, CFRange, CFRelease, CFTypeRef, kCFAllocatorDefault, kCFAllocatorNull},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::{
    CFDictionaryCreate, CFDictionaryCreateMutableCopy, CFDictionaryGetCount,
    CFDictionaryGetKeysAndValues, CFDictionaryGetValue, CFDictionaryRef, CFDictionarySetValue,
    CFMutableDictionaryRef, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
  },
  number::{CFNumberCreate, CFNumberRef, kCFNumberSInt32Type},
  string::{CFStringCreateWithBytesNoCopy, CFStringGetCString, CFStringRef, kCFStringEncodingUTF8},
};
use serde::Serialize;

pub type WithError<T> = Result<T, Box<dyn std::error::Error>>;
pub type CVoidRef = *const std::ffi::c_void;

static SOC_INFO_CACHE: OnceLock<SocInfo> = OnceLock::new();

// MARK: CFUtils

pub fn cfnum(val: i32) -> CFNumberRef {
  unsafe { CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &val as *const i32 as _) }
}

pub fn cfstr(val: &str) -> CFStringRef {
  // this creates broken objects if string len > 9
  // CFString::from_static_string(val).as_concrete_TypeRef()
  // CFString::new(val).as_concrete_TypeRef()

  unsafe {
    CFStringCreateWithBytesNoCopy(
      kCFAllocatorDefault,
      val.as_ptr(),
      val.len() as isize,
      kCFStringEncodingUTF8,
      0,
      kCFAllocatorNull,
    )
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn from_cfstr(val: CFStringRef) -> String {
  unsafe {
    let mut buf = Vec::with_capacity(128);
    if CFStringGetCString(val, buf.as_mut_ptr(), 128, kCFStringEncodingUTF8) == 0 {
      panic!("Failed to convert CFString to CString");
    }
    std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().to_string()
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn cfdict_keys(dict: CFDictionaryRef) -> Vec<String> {
  unsafe {
    let count = CFDictionaryGetCount(dict) as usize;
    let mut keys: Vec<CFStringRef> = Vec::with_capacity(count);
    let mut vals: Vec<CFTypeRef> = Vec::with_capacity(count);
    CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr() as _, vals.as_mut_ptr());
    keys.set_len(count);
    vals.set_len(count);

    keys.iter().map(|k| from_cfstr(*k as _)).collect()
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn cfdict_get_val(dict: CFDictionaryRef, key: &str) -> Option<CFTypeRef> {
  unsafe {
    let key = cfstr(key);
    let val = CFDictionaryGetValue(dict, key as _);
    CFRelease(key as _);

    match val {
      _ if val.is_null() => None,
      _ => Some(val),
    }
  }
}

// MARK: IOReport Bindings

#[link(name = "IOKit", kind = "framework")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOServiceMatching(name: *const i8) -> CFMutableDictionaryRef;
  fn IOServiceGetMatchingServices(mainPort: u32, matching: CFDictionaryRef, existing: *mut u32) -> i32;
  fn IOIteratorNext(iterator: u32) -> u32;
  fn IORegistryEntryGetName(entry: u32, name: *mut i8) -> i32;
  fn IORegistryEntryCreateCFProperties(entry: u32, properties: *mut CFMutableDictionaryRef, allocator: CFAllocatorRef, options: u32) -> i32;
  fn IOObjectRelease(obj: u32) -> u32;
}

#[repr(C)]
struct IOReportSubscription {
  _data: [u8; 0],
  _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}

type IOReportSubscriptionRef = *const IOReportSubscription;
pub type ChannelFilter = fn(&str, &str, &str, &str) -> bool;

#[link(name = "IOReport", kind = "dylib")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
  fn IOReportCreateSubscription(a: CVoidRef, b: CFMutableDictionaryRef, c: *mut CFMutableDictionaryRef, d: u64, b: CFTypeRef) -> IOReportSubscriptionRef;
  fn IOReportCreateSamples(a: IOReportSubscriptionRef, b: CFMutableDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportCreateSamplesDelta(a: CFDictionaryRef, b: CFDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportChannelGetGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetSubGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetChannelName(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportSimpleGetIntegerValue(a: CFDictionaryRef, b: i32) -> i64;
  fn IOReportChannelGetUnitLabel(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportStateGetCount(a: CFDictionaryRef) -> i32;
  fn IOReportStateGetNameForIndex(a: CFDictionaryRef, b: i32) -> CFStringRef;
  fn IOReportStateGetResidency(a: CFDictionaryRef, b: i32) -> i64;
}

// MARK: IOReport helpers

fn cfio_get_group(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetGroup(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_get_subgroup(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetSubGroup(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_get_channel(item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetChannelName(item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn cfio_channel_matches(items: &[(&str, Option<&str>)], group: &str, subgroup: &str) -> bool {
  items.is_empty()
    || items.iter().any(|(item_group, item_subgroup)| {
      *item_group == group && item_subgroup.map_or(true, |value| value == subgroup)
    })
}

pub fn cfio_get_props(entry: u32, name: String) -> WithError<CFDictionaryRef> {
  unsafe {
    let mut props: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
    if IORegistryEntryCreateCFProperties(entry, props.as_mut_ptr(), kCFAllocatorDefault, 0) != 0 {
      return Err(format!("Failed to get properties for {}", name).into());
    }

    Ok(props.assume_init())
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn cfio_get_residencies(item: CFDictionaryRef) -> Vec<(String, i64)> {
  let count = unsafe { IOReportStateGetCount(item) };
  let mut res = vec![];

  for i in 0..count {
    let name = unsafe { IOReportStateGetNameForIndex(item, i) };
    let val = unsafe { IOReportStateGetResidency(item, i) };
    let name = match name {
      x if x.is_null() => format!("S{i}"),
      x => from_cfstr(x),
    };
    res.push((name, val));
  }

  res
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn cfio_watts(item: CFDictionaryRef, unit: &String, duration: u64) -> WithError<f32> {
  let val = unsafe { IOReportSimpleGetIntegerValue(item, 0) } as f32;
  let val = val / (duration as f32 / 1000.0);
  match unit.as_str() {
    "mJ" => Ok(val / 1e3f32),
    "uJ" => Ok(val / 1e6f32),
    "nJ" => Ok(val / 1e9f32),
    _ => Err(format!("Invalid energy unit: {}", unit).into()),
  }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn cfio_integer_value(item: CFDictionaryRef) -> i64 {
  unsafe { IOReportSimpleGetIntegerValue(item, 0) }
}

// MARK: IOServiceIterator

pub struct IOServiceIterator {
  existing: u32,
}

impl IOServiceIterator {
  pub fn new(service_name: &str) -> WithError<Self> {
    let service_name = std::ffi::CString::new(service_name).unwrap();
    let existing = unsafe {
      let service = IOServiceMatching(service_name.as_ptr() as _);
      let mut existing = 0;
      if IOServiceGetMatchingServices(0, service, &mut existing) != 0 {
        return Err(format!("{} not found", service_name.to_string_lossy()).into());
      }
      existing
    };

    Ok(Self { existing })
  }
}

impl Drop for IOServiceIterator {
  fn drop(&mut self) {
    unsafe {
      IOObjectRelease(self.existing);
    }
  }
}

impl Iterator for IOServiceIterator {
  type Item = (u32, String);

  fn next(&mut self) -> Option<Self::Item> {
    let next = unsafe { IOIteratorNext(self.existing) };
    if next == 0 {
      return None;
    }

    let mut name = [0; 128]; // 128 defined in apple docs
    if unsafe { IORegistryEntryGetName(next, name.as_mut_ptr()) } != 0 {
      return None;
    }

    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    let name = name.to_string_lossy().to_string();
    Some((next, name))
  }
}

// MARK: IOReportIterator

pub struct IOReportIterator {
  sample: CFDictionaryRef,
  index: isize,
  items: CFArrayRef,
  items_size: isize,
  metadata: Vec<(String, String, String, String)>,
}

impl IOReportIterator {
  pub fn new(data: CFDictionaryRef, metadata: Vec<(String, String, String, String)>) -> Self {
    let items = cfdict_get_val(data, "IOReportChannels").unwrap() as CFArrayRef;
    let items_size = unsafe { CFArrayGetCount(items) } as isize;
    Self { sample: data, items, items_size, index: 0, metadata }
  }
}

impl Drop for IOReportIterator {
  fn drop(&mut self) {
    unsafe { CFRelease(self.sample as _) };
  }
}

#[derive(Debug)]
pub struct IOReportIteratorItem {
  pub group: String,
  pub subgroup: String,
  pub channel: String,
  pub unit: String,
  pub item: CFDictionaryRef,
}

impl Iterator for IOReportIterator {
  type Item = IOReportIteratorItem;

  fn next(&mut self) -> Option<Self::Item> {
    if self.index >= self.items_size {
      return None;
    }

    let item = unsafe { CFArrayGetValueAtIndex(self.items, self.index) } as CFDictionaryRef;
    let (group, subgroup, channel, unit) =
      self.metadata.get(self.index as usize).cloned().unwrap_or_default();

    self.index += 1;
    Some(IOReportIteratorItem { group, subgroup, channel, unit, item })
  }
}

// MARK: RAM

pub fn libc_ram() -> WithError<(u64, u64)> {
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

    // todo: https://github.com/JohnTitor/mach2/issues/34
    #[allow(deprecated)]
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

    usage = (stats.active_count as u64
      + stats.inactive_count as u64
      + stats.wire_count as u64
      + stats.speculative_count as u64
      + stats.compressor_page_count as u64
      - stats.purgeable_count as u64
      - stats.external_page_count as u64)
      * page_size_kb;
  }

  Ok((usage, total))
}

pub fn libc_swap() -> WithError<(u64, u64)> {
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

// MARK: SockInfo

#[derive(Debug, Default, Clone, Serialize)]
pub struct SocInfo {
  pub mac_model: String,
  pub chip_name: String,
  pub memory_gb: u16,
  pub ecpu_cores: u8,
  pub pcpu_cores: u8,
  pub ecpu_label: String, // "E" on M1-M4, "P" on M5+
  pub pcpu_label: String, // "P" on M1-M4, "S" on M5+
  pub ecpu_freqs: Vec<u32>,
  pub pcpu_freqs: Vec<u32>,
  pub gpu_cores: u8,
  pub gpu_freqs: Vec<u32>,
}

// dynamic voltage and frequency scaling
pub fn get_dvfs_mhz(dict: CFDictionaryRef, key: &str) -> Option<(Vec<u32>, Vec<u32>)> {
  unsafe {
    let obj = cfdict_get_val(dict, key)? as CFDataRef;
    let obj_len = CFDataGetLength(obj);
    let obj_val = vec![0u8; obj_len as usize];
    CFDataGetBytes(obj, CFRange::init(0, obj_len), obj_val.as_ptr() as *mut u8);

    // obj_val is pairs of (freq, voltage) 4 bytes each
    let items_count = (obj_len / 8) as usize;
    let [mut freqs, mut volts] = [vec![0u32; items_count], vec![0u32; items_count]];
    for (i, x) in obj_val.chunks_exact(8).enumerate() {
      volts[i] = u32::from_le_bytes([x[4], x[5], x[6], x[7]]);
      freqs[i] = u32::from_le_bytes([x[0], x[1], x[2], x[3]]);
    }

    Some((volts, freqs))
  }
}

// Parse acc-clusters bytes into (ecpu_key, pcpu_key) voltage-states key names.
// Each 8-byte entry: byte 0 = voltage-states index, byte 1 = cluster type
// (0 = efficiency/lowest tier, higher = higher perf tier).
// Picks highest type as pcpu, second-highest as ecpu — handles M5 Max where
// type 0 (E-core cluster) is absent and the two active tiers are 1 and 2.
fn parse_acc_clusters(data: &[u8]) -> Option<(String, String)> {
  let mut clusters: Vec<(u8, String)> = Vec::new();
  for chunk in data.chunks_exact(8) {
    clusters.push((chunk[1], format!("voltage-states{}-sram", chunk[0])));
  }
  clusters.sort_by_key(|c| c.0);
  if clusters.len() < 2 {
    return None;
  }
  let ecpu_key = clusters[clusters.len() - 2].1.clone();
  let pcpu_key = clusters.last()?.1.clone();
  Some((ecpu_key, pcpu_key))
}

// Read acc-clusters from pmgr dict and parse into (ecpu_key, pcpu_key).
fn parse_acc_clusters_from(dict: CFDictionaryRef) -> Option<(String, String)> {
  let obj = cfdict_get_val(dict, "acc-clusters")? as CFDataRef;

  let len = unsafe { CFDataGetLength(obj) } as usize;
  if len < 8 {
    return None;
  }

  let mut data = vec![0u8; len];
  unsafe { CFDataGetBytes(obj, CFRange::init(0, len as _), data.as_mut_ptr()) };

  parse_acc_clusters(&data)
}

pub fn run_system_profiler() -> WithError<serde_json::Value> {
  // system_profiler -listDataTypes
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

// M1–M3 and A-series chips store frequencies in Hz; M4+ store in kHz.
fn cpu_freq_scale(chip_name: &str) -> u32 {
  let hz_freqs = chip_name.contains("M1")
    || chip_name.contains("M2")
    || chip_name.contains("M3")
    || chip_name.contains("A1"); // A14–A18 and future A1x chips
  if hz_freqs { 1_000_000 } else { 1_000 }
}

// Try known voltage-states key (M1-M4) first, fall back to acc-clusters discovery (M5+).
fn cpu_freqs(item: CFDictionaryRef, key: &str, is_ecpu: bool, scale: u32) -> Option<Vec<u32>> {
  if let Some((_, freqs)) = get_dvfs_mhz(item, key) {
    return Some(to_mhz(freqs, scale));
  }
  let (ecpu_key, pcpu_key) = parse_acc_clusters_from(item)?;
  let key = if is_ecpu { ecpu_key } else { pcpu_key };
  let (_, freqs) = get_dvfs_mhz(item, &key)?;
  Some(to_mhz(freqs, scale))
}

// Parse "proc T:P:E" (macOS 15) or "proc T:P_or_S:E:M" (macOS 26+) into (ecpu, pcpu, has_mcpu).
// macOS 26 always uses 4 fields; M5+ has M>0 (ecpu=M, pcpu=S), M1-M4 has M=0 (ecpu=E, pcpu=P).
fn parse_cpu_cores(s: &str) -> (u64, u64, bool) {
  let procs = s.strip_prefix("proc ").unwrap_or("");
  let parts: Vec<u64> = procs.split(':').map(|x| x.parse().unwrap_or(0)).collect();

  match parts.len() {
    4 => {
      let (e, m) = (parts[2], parts[3]);
      if m > 0 { (m, parts[1], true) } else { (e, parts[1], false) }
    }
    3 => (parts[2], parts[1], false), // macOS 15: "proc total:P:E"
    _ => (0, 0, false),
  }
}

pub fn get_soc_info() -> WithError<SocInfo> {
  if let Some(info) = SOC_INFO_CACHE.get() {
    return Ok(info.clone());
  }

  let info = load_soc_info()?;
  let _ = SOC_INFO_CACHE.set(info.clone());
  Ok(info)
}

fn load_soc_info() -> WithError<SocInfo> {
  let out = run_system_profiler()?;
  let mut info = SocInfo::default();

  // SPHardwareDataType.0.chip_type
  let chip_name = out["SPHardwareDataType"][0]["chip_type"].as_str();
  let chip_name = chip_name.unwrap_or("Unknown chip").to_string();

  // SPHardwareDataType.0.machine_model
  let mac_model = out["SPHardwareDataType"][0]["machine_model"].as_str();
  let mac_model = mac_model.unwrap_or("Unknown model").to_string();

  // SPHardwareDataType.0.physical_memory -> "x GB"
  let mem_gb = out["SPHardwareDataType"][0]["physical_memory"].as_str();
  let mem_gb = mem_gb.and_then(|x| x.strip_suffix(" GB")).and_then(|x| x.parse::<u64>().ok());
  let mem_gb = mem_gb.unwrap_or(0);

  // SPHardwareDataType.0.number_processors -> "proc x:y:z" or "proc x:y:z:w"
  let number_processors = out["SPHardwareDataType"][0]["number_processors"].as_str().unwrap_or("");
  let (ecpu_cores, pcpu_cores, has_mcpu) = parse_cpu_cores(number_processors);

  // SPDisplaysDataType.0.sppci_cores
  let gpu_cores = out["SPDisplaysDataType"][0]["sppci_cores"].as_str();
  let gpu_cores = gpu_cores.unwrap_or("0").parse::<u64>().unwrap_or(0);

  let cpu_scale = cpu_freq_scale(&chip_name);
  let gpu_scale: u32 = 1000 * 1000; // MHz

  // Assign parsed values to info
  info.chip_name = chip_name;
  info.mac_model = mac_model;
  info.memory_gb = mem_gb as u16;
  info.gpu_cores = gpu_cores as u8;
  info.ecpu_cores = ecpu_cores as u8;
  info.pcpu_cores = pcpu_cores as u8;
  info.ecpu_label = if has_mcpu { "P".into() } else { "E".into() };
  info.pcpu_label = if has_mcpu { "S".into() } else { "P".into() };

  // CPU frequencies
  for (entry, name) in IOServiceIterator::new("AppleARMIODevice")? {
    if name == "pmgr" {
      let item = cfio_get_props(entry, name)?;
      // 1) `strings /usr/bin/powermetrics | grep voltage-states` uses non-sram keys
      //    but their values are zero, so sram used here; it looks valid.
      // 2) sudo powermetrics --samplers cpu_power -i 1000 -n 1 | grep "active residency" | grep "Cluster"
      if let Some(f) = cpu_freqs(item, "voltage-states1-sram", true, cpu_scale) {
        info.ecpu_freqs = f;
      }
      if let Some(f) = cpu_freqs(item, "voltage-states5-sram", false, cpu_scale) {
        info.pcpu_freqs = f;
      }

      if let Some((_, freqs)) = get_dvfs_mhz(item, "voltage-states9") {
        info.gpu_freqs = to_mhz(freqs, gpu_scale);
      }
      unsafe { CFRelease(item as _) }
    }
  }

  if info.ecpu_freqs.is_empty() || info.pcpu_freqs.is_empty() {
    return Err("No CPU frequencies found".into());
  }

  Ok(info)
}

// MARK: IOReport

struct IOReportChannels {
  chan: CFMutableDictionaryRef,
  source: Option<CFDictionaryRef>,
  selected: Option<CFMutableArrayRef>,
}

fn cfio_get_chan(filter: Option<ChannelFilter>) -> WithError<IOReportChannels> {
  let all_channels = unsafe { IOReportCopyAllChannels(0, 0) };
  let Some(channel_array) = cfdict_get_val(all_channels, "IOReportChannels") else {
    unsafe { CFRelease(all_channels as _) };
    return Err("Failed to get channels".into());
  };
  let channel_array = channel_array as CFArrayRef;

  let size = unsafe { CFDictionaryGetCount(all_channels) };
  let chan = unsafe { CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, all_channels) };

  let mut selected_channels = None;
  if let Some(filter) = filter {
    let count = unsafe { CFArrayGetCount(channel_array) };
    let selected =
      unsafe { CFArrayCreateMutable(kCFAllocatorDefault, count, &kCFTypeArrayCallBacks) };

    for i in 0..count {
      let item = unsafe { CFArrayGetValueAtIndex(channel_array, i) } as CFDictionaryRef;
      let group = cfio_get_group(item);
      let subgroup = cfio_get_subgroup(item);
      let channel = cfio_get_channel(item);
      let unit = from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string();
      if filter(&group, &subgroup, &channel, &unit) {
        unsafe { CFArrayAppendValue(selected, item as _) };
      }
    }

    let key = cfstr("IOReportChannels");
    unsafe {
      CFDictionarySetValue(chan, key as _, selected as _);
      CFRelease(key as _);
    }
    selected_channels = Some(selected);
  }

  Ok(IOReportChannels { chan, source: Some(all_channels), selected: selected_channels })
}

fn cfio_channel_metadata(channels: CFDictionaryRef) -> Vec<(String, String, String, String)> {
  let Some(channel_array) = cfdict_get_val(channels, "IOReportChannels") else {
    return Vec::new();
  };
  let channel_array = channel_array as CFArrayRef;
  let count = unsafe { CFArrayGetCount(channel_array) };
  let mut metadata = Vec::with_capacity(count as usize);

  for i in 0..count {
    let item = unsafe { CFArrayGetValueAtIndex(channel_array, i) } as CFDictionaryRef;
    metadata.push((
      cfio_get_group(item),
      cfio_get_subgroup(item),
      cfio_get_channel(item),
      from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string(),
    ));
  }

  metadata
}

fn cfio_get_subs(chan: CFMutableDictionaryRef) -> WithError<IOReportSubscriptionRef> {
  let mut s: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
  let rs = unsafe { IOReportCreateSubscription(null(), chan, s.as_mut_ptr(), 0, null()) };
  if rs.is_null() {
    return Err("Failed to create subscription".into());
  }

  unsafe { s.assume_init() };
  Ok(rs)
}

pub struct IOReport {
  subs: IOReportSubscriptionRef,
  chan: CFMutableDictionaryRef,
  source: Option<CFDictionaryRef>,
  selected: Option<CFMutableArrayRef>,
  metadata: Vec<(String, String, String, String)>,
  prev: Option<(CFDictionaryRef, std::time::Instant)>,
}

impl IOReport {
  pub fn new(filter: Option<ChannelFilter>) -> WithError<Self> {
    let channels = cfio_get_chan(filter)?;
    let metadata = cfio_channel_metadata(channels.chan);
    let subs = cfio_get_subs(channels.chan)?;
    Ok(Self {
      subs,
      chan: channels.chan,
      source: channels.source,
      selected: channels.selected,
      metadata,
      prev: None,
    })
  }

  pub fn get_sample(&self, duration: u64) -> IOReportIterator {
    unsafe {
      let sample1 = IOReportCreateSamples(self.subs, self.chan, null());
      std::thread::sleep(std::time::Duration::from_millis(duration));
      let sample2 = IOReportCreateSamples(self.subs, self.chan, null());

      let sample3 = IOReportCreateSamplesDelta(sample1, sample2, null());
      CFRelease(sample1 as _);
      CFRelease(sample2 as _);
      IOReportIterator::new(sample3, self.metadata.clone())
    }
  }

  fn raw_sample(&self) -> (CFDictionaryRef, std::time::Instant) {
    (unsafe { IOReportCreateSamples(self.subs, self.chan, null()) }, std::time::Instant::now())
  }

  pub fn get_samples(&mut self, duration: u64, count: usize) -> Vec<(IOReportIterator, u64)> {
    let count = count.clamp(1, 32);
    let mut samples: Vec<(IOReportIterator, u64)> = Vec::with_capacity(count);
    let step_msec = duration / count as u64;

    let mut prev = match self.prev {
      Some(x) => x,
      None => self.raw_sample(),
    };

    for _ in 0..count {
      if step_msec > 0 {
        std::thread::sleep(std::time::Duration::from_millis(step_msec));
      }

      let next = self.raw_sample();
      let diff = unsafe { IOReportCreateSamplesDelta(prev.0, next.0, null()) };
      unsafe { CFRelease(prev.0 as _) };

      let elapsed = next.1.duration_since(prev.1).as_millis() as u64;
      prev = next;

      samples.push((IOReportIterator::new(diff, self.metadata.clone()), elapsed.max(1)));
    }

    self.prev = Some(prev);
    samples
  }
}

impl Drop for IOReport {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.chan as _);
      CFRelease(self.subs as _);
      if let Some(selected) = self.selected {
        CFRelease(selected as _);
      }
      if let Some(source) = self.source {
        CFRelease(source as _);
      }
      if let Some(prev) = self.prev {
        CFRelease(prev.0 as _);
      }
    }
  }
}

// MARK: IOHID Bindings
// referenced from: https://github.com/freedomtan/sensors/blob/master/sensors/sensors.m

#[repr(C)]
struct IOHIDServiceClient(libc::c_void);

#[repr(C)]
struct IOHIDEventSystemClient(libc::c_void);

#[repr(C)]
struct IOHIDEvent(libc::c_void);

type IOHIDServiceClientRef = *const IOHIDServiceClient;
type IOHIDEventSystemClientRef = *const IOHIDEventSystemClient;
type IOHIDEventRef = *const IOHIDEvent;

const kHIDPage_AppleVendor: i32 = 0xff00;
const kHIDUsage_AppleVendor_TemperatureSensor: i32 = 0x0005;

const kIOHIDEventTypeTemperature: i64 = 15;
const kIOHIDEventTypePower: i64 = 25;

#[link(name = "IOKit", kind = "framework")]
#[rustfmt::skip]
unsafe extern "C" {
  fn IOHIDEventSystemClientCreate(allocator: CFAllocatorRef) -> IOHIDEventSystemClientRef;
  fn IOHIDEventSystemClientSetMatching(a: IOHIDEventSystemClientRef, b: CFDictionaryRef) -> i32;
  fn IOHIDEventSystemClientCopyServices(a: IOHIDEventSystemClientRef) -> CFArrayRef;
  fn IOHIDServiceClientCopyProperty(a: IOHIDServiceClientRef, b: CFStringRef) -> CFStringRef;
  fn IOHIDServiceClientCopyEvent(a: IOHIDServiceClientRef, v0: i64, v1: i32, v2: i64) -> IOHIDEventRef;
  fn IOHIDEventGetFloatValue(event: IOHIDEventRef, field: i64) -> f64;
}

// MARK: IOHIDSensors

pub struct IOHIDSensors {
  sensors: CFDictionaryRef,
}

impl IOHIDSensors {
  pub fn new() -> WithError<Self> {
    let keys = [cfstr("PrimaryUsagePage"), cfstr("PrimaryUsage")];
    let nums = [cfnum(kHIDPage_AppleVendor), cfnum(kHIDUsage_AppleVendor_TemperatureSensor)];

    let sensors = unsafe {
      CFDictionaryCreate(
        kCFAllocatorDefault,
        keys.as_ptr() as _,
        nums.as_ptr() as _,
        2,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
      )
    };

    Ok(Self { sensors })
  }

  pub fn get_metrics(&self) -> Vec<(String, f32)> {
    unsafe {
      let system = match IOHIDEventSystemClientCreate(kCFAllocatorDefault) {
        x if x.is_null() => return vec![],
        x => x,
      };

      IOHIDEventSystemClientSetMatching(system, self.sensors);

      let services = match IOHIDEventSystemClientCopyServices(system) {
        x if x.is_null() => return vec![],
        x => x,
      };

      let mut items = vec![] as Vec<(String, f32)>;
      for i in 0..CFArrayGetCount(services) {
        let sc = match CFArrayGetValueAtIndex(services, i) as IOHIDServiceClientRef {
          x if x.is_null() => continue,
          x => x,
        };

        let name = match IOHIDServiceClientCopyProperty(sc, cfstr("Product")) {
          x if x.is_null() => continue,
          x => from_cfstr(x),
        };

        let event = match IOHIDServiceClientCopyEvent(sc, kIOHIDEventTypeTemperature, 0, 0) {
          x if x.is_null() => continue,
          x => x,
        };

        let temp = IOHIDEventGetFloatValue(event, kIOHIDEventTypeTemperature << 16);
        CFRelease(event as _);
        if temp <= 0.0 || temp > 150.0 {
          continue;
        }
        items.push((name, temp as f32));
      }

      CFRelease(services as _);
      CFRelease(system as _);

      items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
      items
    }
  }
}

impl Drop for IOHIDSensors {
  fn drop(&mut self) {
    unsafe { CFRelease(self.sensors as _) };
  }
}

// MARK: SMC Bindings

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
  fn mach_task_self() -> u32;
  fn IOServiceOpen(device: u32, a: u32, b: u32, c: *mut u32) -> i32;
  fn IOServiceClose(conn: u32) -> i32;
  fn IOConnectCallStructMethod(
    conn: u32,
    selector: u32,
    ival: *const c_void,
    isize: usize,
    oval: *mut c_void,
    osize: *mut usize,
  ) -> i32;
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct KeyDataVer {
  pub major: u8,
  pub minor: u8,
  pub build: u8,
  pub reserved: u8,
  pub release: u16,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct PLimitData {
  pub version: u16,
  pub length: u16,
  pub cpu_p_limit: u32,
  pub gpu_p_limit: u32,
  pub mem_p_limit: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyInfo {
  pub data_size: u32,
  pub data_type: u32,
  pub data_attributes: u8,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct KeyData {
  pub key: u32,
  pub vers: KeyDataVer,
  pub p_limit_data: PLimitData,
  pub key_info: KeyInfo,
  pub result: u8,
  pub status: u8,
  pub data8: u8,
  pub data32: u32,
  pub bytes: [u8; 32],
}

// MARK: SMC

#[allow(clippy::upper_case_acronyms)]
pub struct SMC {
  conn: u32,
  keys: HashMap<u32, KeyInfo>,
}

impl SMC {
  pub fn new() -> WithError<Self> {
    let mut conn = 0;

    for (device, name) in IOServiceIterator::new("AppleSMC")? {
      if name == "AppleSMCKeysEndpoint" {
        let rs = unsafe { IOServiceOpen(device, mach_task_self(), 0, &mut conn) };
        if rs != 0 {
          return Err(format!("IOServiceOpen: {}", rs).into());
        }
      }
    }

    Ok(Self { conn, keys: HashMap::new() })
  }

  fn read(&self, input: &KeyData) -> WithError<KeyData> {
    let ival = input as *const _ as _;
    let ilen = size_of::<KeyData>();
    let mut oval = KeyData::default();
    let mut olen = size_of::<KeyData>();

    let rs = unsafe {
      IOConnectCallStructMethod(self.conn, 2, ival, ilen, &mut oval as *mut _ as _, &mut olen)
    };

    if rs != 0 {
      // println!("{:?}", input);
      return Err(format!("IOConnectCallStructMethod: {}", rs).into());
    }

    if oval.result == 132 {
      return Err("SMC key not found".into());
    }

    if oval.result != 0 {
      return Err(format!("SMC error: {}", oval.result).into());
    }

    Ok(oval)
  }

  fn parse_key(key: &str) -> WithError<u32> {
    if key.len() != 4 {
      return Err("SMC key must be 4 bytes long".into());
    }

    Ok(key.bytes().fold(0, |acc, x| (acc << 8) + x as u32))
  }

  fn key_by_index(&self, index: u32) -> WithError<String> {
    let ival = KeyData { data8: 8, data32: index, ..Default::default() };
    let oval = self.read(&ival)?;
    Ok(std::str::from_utf8(&oval.key.to_be_bytes()).unwrap().to_string())
  }

  fn read_key_info(&mut self, key: &str) -> WithError<KeyInfo> {
    let key = Self::parse_key(key)?;
    if let Some(key_info) = self.keys.get(&key) {
      // println!("cache hit for {}", key);
      return Ok(*key_info);
    }

    let ival = KeyData { data8: 9, key, ..Default::default() };
    let oval = self.read(&ival)?;
    self.keys.insert(key, oval.key_info);
    Ok(oval.key_info)
  }

  pub fn read_float_val(&mut self, key: &str) -> WithError<f32> {
    const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "

    let key_info = self.read_key_info(key)?;
    if key_info.data_size != 4 || key_info.data_type != FLOAT_TYPE {
      return Err(
        format!(
          "SMC key '{}' is not a 4-byte float (size={}, type={})",
          key, key_info.data_size, key_info.data_type
        )
        .into(),
      );
    }

    let key = Self::parse_key(key)?;
    let ival = KeyData { data8: 5, key, key_info, ..Default::default() };
    let oval = self.read(&ival)?;

    Ok(f32::from_le_bytes(oval.bytes[0..4].try_into().unwrap()))
  }

  pub fn key_count(&mut self) -> WithError<u32> {
    let key_info = self.read_key_info("#KEY")?;
    let key = Self::parse_key("#KEY")?;
    let ival = KeyData { data8: 5, key, key_info, ..Default::default() };
    let oval = self.read(&ival)?;
    Ok(u32::from_be_bytes(oval.bytes[0..4].try_into().unwrap()))
  }

  pub fn read_all_keys(&mut self) -> WithError<Vec<String>> {
    let count = self.key_count()?;

    let mut keys = Vec::new();
    for i in 0..count {
      match self.key_by_index(i) {
        Ok(key) => keys.push(key),
        Err(_) => continue,
      }
    }

    Ok(keys)
  }
}

impl Drop for SMC {
  fn drop(&mut self) {
    unsafe {
      IOServiceClose(self.conn);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_acc_clusters_m5_max() {
    // Real acc-clusters bytes captured from M5 Max via ioreg
    #[rustfmt::skip]
    let data = [
      0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
      0x05, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let (e, p) = parse_acc_clusters(&data).unwrap();
    // Second-highest type (1 = Performance) as ecpu, highest (2 = Super) as pcpu
    assert_eq!(e, "voltage-states23-sram");
    assert_eq!(p, "voltage-states5-sram");
  }

  #[test]
  fn parse_acc_clusters_incomplete() {
    assert!(parse_acc_clusters(&[]).is_none());
    // Single cluster – need both ecpu and pcpu
    assert!(parse_acc_clusters(&[1, 0, 0, 0, 0, 0, 0, 0]).is_none());
  }

  #[test]
  fn parse_cpu_cores_macos26_4field() {
    // Real data captured from macOS 26 machines
    // M5 Max: 18 total, 6 super, 0 efficiency, 12 performance(M-cores)
    assert_eq!(parse_cpu_cores("proc 18:6:0:12"), (12, 6, true));
    // M4 Max: 16 total, 12 performance, 4 efficiency, 0 M-cores
    assert_eq!(parse_cpu_cores("proc 16:12:4:0"), (4, 12, false));
    // M3 Air: 8 total, 4 performance, 4 efficiency, 0 M-cores
    assert_eq!(parse_cpu_cores("proc 8:4:4:0"), (4, 4, false));
  }

  #[test]
  fn parse_cpu_cores_macos15_3field() {
    // Real data: M3 Air on macOS 15.6.1
    assert_eq!(parse_cpu_cores("proc 8:4:4"), (4, 4, false));
  }

  #[test]
  fn parse_cpu_cores_invalid() {
    assert_eq!(parse_cpu_cores(""), (0, 0, false));
    assert_eq!(parse_cpu_cores("garbage"), (0, 0, false));
    assert_eq!(parse_cpu_cores("10:8:2"), (0, 0, false)); // missing "proc " prefix
    assert_eq!(parse_cpu_cores("proc 8"), (0, 0, false)); // too few fields
    assert_eq!(parse_cpu_cores("proc 8:4"), (0, 0, false)); // 2 fields, unsupported
    assert_eq!(parse_cpu_cores("proc 24:6:0:12:6"), (0, 0, false)); // unknown future format
  }

  #[test]
  fn to_mhz_scales() {
    // M4+: KHz scale
    assert_eq!(to_mhz(vec![4608000, 3000000], 1000), vec![4608, 3000]);
    // M1-M3: MHz scale
    assert_eq!(to_mhz(vec![3_000_000_000, 2_000_000_000], 1000 * 1000), vec![3000, 2000]);
    assert_eq!(to_mhz(vec![], 1000), Vec::<u32>::new());
  }

  #[test]
  fn cfio_channel_filter_preserves_group_subscription_semantics() {
    let items = [("Energy Model", None)];

    assert!(cfio_channel_matches(&items, "Energy Model", ""));
    assert!(cfio_channel_matches(&items, "Energy Model", "CPU Core Performance States"));
    assert!(!cfio_channel_matches(&items, "CPU Stats", "CPU Core Performance States"));
  }

  #[test]
  fn cfio_channel_filter_preserves_subgroup_subscription_semantics() {
    let items = [("CPU Stats", Some("CPU Core Performance States"))];

    assert!(cfio_channel_matches(&items, "CPU Stats", "CPU Core Performance States"));
    assert!(!cfio_channel_matches(&items, "CPU Stats", "CPU Performance States"));
    assert!(!cfio_channel_matches(&items, "GPU Stats", "CPU Core Performance States"));
  }

  #[test]
  fn cfio_channel_filter_empty_items_means_all_channels() {
    assert!(cfio_channel_matches(&[], "CPU Stats", "CPU Core Performance States"));
    assert!(cfio_channel_matches(&[], "Energy Model", ""));
  }
}
