use std::{
  marker::{PhantomData, PhantomPinned},
  mem::MaybeUninit,
  os::raw::c_void,
  ptr::null,
  time::Instant,
};

use core_foundation::{
  array::{
    CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef,
    kCFTypeArrayCallBacks,
  },
  base::{CFRelease, CFTypeRef, kCFAllocatorDefault},
  dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFDictionarySetValue,
    CFMutableDictionaryRef,
  },
  string::CFStringRef,
};

use super::{WithError, cfdict_get_val, cfstr, from_cfstr};
use crate::diag::startup_log;

type CVoidRef = *const c_void;

#[repr(C)]
struct IOReportSubscription {
  _data: [u8; 0],
  _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}

type IOReportSubscriptionRef = *const IOReportSubscription;
// Arguments: (group, subgroup, channel, unit)
pub type ChannelFilter = fn(&str, &str, &str, &str) -> bool;

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
  fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
  fn IOReportCreateSubscription(
    a: CVoidRef,
    b: CFMutableDictionaryRef,
    c: *mut CFMutableDictionaryRef,
    d: u64,
    e: CFTypeRef,
  ) -> IOReportSubscriptionRef;
  fn IOReportCreateSamples(
    a: IOReportSubscriptionRef,
    b: CFMutableDictionaryRef,
    c: CFTypeRef,
  ) -> CFDictionaryRef;
  fn IOReportCreateSamplesDelta(
    a: CFDictionaryRef,
    b: CFDictionaryRef,
    c: CFTypeRef,
  ) -> CFDictionaryRef;
  fn IOReportChannelGetGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetSubGroup(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportChannelGetChannelName(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportSimpleGetIntegerValue(a: CFDictionaryRef, b: i32) -> i64;
  fn IOReportChannelGetUnitLabel(a: CFDictionaryRef) -> CFStringRef;
  fn IOReportStateGetCount(a: CFDictionaryRef) -> i32;
  fn IOReportStateGetNameForIndex(a: CFDictionaryRef, b: i32) -> CFStringRef;
  fn IOReportStateGetResidency(a: CFDictionaryRef, b: i32) -> i64;
}

pub fn cfio_collect_residencies(channel_item: CFDictionaryRef) -> Vec<(String, i64)> {
  let count = unsafe { IOReportStateGetCount(channel_item) };
  let mut residencies = Vec::new();

  for i in 0..count {
    let name = unsafe { IOReportStateGetNameForIndex(channel_item, i) };
    let residency = unsafe { IOReportStateGetResidency(channel_item, i) };
    residencies.push((from_cfstr(name), residency));
  }

  residencies
}

fn channel_group(channel_item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetGroup(channel_item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn channel_subgroup(channel_item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetSubGroup(channel_item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn channel_name(channel_item: CFDictionaryRef) -> String {
  match unsafe { IOReportChannelGetChannelName(channel_item) } {
    x if x.is_null() => String::new(),
    x => from_cfstr(x),
  }
}

fn channel_unit(channel_item: CFDictionaryRef) -> String {
  from_cfstr(unsafe { IOReportChannelGetUnitLabel(channel_item) }).trim().to_string()
}

pub struct IOReportSample {
  sample_delta: CFDictionaryRef,
  duration_ms: u64,
  next_index: isize,
  channel_items: CFArrayRef,
  channel_count: isize,
}

impl IOReportSample {
  fn new(sample_delta: CFDictionaryRef, duration_ms: u64) -> Self {
    let channel_items = cfdict_get_val(sample_delta, "IOReportChannels").unwrap() as CFArrayRef;
    let channel_count = unsafe { CFArrayGetCount(channel_items) } as isize;
    Self { sample_delta, duration_ms, channel_items, channel_count, next_index: 0 }
  }
}

impl Drop for IOReportSample {
  fn drop(&mut self) {
    unsafe { CFRelease(self.sample_delta as _) };
  }
}

#[derive(Debug)]
pub struct IOReportSampleItem {
  pub group: String,
  pub subgroup: String,
  pub channel: String,
  pub unit: String,
  pub channel_item: CFDictionaryRef,
  duration_ms: u64,
}

impl IOReportSampleItem {
  pub fn watts(&self) -> WithError<f32> {
    let energy_value = unsafe { IOReportSimpleGetIntegerValue(self.channel_item, 0) } as f32;
    let power_watts = energy_value / (self.duration_ms as f32 / 1000.0);
    match self.unit.as_str() {
      "mJ" => Ok(power_watts / 1e3f32),
      "uJ" => Ok(power_watts / 1e6f32),
      "nJ" => Ok(power_watts / 1e9f32),
      _ => Err(format!("Invalid energy unit: {}", self.unit).into()),
    }
  }
}

impl Iterator for IOReportSample {
  type Item = IOReportSampleItem;

  fn next(&mut self) -> Option<Self::Item> {
    if self.next_index >= self.channel_count {
      return None;
    }

    let channel_item =
      unsafe { CFArrayGetValueAtIndex(self.channel_items, self.next_index) } as CFDictionaryRef;
    let group = channel_group(channel_item);
    let subgroup = channel_subgroup(channel_item);
    let channel = channel_name(channel_item);
    let unit = channel_unit(channel_item);

    self.next_index += 1;
    Some(IOReportSampleItem {
      group,
      subgroup,
      channel,
      unit,
      channel_item,
      duration_ms: self.duration_ms,
    })
  }
}

fn cfio_copy_filtered_channels(filter: Option<ChannelFilter>) -> WithError<CFMutableDictionaryRef> {
  let all_channels = unsafe { IOReportCopyAllChannels(0, 0) };
  let Some(channel_array) = cfdict_get_val(all_channels, "IOReportChannels") else {
    unsafe { CFRelease(all_channels as _) };
    return Err("Failed to get channels".into());
  };
  let channel_array = channel_array as CFArrayRef;

  startup_log(format!("lib io_report: collected {} channels", unsafe {
    CFArrayGetCount(channel_array)
  }));

  let size = unsafe { CFDictionaryGetCount(all_channels) };
  let selected_channels =
    unsafe { CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, all_channels) };

  let count = unsafe { CFArrayGetCount(channel_array) };
  let selected_channel_array =
    unsafe { CFArrayCreateMutable(kCFAllocatorDefault, count, &kCFTypeArrayCallBacks) };

  for idx in 0..count {
    let channel_item = unsafe { CFArrayGetValueAtIndex(channel_array, idx) } as CFDictionaryRef;
    let group = channel_group(channel_item);
    let subgroup = channel_subgroup(channel_item);
    let channel = channel_name(channel_item);
    let unit = channel_unit(channel_item);
    let keep = filter.is_none_or(|filter| filter(&group, &subgroup, &channel, &unit));
    if keep {
      unsafe { CFArrayAppendValue(selected_channel_array, channel_item as _) };
    }
  }

  let channels_key = cfstr("IOReportChannels");
  let selected_channel_count = unsafe { CFArrayGetCount(selected_channel_array) };
  for idx in 0..selected_channel_count {
    let channel_item =
      unsafe { CFArrayGetValueAtIndex(selected_channel_array, idx) } as CFDictionaryRef;
    startup_log(format!(
      "lib io_report: [{}] group='{}' subgroup='{}' channel='{}' unit='{}'",
      idx,
      channel_group(channel_item),
      channel_subgroup(channel_item),
      channel_name(channel_item),
      channel_unit(channel_item)
    ));
  }
  unsafe {
    CFDictionarySetValue(selected_channels, channels_key as _, selected_channel_array as _);
    CFRelease(channels_key as _);
    CFRelease(selected_channel_array as _);
    CFRelease(all_channels as _);
  }

  startup_log(format!("lib io_report: subscribing to {} channels", selected_channel_count));

  Ok(selected_channels)
}

fn cfio_create_subscription(
  channels: CFMutableDictionaryRef,
) -> WithError<IOReportSubscriptionRef> {
  let mut subscription_output: MaybeUninit<CFMutableDictionaryRef> = MaybeUninit::uninit();
  let subscription = unsafe {
    IOReportCreateSubscription(null(), channels, subscription_output.as_mut_ptr(), 0, null())
  };
  if subscription.is_null() {
    return Err("Failed to create subscription".into());
  }

  unsafe { subscription_output.assume_init() };
  Ok(subscription)
}

pub struct IOReport {
  subscription: IOReportSubscriptionRef,
  channels: CFMutableDictionaryRef,
  previous_sample: CFDictionaryRef,
  last_sampled_at: Instant,
}

impl IOReport {
  pub fn new(filter: Option<ChannelFilter>) -> WithError<Self> {
    let channels = cfio_copy_filtered_channels(filter)?;
    let subscription = cfio_create_subscription(channels)?;
    let previous_sample = unsafe { IOReportCreateSamples(subscription, channels, null()) };
    if previous_sample.is_null() {
      unsafe {
        CFRelease(channels as _);
        CFRelease(subscription as _);
      }
      return Err("Failed to create initial sample".into());
    }

    Ok(Self { subscription, channels, previous_sample, last_sampled_at: Instant::now() })
  }

  pub fn next_sample(&mut self) -> IOReportSample {
    unsafe {
      let next_sample = IOReportCreateSamples(self.subscription, self.channels, null());
      let sampled_at = Instant::now();
      let elapsed_ms = sampled_at.duration_since(self.last_sampled_at).as_millis() as u64;

      let sample_delta = IOReportCreateSamplesDelta(self.previous_sample, next_sample, null());
      CFRelease(self.previous_sample as _);
      self.previous_sample = next_sample;
      self.last_sampled_at = sampled_at;

      IOReportSample::new(sample_delta, elapsed_ms.max(1))
    }
  }
}

impl Drop for IOReport {
  fn drop(&mut self) {
    unsafe {
      CFRelease(self.previous_sample as _);
      CFRelease(self.channels as _);
      CFRelease(self.subscription as _);
    }
  }
}
