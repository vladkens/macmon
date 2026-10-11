//! C API (`include/macmon.h`): metrics and SoC info as JSON for apps in other languages.
//! Build it with `--profile capi` (`panic = "unwind"`): under `panic = "abort"` a panic kills the
//! host app instead of returning `MACMON_STATUS_PANIC`.

use std::any::Any;
use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::metrics::Sampler;
use crate::sources::get_soc_info;

/// `MACMON_ABI_VERSION`, also the `"schema"` key of every JSON document.
const ABI_VERSION: u32 = 1;

const OK: i32 = 0;
const INVALID_ARGUMENT: i32 = 1;
const INIT_FAILED: i32 = 2;
const SAMPLE_FAILED: i32 = 3;
const PANIC: i32 = 4;

// Keys deprecated since 0.10.0: `Metrics` (`metrics.rs`) and `SocInfo` (`sources.rs`).
const DEPRECATED_KEYS: [&str; 12] = [
  "ecpu_freq_mhz",
  "ecpu_scaled_ratio",
  "ecpu_active_ratio",
  "pcpu_freq_mhz",
  "pcpu_scaled_ratio",
  "pcpu_active_ratio",
  "ecpu_cores",
  "pcpu_cores",
  "ecpu_label",
  "pcpu_label",
  "ecpu_freqs",
  "pcpu_freqs",
];

type Status<T = ()> = Result<T, (i32, String)>;

thread_local! {
  static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Opaque `macmon_sampler_t`.
pub struct CSampler {
  sampler: Sampler,
  poisoned: bool, // a call panicked: the sampler state is unknown
}

// Runs `f` catching panics, and sets this thread's error message (cleared on success).
fn guard(f: impl FnOnce() -> Status) -> i32 {
  let rs = catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|e| Err((PANIC, panic_message(e))));
  let (code, msg) = match rs {
    Ok(()) => (OK, None),
    Err((code, msg)) => (code, CString::new(msg.replace('\0', " ")).ok()),
  };
  let _ = LAST_ERROR.try_with(|x| *x.borrow_mut() = msg); // fails only during thread teardown
  code
}

fn panic_message(e: Box<dyn Any + Send>) -> String {
  let msg = e.downcast_ref::<&str>().copied().or(e.downcast_ref::<String>().map(String::as_str));
  format!("panic: {}", msg.unwrap_or("unknown"))
}

// Checks an out pointer and sets it to NULL. Safety: `out` is NULL or valid for writes.
unsafe fn out_ptr<'a, T>(out: *mut *mut T, name: &str) -> Status<&'a mut *mut T> {
  let out = unsafe { out.as_mut() }.ok_or_else(|| (INVALID_ARGUMENT, format!("{name} is NULL")))?;
  *out = ptr::null_mut();
  Ok(out)
}

// `value` without `DEPRECATED_KEYS`, with `"schema"` and, for a sample,
// `"timestamp_ms"`.
fn to_json(value: impl Serialize, timestamp_ms: Option<u64>) -> serde_json::Result<Value> {
  let mut value = serde_json::to_value(value)?;
  if let Value::Object(map) = &mut value {
    map.retain(|k, _| !DEPRECATED_KEYS.contains(&k.as_str()));
    map.insert("schema".into(), ABI_VERSION.into());
    if let Some(timestamp_ms) = timestamp_ms {
      map.insert("timestamp_ms".into(), timestamp_ms.into());
    }
  }
  Ok(value)
}

fn to_c_json(value: impl Serialize, timestamp_ms: Option<u64>, code: i32) -> Status<*mut c_char> {
  let json = to_json(value, timestamp_ms).map_err(|e| (code, e.to_string()))?.to_string();
  Ok(CString::new(json).map_err(|e| (code, e.to_string()))?.into_raw())
}

/// `MACMON_ABI_VERSION` the library was built with.
#[unsafe(no_mangle)]
pub extern "C" fn macmon_abi_version() -> u32 {
  ABI_VERSION
}

