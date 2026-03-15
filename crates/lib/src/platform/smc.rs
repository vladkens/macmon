use std::{collections::HashMap, mem::size_of, os::raw::c_void};

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

  pub fn read_key_info(&mut self, key: &str) -> WithError<KeyInfo> {
    let key = Self::parse_key(key)?;
    if let Some(ki) = self.keys.get(&key) {
      return Ok(*ki);
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
      let ival = KeyData { data8: 8, data32: i, ..Default::default() };
      match self.read(&ival) {
        Ok(oval) => keys.push(std::str::from_utf8(&oval.key.to_be_bytes()).unwrap().to_string()),
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
