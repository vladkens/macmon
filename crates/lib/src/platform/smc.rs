use std::{mem::size_of, os::raw::c_void};

use super::{IOServiceIterator, WithError};

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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
pub struct SensorVal {
  pub name: String,
  pub unit: String,
  pub data: Vec<u8>,
}

#[allow(clippy::upper_case_acronyms)]
pub struct SMC {
  conn: u32,
  keys: Vec<(u32, KeyInfo)>,
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

    Ok(Self { conn, keys: Vec::new() })
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

  pub fn key_by_index(&self, index: u32) -> WithError<String> {
    let ival = KeyData { data8: 8, data32: index, ..Default::default() };
    let oval = self.read(&ival)?;
    Ok(std::str::from_utf8(&oval.key.to_be_bytes()).unwrap().to_string())
  }

  pub fn read_key_info(&mut self, key: &str) -> WithError<KeyInfo> {
    if key.len() != 4 {
      return Err("SMC key must be 4 bytes long".into());
    }

    let key = key.bytes().fold(0, |acc, x| (acc << 8) + x as u32);
    if let Some((_, ki)) = self.keys.iter().find(|(cached_key, _)| *cached_key == key) {
      return Ok(*ki);
    }

    let ival = KeyData { data8: 9, key, ..Default::default() };
    let oval = self.read(&ival)?;
    self.keys.push((key, oval.key_info));
    Ok(oval.key_info)
  }

  pub fn read_val(&mut self, key: &str) -> WithError<SensorVal> {
    let name = key.to_string();
    let key_info = self.read_key_info(key)?;
    let key = key.bytes().fold(0, |acc, x| (acc << 8) + x as u32);

    let ival = KeyData { data8: 5, key, key_info, ..Default::default() };
    let oval = self.read(&ival)?;

    Ok(SensorVal {
      name,
      unit: std::str::from_utf8(&key_info.data_type.to_be_bytes()).unwrap().to_string(),
      data: oval.bytes[0..key_info.data_size as usize].to_vec(),
    })
  }

  pub fn read_all_keys(&mut self) -> WithError<Vec<String>> {
    let val = self.read_val("#KEY")?;
    let val = u32::from_be_bytes(val.data[0..4].try_into().unwrap());

    let mut keys = Vec::new();
    for i in 0..val {
      let key = self.key_by_index(i)?;
      let val = self.read_val(&key);
      if val.is_err() {
        continue;
      }

      keys.push(val.unwrap().name);
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