/// macmon's version, a static string.
#[unsafe(no_mangle)]
pub extern "C" fn macmon_version() -> *const c_char {
  concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Creates a sampler and primes its baseline, so the first sample covers a real interval.
///
/// # Safety
/// `out_sampler` is NULL or valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn macmon_sampler_new(out_sampler: *mut *mut CSampler) -> i32 {
  guard(|| {
    let out = unsafe { out_ptr(out_sampler, "out_sampler") }?;
    let mut sampler = Sampler::new().map_err(|e| (INIT_FAILED, e.to_string()))?;
    sampler.get_metrics(0).map_err(|e| (INIT_FAILED, e.to_string()))?;
    *out = Box::into_raw(Box::new(CSampler { sampler, poisoned: false }));
    Ok(())
  })
}

/// Frees a sampler; NULL is a no-op.
///
/// # Safety
/// `sampler` is NULL or from `macmon_sampler_new`, not freed yet and not in use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn macmon_sampler_free(sampler: *mut CSampler) {
  if !sampler.is_null() {
    let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(sampler) })));
  }
}

/// Samples metrics `interval_ms` after the previous sample, as a JSON object.
///
/// # Safety
/// `sampler` is NULL or a live sampler not used concurrently; `out_json` is NULL or valid for
/// writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn macmon_sampler_next_json(
  sampler: *mut CSampler,
  interval_ms: u32,
  out_json: *mut *mut c_char,
) -> i32 {
  guard(|| {
    let out = unsafe { out_ptr(out_json, "out_json") }?;
    let s = unsafe { sampler.as_mut() }.ok_or((INVALID_ARGUMENT, "sampler is NULL".into()))?;
    if s.poisoned {
      return Err((PANIC, "sampler poisoned by an earlier panic, only free it".into()));
    }

    s.poisoned = true; // stays set if sampling or serialization panics
    let json = sample_json(&mut s.sampler, interval_ms);
    s.poisoned = false;
    *out = json?;
    Ok(())
  })
}

fn sample_json(sampler: &mut Sampler, interval_ms: u32) -> Status<*mut c_char> {
  let metrics = sampler.get_metrics(interval_ms).map_err(|e| (SAMPLE_FAILED, e.to_string()))?;
  let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
  to_c_json(&metrics, Some(now.as_millis() as u64), SAMPLE_FAILED)
}

/// SoC info as a JSON object.
///
/// # Safety
/// `out_json` is NULL or valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn macmon_get_soc_info_json(out_json: *mut *mut c_char) -> i32 {
  guard(|| {
    let out = unsafe { out_ptr(out_json, "out_json") }?;
    let soc = get_soc_info().map_err(|e| (INIT_FAILED, e.to_string()))?;
    *out = to_c_json(&soc, None, INIT_FAILED)?;
    Ok(())
  })
}

/// Frees a JSON string; NULL is a no-op.
///
/// # Safety
/// `json` is NULL or from this library, not freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn macmon_string_free(json: *mut c_char) {
  if !json.is_null() {
    drop(unsafe { CString::from_raw(json) });
  }
}

/// This thread's last error message, NULL after a success; valid until the next call.
#[unsafe(no_mangle)]
pub extern "C" fn macmon_last_error_message() -> *const c_char {
  let msg = LAST_ERROR.try_with(|x| x.borrow().as_ref().map_or(ptr::null(), |x| x.as_ptr()));
  msg.unwrap_or(ptr::null())
}

#[cfg(test)]
mod tests {
  use std::ffi::CStr;

  use super::*;
  use crate::metrics::Metrics;
  use crate::sources::{CpuTierInfo, SocInfo};

  fn last_error() -> Option<String> {
    let msg = macmon_last_error_message();
    (!msg.is_null()).then(|| unsafe { CStr::from_ptr(msg) }.to_str().unwrap().to_string())
  }

  fn dangling<T>() -> *mut T {
    ptr::NonNull::dangling().as_ptr()
  }

