//! AppleCLPC aggregate energy report IDs for macOS 27.
//!
//! Extracted from 27.0 (26A428) and 27.0.1 (26A434); see
//! docs/clpc-discovery.md for the extraction method.

struct ClpcKeys {
  bundle: &'static str,
  cpu: u32,
  gpu: u32,
  ane: u32,
}

struct ClpcIndices {
  cpu: u32,
  gpu: u32,
  ane: u32,
}

const INDICES_27: ClpcIndices = ClpcIndices { cpu: 16, gpu: 25, ane: 24 };

// Keys identify the aggregate counters, including both dies on Ultra.
const CLPC_KEYS: &[ClpcKeys] = &[
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6000CLPCv3",
    cpu: 0x4747_9059,
    gpu: 0x4519_bf1e,
    ane: 0x9aaa_17f8,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6002CLPC",
    cpu: 0xb501_816b,
    gpu: 0x9e2c_3e8b,
    ane: 0x14d5_a574,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6020CLPC",
    cpu: 0xe301_cd75,
    gpu: 0x8f34_cc7a,
    ane: 0x7415_fb0f,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6022CLPC",
    cpu: 0x76f5_fd21,
    gpu: 0x9062_4499,
    ane: 0x3c09_7307,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6030CLPC",
    cpu: 0xbe03_a01a,
    gpu: 0x472d_6c9c,
    ane: 0xb899_0c38,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6031CLPC",
    cpu: 0xd81e_0084,
    gpu: 0x1085_f98b,
    ane: 0x313a_3be1,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6032CLPC",
    cpu: 0xf4a2_1308,
    gpu: 0x85e8_0513,
    ane: 0x63dc_8e25,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6041CLPC",
    cpu: 0x263c_f1f0,
    gpu: 0x22e4_cf59,
    ane: 0x7849_fb5c,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6050CLPC",
    cpu: 0xd3ab_60bb,
    gpu: 0x27cd_4ce5,
    ane: 0x8c0f_1c59,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6050dCLPC",
    cpu: 0x18b3_010e,
    gpu: 0xedec_a11a,
    ane: 0x188b_d883,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8103CLPCv3",
    cpu: 0x2cdd_31f2,
    gpu: 0x21be_cf42,
    ane: 0x0667_fb81,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8112CLPC",
    cpu: 0xb543_5137,
    gpu: 0x638d_9d52,
    ane: 0xea08_9c36,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8122CLPC",
    cpu: 0x54b5_1c99,
    gpu: 0xce2d_d9dc,
    ane: 0x960a_fe2f,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8132CLPC",
    cpu: 0x4a85_4d94,
    gpu: 0x3b3e_3b79,
    ane: 0x1ff4_a9ec,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8140CLPC",
    cpu: 0x9bf5_4436,
    gpu: 0xef70_589c,
    ane: 0x569d_941b,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8142CLPC",
    cpu: 0x4370_8744,
    gpu: 0x6a01_0933,
    ane: 0xbc5e_fdf0,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8152CLPC",
    cpu: 0x485d_4bfc,
    gpu: 0x1178_4654,
    ane: 0x9380_c6bb,
  },
];

const fn report_id(index: u32, key: u32) -> u64 {
  ((index as u64) << 32) | key as u64
}

pub(crate) fn energy_channels(bundle: &str, os_version: &str) -> Option<[(u64, &'static str); 3]> {
  if os_version.split('.').next() != Some("27") {
    return None;
  }

  let keys = CLPC_KEYS.iter().find(|keys| keys.bundle == bundle)?;
  Some([
    (report_id(INDICES_27.cpu, keys.cpu), "CPU Energy"),
    (report_id(INDICES_27.gpu, keys.gpu), "GPU Energy"),
    (report_id(INDICES_27.ane, keys.ane), "ANE"),
  ])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn selects_current_full_ids_for_each_driver() {
    // Independent full IDs from the 27.0.1 static catalog: M1 Ultra, M2, M5.
    for (bundle, ids) in [
      (
        "com.apple.driver.AppleT6002CLPC",
        [0x0000_0010_b501_816b, 0x0000_0019_9e2c_3e8b, 0x0000_0018_14d5_a574],
      ),
      (
        "com.apple.driver.AppleT8112CLPC",
        [0x0000_0010_b543_5137, 0x0000_0019_638d_9d52, 0x0000_0018_ea08_9c36],
      ),
      (
        "com.apple.driver.AppleT8142CLPC",
        [0x0000_0010_4370_8744, 0x0000_0019_6a01_0933, 0x0000_0018_bc5e_fdf0],
      ),
    ] {
      for version in ["27.0", "27.0.1"] {
        let channels = energy_channels(bundle, version).unwrap();
        assert_eq!(channels.map(|(id, _)| id), ids);
        assert_eq!(channels.map(|(_, name)| name), ["CPU Energy", "GPU Energy", "ANE"]);
      }
    }
  }

  #[test]
  fn leaves_unknown_drivers_and_other_os_versions_to_fallbacks() {
    assert!(energy_channels("com.apple.driver.AppleUnknownCLPC", "27.0.1").is_none());
    assert!(energy_channels("AppleT8112CLPC", "27.0.1").is_none());
    for version in ["", "15.8", "26.6.2", "28.0", "127.0"] {
      assert!(energy_channels("com.apple.driver.AppleT8112CLPC", version).is_none());
    }
  }

  #[test]
  fn driver_mappings_are_unambiguous() {
    let mut bundles = std::collections::HashSet::new();
    for keys in CLPC_KEYS {
      assert!(bundles.insert(keys.bundle), "duplicate driver: {}", keys.bundle);
      assert_ne!(keys.cpu, keys.gpu);
      assert_ne!(keys.cpu, keys.ane);
      assert_ne!(keys.gpu, keys.ane);
    }
  }
}
