use core_foundation::{
  base::{CFRelease, CFTypeRef, kCFAllocatorDefault, kCFAllocatorNull},
  dictionary::{
    CFDictionaryGetCount, CFDictionaryGetKeysAndValues, CFDictionaryGetValue, CFDictionaryRef,
  },
  number::{CFNumberCreate, CFNumberRef, kCFNumberSInt32Type},
  string::{CFStringCreateWithBytesNoCopy, CFStringGetCString, CFStringRef, kCFStringEncodingUTF8},
};

pub fn cfnum(val: i32) -> CFNumberRef {
  unsafe { CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &val as *const i32 as _) }
}

pub fn cfstr(val: &str) -> CFStringRef {
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

    if val.is_null() { None } else { Some(val) }
  }
}