  #[test]
  fn version_and_abi() {
    assert_eq!(macmon_abi_version(), 1);
    let version = unsafe { CStr::from_ptr(macmon_version()) };
    assert_eq!(version.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
  }

  #[test]
  fn null_arguments_are_rejected_and_out_is_cleared() {
    assert_eq!(unsafe { macmon_sampler_new(ptr::null_mut()) }, INVALID_ARGUMENT);
    assert_eq!(last_error().as_deref(), Some("out_sampler is NULL"));

    assert_eq!(unsafe { macmon_get_soc_info_json(ptr::null_mut()) }, INVALID_ARGUMENT);
    assert_eq!(last_error().as_deref(), Some("out_json is NULL"));

    let mut json = dangling::<c_char>();
    assert_eq!(
      unsafe { macmon_sampler_next_json(ptr::null_mut(), 0, &mut json) },
      INVALID_ARGUMENT
    );
    assert!(json.is_null());
    assert_eq!(last_error().as_deref(), Some("sampler is NULL"));

    let rs = unsafe { macmon_sampler_next_json(dangling(), 0, ptr::null_mut()) };
    assert_eq!(rs, INVALID_ARGUMENT);
    assert_eq!(last_error().as_deref(), Some("out_json is NULL"));

    unsafe { macmon_sampler_free(ptr::null_mut()) };
    unsafe { macmon_string_free(ptr::null_mut()) };
  }

  #[test]
  fn error_message_is_set_and_cleared() {
    assert_eq!(guard(|| Err((SAMPLE_FAILED, "no\0sources".into()))), SAMPLE_FAILED);
    assert_eq!(last_error().as_deref(), Some("no sources"));
    assert_eq!(guard(|| Ok(())), OK);
    assert_eq!(last_error(), None);

    assert_eq!(guard(|| panic!("boom {}", 1)), PANIC);
    assert_eq!(last_error().as_deref(), Some("panic: boom 1"));
  }

  #[test]
  fn json_string_round_trips_and_frees() {
    let json = to_c_json(Metrics::default(), Some(5), SAMPLE_FAILED).unwrap();
    let value: Value = serde_json::from_slice(unsafe { CStr::from_ptr(json) }.to_bytes()).unwrap();
    assert_eq!(value["timestamp_ms"], 5);
    unsafe { macmon_string_free(json) };
  }

  fn deprecated_keys(value: &Value) -> Vec<&String> {
    let keys = value.as_object().unwrap().keys();
    keys.filter(|k| k.starts_with("ecpu_") || k.starts_with("pcpu_")).collect()
  }

  #[test]
  fn metrics_json_has_schema_and_timestamp_without_deprecated_keys() {
    let value = to_json(Metrics::default(), Some(1_700_000_000_123)).unwrap();
    assert!(deprecated_keys(&value).is_empty());
    assert_eq!(value["schema"], ABI_VERSION);
    assert_eq!(value["timestamp_ms"], 1_700_000_000_123u64);
    assert!(value["temp"]["cpu_temp_avg"].is_null());
    for key in ["cpu_tiers", "memory", "fans", "cpu_power", "gpu_ram_power", "sys_power"] {
      assert!(value.get(key).is_some(), "{key}");
    }
  }

  #[test]
  #[allow(deprecated)]
  fn soc_info_json_has_schema_without_deprecated_keys() {
    let tier = CpuTierInfo { label: "E".into(), cores: 4, freqs: vec![600, 2000] };
    let soc = SocInfo {
      chip_name: "Apple M2".into(),
      cpu_tiers: vec![tier],
      ecpu_cores: 4,
      ecpu_label: "E".into(),
      ..Default::default()
    };
    let value = to_json(&soc, None).unwrap();
    assert!(deprecated_keys(&value).is_empty());
    assert_eq!(value["schema"], ABI_VERSION);
    assert!(value.get("timestamp_ms").is_none());
    assert_eq!(value["chip_name"], "Apple M2");
    assert_eq!(value["cpu_tiers"][0]["cores"], 4);
  }
}
