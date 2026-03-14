use std::{
  marker::{PhantomData, PhantomPinned},
  mem::MaybeUninit,
  os::raw::c_void,
  ptr::null,
};

use core_foundation::{
  array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
  base::{CFRelease, CFTypeRef, kCFAllocatorDefault},
  dictionary::{CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFMutableDictionaryRef},
  string::CFStringRef,
};

use super::{WithError, cfdict_get_val, cfstr, from_cfstr};

type CVoidRef = *const c_void;

#[repr(C)]
struct IOReportSubscription {
  _data: [u8; 0],
  _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}

type IOReportSubscriptionRef = *const IOReportSubscription;

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
  fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
  fn IOReportCopyChannelsInGroup(a: CFStringRef, b: CFStringRef, c: u64, d: u64, e: u64) -> CFDictionaryRef;
  fn IOReportMergeChannels(a: CFDictionaryRef, b: CFDictionaryRef, nil: CFTypeRef);
  fn IOReportCreateSubscription(
    a: CVoidRef,
    b: CFMutableDictionaryRef,
    c: *mut CFMutableDictionaryRef,
    d: u64,
    e: CFTypeRef,
  ) -> IOReportSubscriptionRef;
  fn IOReportCreateSamples(a: IOReportSubscriptionRef, b: CFMutableDictionaryRef, c: CFTypeRef)
    -> CFDictionaryRef;
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

pub fn cfio_get_residencies(item: CFDictionaryRef) -> Vec<(String, i64)> {
  let count = unsafe { IOReportStateGetCount(item) };
  let mut res = Vec::new();

  for i in 0..count {
    let name = unsafe { IOReportStateGetNameForIndex(item, i) };
    let val = unsafe { IOReportStateGetResidency(item, i) };
    res.push((from_cfstr(name), val));
  }

  res
}

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
    let group = cfio_get_group(item);
    let subgroup = cfio_get_subgroup(item);
    let channel = cfio_get_channel(item);
    let unit = from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string();

    self.index += 1;
    Some(IOReportIteratorItem { group, subgroup, channel, unit, item })
  }
}

fn cfio_get_chan(items: Vec<(&str, Option<&str>)>) -> WithError<CFMutableDictionaryRef> {
  if items.is_empty() {
    unsafe {
      let c = IOReportCopyAllChannels(0, 0);
      let r = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, CFDictionaryGetCount(c), c);
      CFRelease(c as _);
      return Ok(r);
    }
  }

  let mut channels = Vec::new();
  for (group, subgroup) in items {
    let gname = cfstr(group);
    let sname = subgroup.map_or(null(), cfstr);
    let chan = unsafe { IOReportCopyChannelsInGroup(gname, sname, 0, 0, 0) };
    channels.push(chan);

    unsafe { CFRelease(gname as _) };
    if subgroup.is_some() {
      unsafe { CFRelease(sname as _) };
    }
  }

  let chan = channels[0];
  for channel in channels.iter().skip(1) {
    unsafe { IOReportMergeChannels(chan, *channel, null()) };
  }

  let size = unsafe { CFDictionaryGetCount(chan) };
  let chan = unsafe { CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, chan) };

  for channel in channels {
    unsafe { CFRelease(channel as _) };
  }

  if cfdict_get_val(chan, "IOReportChannels").is_none() {
    return Err("Failed to get channels".into());
  }

  Ok(chan)
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
}

impl IOReport {
  pub fn new(channels: Vec<(&str, Option<&str>)>) -> WithError<Self> {
    let chan = cfio_get_chan(channels)?;
    let subs = cfio_get_subs(chan)?;
    Ok(Self { subs, chan })
  }

  pub fn get_sample(&self, duration: u64) -> IOReportIterator {
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

impl Drop for IOReport {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.chan as _);
      CFRelease(self.subs as _);
    }
  }
}
