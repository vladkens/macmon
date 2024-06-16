use core_foundation::{
  base::{kCFAllocatorDefault, kCFAllocatorNull, CFRelease, CFTypeRef},
  dictionary::{
    CFDictionaryGetCount, CFDictionaryGetKeysAndValues, CFDictionaryGetValue, CFDictionaryRef,
  },
  string::{kCFStringEncodingUTF8, CFStringCreateWithBytesNoCopy, CFStringGetCString, CFStringRef},
};

pub type CVoidRef = *const std::ffi::c_void;

// pub fn json_save(filepath: &str, val: &Value) {
//   use std::fs::File;
//   use std::io::prelude::*;
//   let val = serde_json::to_string_pretty(&val).unwrap();
//   let mut file = File::create(filepath).unwrap();
//   file.write_all(val.as_bytes()).unwrap();
// }

// pub fn cf_to_json(val: CFTypeRef) -> Value {
//   unsafe {
//     let tid = CFGetTypeID(val);
//     match tid {
//       _ if tid == CFStringGetTypeID() => {
//         json!(CFString::wrap_under_get_rule(val as CFStringRef).to_string())
//       }
//       _ if tid == CFNumberGetTypeID() => {
//         json!(CFNumber::wrap_under_get_rule(val as CFNumberRef).to_i64())
//       }
//       _ if tid == CFBooleanGetTypeID() => {
//         json!(CFBooleanGetValue(val as CFBooleanRef))
//       }
//       _ if tid == CFDictionaryGetTypeID() => {
//         let val = CFDictionary::<CFTypeRef, CFTypeRef>::wrap_under_get_rule(val as CFDictionaryRef);
//         let (keys, vals) = val.get_keys_and_values();

//         let mut map: HashMap<String, Value> = HashMap::new();
//         for (key, value) in keys.iter().zip(vals.iter()) {
//           map.insert(
//             CFString::wrap_under_get_rule(*key as CFStringRef).to_string(),
//             cf_to_json(*value),
//           );
//         }
//         json!(map)
//       }
//       _ if tid == CFArrayGetTypeID() => {
//         let val = CFArray::<CFTypeRef>::wrap_under_get_rule(val as CFArrayRef);
//         let mut arr: Vec<Value> = Vec::new();
//         for x in val.iter() {
//           arr.push(cf_to_json(*x));
//         }
//         json!(arr)
//       }
//       _ if tid == CFDataGetTypeID() => {
//         let val = CFData::wrap_under_get_rule(val as CFDataRef);
//         json!(val.bytes().to_vec())
//       }
//       _ => {
//         eprintln!("UNKNOWN_TYPE: {:?}", tid);
//         CFShow(val);
//         json!(null)
//       }
//     }
//   }
// }

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

pub fn cfdict_get_str(dict: CFDictionaryRef, key: &str) -> Option<String> {
  match cfdict_get_val(dict, key) {
    Some(val) => Some(from_cfstr(val as _)),
    None => None,
  }
}
