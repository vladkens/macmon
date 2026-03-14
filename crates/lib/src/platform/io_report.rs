use std::{
  marker::{PhantomData, PhantomPinned},
  mem::MaybeUninit,
  os::raw::c_void,
  ptr::null,
  time::Instant,
};

use core_foundation::{
  array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
  base::{CFRelease, CFTypeRef, kCFAllocatorDefault},
  dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFMutableDictionaryRef,
  },
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

pub struct IOReportSample {
  sample: CFDictionaryRef,
  duration_ms: u64,
  index: isize,
  items: CFArrayRef,
  items_size: isize,
}

impl IOReportSample {
  fn new(data: CFDictionaryRef, duration_ms: u64) -> Self {
    let items = cfdict_get_val(data, "IOReportChannels").unwrap() as CFArrayRef;
    let items_size = unsafe { CFArrayGetCount(items) } as isize;
    Self { sample: data, duration_ms, items, items_size, index: 0 }
  }
}

impl Drop for IOReportSample {
  fn drop(&mut self) {
    unsafe { CFRelease(self.sample as _) };
  }
}

#[derive(Debug)]
pub struct IOReportSampleItem {
  pub group: String,
  pub subgroup: String,
  pub channel: String,
  pub unit: String,
  pub item: CFDictionaryRef,
  duration_ms: u64,
}

impl IOReportSampleItem {
  pub fn watts(&self) -> WithError<f32> {
    let val = unsafe { IOReportSimpleGetIntegerValue(self.item, 0) } as f32;
    let val = val / (self.duration_ms as f32 / 1000.0);
    match self.unit.as_str() {
      "mJ" => Ok(val / 1e3f32),
      "uJ" => Ok(val / 1e6f32),
      "nJ" => Ok(val / 1e9f32),
      _ => Err(format!("Invalid energy unit: {}", self.unit).into()),
    }
  }
}

impl Iterator for IOReportSample {
  type Item = IOReportSampleItem;

  fn next(&mut self) -> Option<Self::Item> {
    if self.index >= self.items_size {
      return None;
    }

    let item = unsafe { CFArrayGetValueAtIndex(self.items, self.index) } as CFDictionaryRef;
    let group = match unsafe { IOReportChannelGetGroup(item) } {
      x if x.is_null() => String::new(),
      x => from_cfstr(x),
    };
    let subgroup = match unsafe { IOReportChannelGetSubGroup(item) } {
      x if x.is_null() => String::new(),
      x => from_cfstr(x),
    };
    let channel = match unsafe { IOReportChannelGetChannelName(item) } {
      x if x.is_null() => String::new(),
      x => from_cfstr(x),
    };
    let unit = from_cfstr(unsafe { IOReportChannelGetUnitLabel(item) }).trim().to_string();

    self.index += 1;
    Some(IOReportSampleItem { group, subgroup, channel, unit, item, duration_ms: self.duration_ms })
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
  sample: CFDictionaryRef,
  sampled_at: Instant,
}

impl IOReport {
  pub fn new(channels: Vec<(&str, Option<&str>)>) -> WithError<Self> {
    let chan = cfio_get_chan(channels)?;
    let subs = cfio_get_subs(chan)?;
    let sample = unsafe { IOReportCreateSamples(subs, chan, null()) };
    if sample.is_null() {
      unsafe {
        CFRelease(chan as _);
        CFRelease(subs as _);
      }
      return Err("Failed to create initial sample".into());
    }

    Ok(Self { subs, chan, sample, sampled_at: Instant::now() })
  }

  pub fn get_sample(&mut self) -> IOReportSample {
    unsafe {
      let next_sample = IOReportCreateSamples(self.subs, self.chan, null());
      let sampled_at = Instant::now();
      let elapsed_ms = sampled_at.duration_since(self.sampled_at).as_millis() as u64;

      let delta = IOReportCreateSamplesDelta(self.sample, next_sample, null());
      CFRelease(self.sample as _);
      self.sample = next_sample;
      self.sampled_at = sampled_at;

      IOReportSample::new(delta, elapsed_ms.max(1))
    }
  }
}

impl Drop for IOReport {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.sample as _);
      CFRelease(self.chan as _);
      CFRelease(self.subs as _);
    }
  }
}
