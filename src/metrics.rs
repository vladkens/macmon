use crate::cfutil::*;
use core_foundation::{
  array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
  base::{kCFAllocatorDefault, CFAllocatorRef, CFRange, CFRelease, CFTypeRef},
  data::{CFDataGetBytes, CFDataGetLength, CFDataRef},
  dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFMutableDictionaryRef,
  },
  string::CFStringRef,
};
use std::marker::{PhantomData, PhantomPinned};
use std::mem::MaybeUninit;
use std::ptr::null;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const CPU_POWER_SUBG: &str = "CPU Complex Performance States";
const GPU_POWER_SUBG: &str = "GPU Performance States";

#[rustfmt::skip]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
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

#[rustfmt::skip]
#[link(name = "IOReport", kind = "dylib")]
extern "C" {
  fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
  fn IOReportCopyChannelsInGroup(a: CFStringRef, b: CFStringRef, c: u64, d: u64, e: u64) -> CFDictionaryRef;
  fn IOReportMergeChannels(a: CFDictionaryRef, b: CFDictionaryRef, nil: CFTypeRef);
  fn IOReportCreateSubscription(a: CVoidRef, b: CFMutableDictionaryRef, c: *mut CFMutableDictionaryRef, d: u64, b: CFTypeRef) -> IOReportSubscriptionRef;
  fn IOReportCreateSamples(a: IOReportSubscriptionRef, b: CFMutableDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportCreateSamplesDelta(a: CFDictionaryRef, b: CFDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
  fn IOReportChannelGetChannelName(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportSimpleGetIntegerValue(a: CFDictionaryRef, b: i32) -> i64;
  fn IOReportChannelGetUnitLabel(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportStateGetCount(a: CFDictionaryRef) -> i32;
  fn IOReportStateGetNameForIndex(a: CFDictionaryRef, b: i32) -> CFStringRef;
  fn IOReportStateGetResidency(a: CFDictionaryRef, b: i32) -> i64;
}

fn cfio_iter_next(existing: u32) -> Option<(u32, String)> {
  unsafe {
    let el = IOIteratorNext(existing);
    if el == 0 {
      return None;
    }

    let mut name = [0; 128]; // 128 defined in apple docs
    if IORegistryEntryGetName(el, name.as_mut_ptr()) != 0 {
      return None;
    }

    let name = std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy().to_string();
    Some((el, name))
  }
}

fn cfio_get_props(entry: u32, name: String) -> WithError<CFDictionaryRef> {
  unsafe {
    let mut props: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
    if IORegistryEntryCreateCFProperties(entry, props.as_mut_ptr(), kCFAllocatorDefault, 0) != 0 {
      return Err(format!("Failed to get properties for {}", name).into());
    }

    Ok(props.assume_init())
  }
}

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

// General info

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

fn fill_basic_info(info: &mut SocInfo) -> WithError<()> {
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

  Ok(())
}

fn fill_cores_info(info: &mut SocInfo) -> WithError<()> {
  unsafe {
    let service_name = std::ffi::CString::new("AppleARMIODevice").unwrap();
    let service = IOServiceMatching(service_name.as_ptr() as *const i8);

    let mut existing = 0;
    if IOServiceGetMatchingServices(0, service, &mut existing) != 0 {
      return Err("AppleARMIODevice not found".into());
    }

    // println!("Found {} services", existing);
    while let Some(obj) = cfio_iter_next(existing) {
      let (entry, name) = obj;
      // println!("Found service ({:?}): {}", entry, name);

      if name == "pmgr" {
        let item = cfio_get_props(entry, name)?;
        // let keys = cfdict_keys(item);
        // println!("Keys: {:?}", keys);
        // CFShow(item as _);

        info.ecpu_freqs = get_dvfs_mhz(item, "voltage-states1-sram").1;
        info.pcpu_freqs = get_dvfs_mhz(item, "voltage-states5-sram").1;
        info.gpu_freqs = get_dvfs_mhz(item, "voltage-states9").1;

        CFRelease(item as _);
      }
    }

    IOObjectRelease(existing);
  }

  if info.ecpu_freqs.len() == 0 || info.pcpu_freqs.len() == 0 {
    return Err("No CPU cores found".into());
  }

  Ok(())
}

pub fn initialize() -> WithError<SocInfo> {
  let mut info = SocInfo::default();
  fill_basic_info(&mut info)?;
  fill_cores_info(&mut info)?;
  Ok(info)
}

// Memory

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

// Metrics collector

unsafe fn cfio_get_chan(items: Vec<(&str, Option<&str>)>) -> WithError<CFMutableDictionaryRef> {
  // if no items are provided, return all channels
  if items.len() == 0 {
    let c = IOReportCopyAllChannels(0, 0);
    let r = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, CFDictionaryGetCount(c), c);
    CFRelease(c as _);
    return Ok(r);
  }

  let mut channels = vec![];
  for (group, subgroup) in items {
    let gname = cfstr(group);
    let sname = subgroup.map_or(null(), |x| cfstr(x));
    let chan = IOReportCopyChannelsInGroup(gname, sname, 0, 0, 0);
    channels.push(chan);

    CFRelease(gname as _);
    if subgroup.is_some() {
      CFRelease(sname as _);
    }
  }

  let chan = channels[0];
  for i in 1..channels.len() {
    IOReportMergeChannels(chan, channels[i], null());
  }

  let size = CFDictionaryGetCount(chan);
  let chan = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, chan);

  for i in 0..channels.len() {
    CFRelease(channels[i] as _);
  }

  if cfdict_get_val(chan, "IOReportChannels").is_none() {
    return Err("Failed to get channels".into());
  }

  Ok(chan)
}

unsafe fn cfio_get_subs(chan: CFMutableDictionaryRef) -> WithError<IOReportSubscriptionRef> {
  let mut s: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
  let rs = IOReportCreateSubscription(std::ptr::null(), chan, s.as_mut_ptr(), 0, std::ptr::null());
  if rs == std::ptr::null() {
    return Err("Failed to create subscription".into());
  }

  s.assume_init();
  Ok(rs)
}

#[derive(Debug, Default)]
pub struct MemoryUsage {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default)]
pub struct PerfSample {
  pub ecpu_usage: (u32, f32), // freq, percent_from_max
  pub pcpu_usage: (u32, f32), // freq, percent_from_max
  pub gpu_usage: (u32, f32),  // freq, percent_from_max
  pub all_watts: f32,         // W
  pub cpu_power: f32,         // W
  pub gpu_power: f32,         // W
  pub ane_power: f32,         // W
  pub memory: MemoryUsage,
}

unsafe fn calc_freq(item: CFDictionaryRef, freqs: &Vec<u32>) -> (u32, f32) {
  let count = IOReportStateGetCount(item) as usize;
  assert!(count > freqs.len(), "Invalid freqs count"); // todo?

  let mut residencies = vec![0; count];
  for i in 0..count as i32 {
    let val = IOReportStateGetResidency(item, i);
    residencies[i as usize] = val as u64;

    let _key = from_cfstr(IOReportStateGetNameForIndex(item, i));
    // println!("{} {}: {}", i, _key, val)
  }

  let count = freqs.len();
  let total = residencies.iter().sum::<u64>();
  let usage = residencies.iter().skip(1).sum::<u64>(); // first is IDLE for CPU and OFF for GPU

  let mut freq = 0f64;
  for i in 0..count {
    let percent = match usage {
      0 => 0.0,
      _ => residencies[i + 1] as f64 / usage as f64,
    };

    freq += percent * freqs[i] as f64;
  }

  let percent = usage as f64 / total as f64;
  let max_freq = freqs.last().unwrap().clone() as f64;
  let from_max = (freq * percent) / max_freq;

  (freq as u32, from_max as f32)
}

fn get_watts(item: CFDictionaryRef, unit: &String, duration: u64) -> WithError<f32> {
  let val = unsafe { IOReportSimpleGetIntegerValue(item, 0) } as f32;
  let val = val / (duration as f32 / 1000.0);
  match unit.as_str() {
    "mJ" => Ok(val / 1e3f32),
    "nJ" => Ok(val / 1e9f32),
    _ => Err(format!("Invalid energy unit: {}", unit).into()),
  }
}

unsafe fn cfio_parse_sample(
  subs: IOReportSubscriptionRef,
  chan: CFMutableDictionaryRef,
  info: &SocInfo,
  duration: u64,
) -> WithError<PerfSample> {
  let sample1 = IOReportCreateSamples(subs, chan, null());
  std::thread::sleep(std::time::Duration::from_millis(duration));
  let sample2 = IOReportCreateSamples(subs, chan, null());

  let sample = IOReportCreateSamplesDelta(sample1, sample2, null());
  CFRelease(sample1 as _);
  CFRelease(sample2 as _);

  let mut rs = PerfSample::default();
  let na = "na".to_string();

  let items = cfdict_get_val(sample, "IOReportChannels").unwrap() as CFArrayRef;
  for i in 0..CFArrayGetCount(items) {
    let item = CFArrayGetValueAtIndex(items, i) as CFDictionaryRef;

    let gname = cfdict_get_str(item, "IOReportGroupName").unwrap_or(na.clone());
    let sname = cfdict_get_str(item, "IOReportSubGroupName").unwrap_or(na.clone());
    let cname = from_cfstr(IOReportChannelGetChannelName(item));
    let _unit = from_cfstr(IOReportChannelGetUnitLabel(item));

    if gname == "CPU Stats" || gname == "GPU Stats" {
      match (sname.as_str(), cname.as_str()) {
        (CPU_POWER_SUBG, "ECPU") => rs.ecpu_usage = calc_freq(item, &info.ecpu_freqs),
        (CPU_POWER_SUBG, "PCPU") => rs.pcpu_usage = calc_freq(item, &info.pcpu_freqs),
        (GPU_POWER_SUBG, "GPUPH") => rs.gpu_usage = calc_freq(item, &info.gpu_freqs[1..].to_vec()),
        _ => {}
      }
    }

    if gname == "Energy Model" {
      // ultra chip is two joined max chips
      match cname.as_str() {
        "CPU Energy" => rs.cpu_power += get_watts(item, &_unit, duration)?,
        "GPU Energy" => rs.gpu_power += get_watts(item, &_unit, duration)?,
        x if x.starts_with("ANE") => rs.ane_power += get_watts(item, &_unit, duration)?,
        _ => (),
      }
    }
  }

  CFRelease(sample as _);
  rs.all_watts = rs.cpu_power + rs.gpu_power;
  Ok(rs)
}

pub struct SubsChan {
  subs: IOReportSubscriptionRef,
  chan: CFMutableDictionaryRef,
  info: SocInfo,
}

impl Drop for SubsChan {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.subs as _);
      CFRelease(self.chan as _);
    }
  }
}

impl SubsChan {
  pub fn new(info: SocInfo) -> WithError<Self> {
    let channels = vec![
      ("Energy Model", None),              // cpu+gpu+ane power
      ("CPU Stats", Some(CPU_POWER_SUBG)), // cpu freq by cluster
      ("GPU Stats", Some(GPU_POWER_SUBG)), // gpu freq
    ];

    let chan = unsafe { cfio_get_chan(channels)? };
    let subs = unsafe { cfio_get_subs(chan)? };
    Ok(Self { subs, chan, info })
  }

  pub fn sample(&mut self, duration: u64) -> WithError<PerfSample> {
    let (ram_usage, ram_total) = libc_ram_info()?;
    let (swap_usage, swap_total) = libc_swap_info()?;

    let mut res = unsafe { cfio_parse_sample(self.subs, self.chan, &self.info, duration) }?;
    res.memory = MemoryUsage { ram_total, ram_usage, swap_total, swap_usage };

    Ok(res)
  }
}
