use core_foundation::{
  array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
  base::{kCFAllocatorDefault, kCFAllocatorNull, CFAllocatorRef, CFRelease, CFTypeRef},
  dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryGetKeysAndValues,
    CFDictionaryGetValue, CFDictionaryRef, CFMutableDictionaryRef,
  },
  string::{kCFStringEncodingUTF8, CFStringCreateWithBytesNoCopy, CFStringGetCString, CFStringRef},
};
use std::marker::{PhantomData, PhantomPinned};
use std::mem::MaybeUninit;
use std::ptr::null;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;
type CVoidRef = *const std::ffi::c_void;

// MARK: ioreport_bindings

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
  fn IOReportChannelGetGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetSubGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetChannelName(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportSimpleGetIntegerValue(a: CFDictionaryRef, b: i32) -> i64;
  fn IOReportChannelGetUnitLabel(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportStateGetCount(a: CFDictionaryRef) -> i32;
  fn IOReportStateGetNameForIndex(a: CFDictionaryRef, b: i32) -> CFStringRef;
  fn IOReportStateGetResidency(a: CFDictionaryRef, b: i32) -> i64;
}

// MARK: cf_utils

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

pub fn from_cfstr(val: CFStringRef) -> String {
  unsafe {
    let mut buf = Vec::with_capacity(128);
    if CFStringGetCString(val, buf.as_mut_ptr(), 128, kCFStringEncodingUTF8) == 0 {
      panic!("Failed to convert CFString to CString");
    }
    std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().to_string()
  }
}

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

// MARK: io_helpers

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

pub fn cfio_get_props(entry: u32, name: String) -> WithError<CFDictionaryRef> {
  unsafe {
    let mut props: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
    if IORegistryEntryCreateCFProperties(entry, props.as_mut_ptr(), kCFAllocatorDefault, 0) != 0 {
      return Err(format!("Failed to get properties for {}", name).into());
    }

    Ok(props.assume_init())
  }
}

pub fn cfio_get_residencies(item: CFDictionaryRef) -> Vec<(String, i64)> {
  let count = unsafe { IOReportStateGetCount(item) };
  let mut res = vec![];

  for i in 0..count {
    let name = unsafe { IOReportStateGetNameForIndex(item, i) };
    let val = unsafe { IOReportStateGetResidency(item, i) };
    res.push((from_cfstr(name), val));
  }

  res
}

pub fn get_watts(item: CFDictionaryRef, unit: &String, duration: u64) -> WithError<f32> {
  let val = unsafe { IOReportSimpleGetIntegerValue(item, 0) } as f32;
  let val = val / (duration as f32 / 1000.0);
  match unit.as_str() {
    "mJ" => Ok(val / 1e3f32),
    "nJ" => Ok(val / 1e9f32),
    _ => Err(format!("Invalid energy unit: {}", unit).into()),
  }
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
    // cfio_iter_next(self.existing)

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
}

impl IOReportIterator {
  pub fn new(data: CFDictionaryRef) -> Self {
    let items = cfdict_get_val(data, "IOReportChannels").unwrap() as CFArrayRef;
    let items_size = unsafe { CFArrayGetCount(items) } as isize;
    Self { sample: data, items, items_size, index: 0 }
  }
}

impl Drop for IOReportIterator {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.sample as _);
    }
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

    let group = cfio_get_group(item);
    let subgroup = cfio_get_subgroup(item);
    let channel = cfio_get_channel(item);
    let unit = from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string();

    self.index += 1;
    Some(IOReportIteratorItem { group, subgroup, channel, unit, item })
  }
}

// MARK: IOReportSampler

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

pub struct IOReportSampler {
  subs: IOReportSubscriptionRef,
  chan: CFMutableDictionaryRef,
}

impl IOReportSampler {
  pub fn new(channels: Vec<(&str, Option<&str>)>) -> WithError<Self> {
    let chan = unsafe { cfio_get_chan(channels)? };
    let subs = unsafe { cfio_get_subs(chan)? };

    Ok(Self { subs, chan })
  }

  pub fn sample(&self, duration: u64) -> IOReportIterator {
    unsafe {
      let sample1 = IOReportCreateSamples(self.subs, self.chan, null());
      std::thread::sleep(std::time::Duration::from_millis(duration));
      let sample2 = IOReportCreateSamples(self.subs, self.chan, null());

      let sample3 = IOReportCreateSamplesDelta(sample1, sample2, null());
      CFRelease(sample1 as _);
      CFRelease(sample2 as _);
      IOReportIterator::new(sample3)
    }
  }
}

impl Drop for IOReportSampler {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.chan as _);
      CFRelease(self.subs as _);
    }
  }
}
