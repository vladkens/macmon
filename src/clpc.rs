//! AppleCLPC aggregate energy report IDs for macOS 27.
//!
//! Generated from macOS 27.2 (26B123) by scripts/clpc.py.

struct ClpcKeys {
  bundle: &'static str,
  cpu: u32,
  gpu: u32,
  ane: u32,
  ram: u32,
  dcs: u32,
  chip: u32,
  ave: Option<u32>,
  vdec: Option<u32>,
  isp: Option<u32>,
}

struct ClpcIndices {
  cpu: u32,
  gpu: u32,
  ane: u32,
  ram: u32,
  dcs: u32,
  chip: u32,
  ave: (u32, &'static str),
  isp: (u32, &'static str),
}

// Generated from macOS 27.2 (26B123) by scripts/clpc.py.
const INDICES_27: ClpcIndices = ClpcIndices {
  cpu: 16,
  gpu: 25,
  ane: 24,
  ram: 26,
  dcs: 27,
  chip: 9,
  ave: (60, "mJ"),
  isp: (58, "mJ"),
};

const CLPC_KEYS: &[ClpcKeys] = &[
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6000CLPCv3",
    cpu: 0x4747_9059,
    gpu: 0x4519_bf1e,
    ane: 0x9aaa_17f8,
    ram: 0xd6ca_0cfb,
    dcs: 0x05ca_c729,
    chip: 0x3c7a_f4b5,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6002CLPC",
    cpu: 0xb501_816b,
    gpu: 0x9e2c_3e8b,
    ane: 0x14d5_a574,
    ram: 0x7670_4b08,
    dcs: 0x2d7b_7fb2,
    chip: 0xe16c_1380,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6020CLPC",
    cpu: 0xe301_cd75,
    gpu: 0x8f34_cc7a,
    ane: 0x7415_fb0f,
    ram: 0x2624_4bb6,
    dcs: 0x5f31_a20f,
    chip: 0x9877_104c,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6022CLPC",
    cpu: 0x76f5_fd21,
    gpu: 0x9062_4499,
    ane: 0x3c09_7307,
    ram: 0xe8f2_ddf9,
    dcs: 0xfa86_5a91,
    chip: 0xef56_ddf3,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6030CLPC",
    cpu: 0xbe03_a01a,
    gpu: 0x472d_6c9c,
    ane: 0xb899_0c38,
    ram: 0x2380_9c0f,
    dcs: 0xb594_4c6a,
    chip: 0xd9ac_8450,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6031CLPC",
    cpu: 0xd81e_0084,
    gpu: 0x1085_f98b,
    ane: 0x313a_3be1,
    ram: 0x65ff_699d,
    dcs: 0x10cb_1f6a,
    chip: 0xfc3f_ce99,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6032CLPC",
    cpu: 0xf4a2_1308,
    gpu: 0x85e8_0513,
    ane: 0x63dc_8e25,
    ram: 0x63dc_aae5,
    dcs: 0x8cc1_3c13,
    chip: 0xfaa8_ecc3,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6041CLPC",
    cpu: 0x263c_f1f0,
    gpu: 0x22e4_cf59,
    ane: 0x7849_fb5c,
    ram: 0xfe93_2624,
    dcs: 0x8fc8_575c,
    chip: 0xf708_48a3,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6050CLPC",
    cpu: 0xd3ab_60bb,
    gpu: 0x27cd_4ce5,
    ane: 0x8c0f_1c59,
    ram: 0xf3c8_1737,
    dcs: 0x3d89_affa,
    chip: 0x958c_fda5,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT6050dCLPC",
    cpu: 0x18b3_010e,
    gpu: 0xedec_a11a,
    ane: 0x188b_d883,
    ram: 0xa331_96f9,
    dcs: 0x0b3c_717e,
    chip: 0x9ee7_9a95,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8103CLPCv3",
    cpu: 0x2cdd_31f2,
    gpu: 0x21be_cf42,
    ane: 0x0667_fb81,
    ram: 0xc0c2_0839,
    dcs: 0x31d3_e1e7,
    chip: 0xd685_8d06,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8112CLPC",
    cpu: 0xb543_5137,
    gpu: 0x638d_9d52,
    ane: 0xea08_9c36,
    ram: 0x8b6e_0faa,
    dcs: 0x9a6e_145a,
    chip: 0x824b_cad4,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8122CLPC",
    cpu: 0x54b5_1c99,
    gpu: 0xce2d_d9dc,
    ane: 0x960a_fe2f,
    ram: 0x0b96_41d3,
    dcs: 0x0d80_2b66,
    chip: 0x9709_cb31,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8132CLPC",
    cpu: 0x4a85_4d94,
    gpu: 0x3b3e_3b79,
    ane: 0x1ff4_a9ec,
    ram: 0x9a44_d225,
    dcs: 0xe340_48d2,
    chip: 0x6ab9_90d0,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8140CLPC",
    cpu: 0x9bf5_4436,
    gpu: 0xef70_589c,
    ane: 0x569d_941b,
    ram: 0x035f_5005,
    dcs: 0x5344_2391,
    chip: 0x7f14_616c,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8142CLPC",
    cpu: 0x4370_8744,
    gpu: 0x6a01_0933,
    ane: 0xbc5e_fdf0,
    ram: 0xe136_33aa,
    dcs: 0x2b8d_7fec,
    chip: 0xa65a_b9a8,
    ave: None,
    vdec: None,
    isp: None,
  },
  ClpcKeys {
    bundle: "com.apple.driver.AppleT8152CLPC",
    cpu: 0x485d_4bfc,
    gpu: 0x1178_4654,
    ane: 0x9380_c6bb,
    ram: 0xe17c_13bf,
    dcs: 0x4e80_dc41,
    chip: 0xf6ed_063b,
    ave: Some(0x6541_5645),
    vdec: None,
    isp: Some(0x6549_5350),
  },
];

const fn report_id(index: u32, key: u32) -> u64 {
  ((index as u64) << 32) | key as u64
}

pub(crate) fn energy_channels(
  bundle: &str,
  os_version: &str,
) -> Option<Vec<(u64, &'static str, &'static str)>> {
  if os_version.split('.').next() != Some("27") {
    return None;
  }
  let keys = CLPC_KEYS.iter().find(|keys| keys.bundle == bundle)?;
  let mut channels = vec![(report_id(INDICES_27.cpu, keys.cpu), "CPU Energy", "nJ")];
  channels.push((report_id(INDICES_27.gpu, keys.gpu), "GPU Energy", "nJ"));
  channels.extend([
    (report_id(INDICES_27.ane, keys.ane), "ANE", "nJ"),
    (report_id(INDICES_27.ram, keys.ram), "DRAM", "nJ"),
    (report_id(INDICES_27.dcs, keys.dcs), "DCS", "nJ"),
    (report_id(INDICES_27.chip, keys.chip), "Package Energy", "nJ"),
  ]);
  for (key, index, name) in [
    (keys.ave, Some(INDICES_27.ave), "AVE"),
    (keys.vdec, None, "VDEC"),
    (keys.isp, Some(INDICES_27.isp), "ISP"),
  ] {
    if let (Some(key), Some((index, unit))) = (key, index) {
      channels.push((report_id(index, key), name, unit));
    }
  }
  Some(channels)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn selects_current_full_ids_for_each_driver() {
    for (bundle, ids) in [
      (
        "com.apple.driver.AppleT6002CLPC",
        vec![
          0x00000010b501816b,
          0x000000199e2c3e8b,
          0x0000001814d5a574,
          0x0000001a76704b08,
          0x0000001b2d7b7fb2,
          0x00000009e16c1380,
        ],
      ),
      (
        "com.apple.driver.AppleT8112CLPC",
        vec![
          0x00000010b5435137,
          0x00000019638d9d52,
          0x00000018ea089c36,
          0x0000001a8b6e0faa,
          0x0000001b9a6e145a,
          0x00000009824bcad4,
        ],
      ),
      (
        "com.apple.driver.AppleT8142CLPC",
        vec![
          0x0000001043708744,
          0x000000196a010933,
          0x00000018bc5efdf0,
          0x0000001ae13633aa,
          0x0000001b2b8d7fec,
          0x00000009a65ab9a8,
        ],
      ),
    ] {
      for version in ["27.0", "27.0.1", "27.2"] {
        let channels = energy_channels(bundle, version).unwrap();
        assert_eq!(channels.iter().map(|c| c.0).collect::<Vec<_>>(), ids);
        assert!(channels.iter().all(|c| c.2 == "nJ"));
      }
    }
  }

  #[test]
  fn subscribes_to_separate_media_reports_and_omits_unresolved_components() {
    let channels = energy_channels("com.apple.driver.AppleT8152CLPC", "27.2").unwrap();
    assert!(channels.contains(&(0x0000003a65495350, "ISP", "mJ")));
    assert!(channels.contains(&(0x0000003c65415645, "AVE", "mJ")));
    assert!(!channels.iter().any(|c| matches!(c.1, "GPU SRAM" | "VDEC")));
    let channels = energy_channels("com.apple.driver.AppleT6002CLPC", "27.2").unwrap();
    assert!(channels.contains(&(0x000000199e2c3e8b, "GPU Energy", "nJ")));
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
      assert!(bundles.insert(keys.bundle));
      let channels = energy_channels(keys.bundle, "27.2").unwrap();
      let ids: std::collections::HashSet<_> = channels.iter().map(|c| c.0).collect();
      assert_eq!(ids.len(), channels.len());
    }
  }
}
