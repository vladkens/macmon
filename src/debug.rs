use core_foundation::base::CFRelease;

use crate::sources::{
  cfdict_keys, cfio_get_props, get_dvfs_mhz, IOHIDSensors, IOReport, IOServiceIterator, SMC,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

fn divider(msg: &str) {
  if msg.len() == 0 {
    println!("{}", "-".repeat(80));
    return;
  }

  let len = 80 - msg.len() - 2 - 3;
  println!("\n--- {} {}", msg, "-".repeat(len));
}

pub fn print_debug() -> WithError<()> {
  // system_profiler -listDataTypes
  let out = std::process::Command::new("system_profiler")
    .args(&["SPHardwareDataType", "SPDisplaysDataType", "-json"])
    .output()
    .unwrap();

  let out = std::str::from_utf8(&out.stdout).unwrap();
  let out = serde_json::from_str::<serde_json::Value>(out).unwrap();

  let mac_model = out["SPHardwareDataType"][0]["machine_model"].as_str().unwrap().to_string();
  let chip_name = out["SPHardwareDataType"][0]["chip_type"].as_str().unwrap().to_string();
  println!("{} :: {}", chip_name, mac_model);

  divider("AppleARMIODevice");
  for (entry, name) in IOServiceIterator::new("AppleARMIODevice")? {
    if name == "pmgr" {
      let item = cfio_get_props(entry, name)?;
      let mut keys = cfdict_keys(item);
      keys.sort();

      for key in keys {
        if !key.contains("voltage-states") {
          continue;
        }

        let (volts, freqs) = get_dvfs_mhz(item, &key);
        let volts = volts.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(" ");
        let freqs = freqs.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(" ");
        println!("{:>32}: (v) {}", key, volts);
        println!("{:>32}: (f) {}", key, freqs);
      }

      unsafe { CFRelease(item as _) }
    }
  }

  divider("IOReport");
  let channels = vec![
    // ("Energy Model", None),
    ("CPU Stats", Some("CPU Complex Performance States")),
    ("CPU Stats", Some("CPU Core Performance States")),
    ("GPU Stats", Some("GPU Performance States")),
  ];

  let ior = IOReport::new(channels)?;
  for x in ior.get_sample(100) {
    println!("{} :: {} :: {} ({})", x.group, x.subgroup, x.channel, x.unit);
    // println!("{:?}", x);
  }

  divider("IOHID");
  let hid = IOHIDSensors::new()?;
  for (key, val) in hid.get_metrics() {
    println!("{:>32}: {:6.2}", key, val);
  }

  divider("SMC");
  let mut smc = SMC::new()?;
  let keys = smc.read_all_keys().unwrap_or(vec![]);
  for key in &keys {
    let ki = smc.read_key_info(&key)?;
    if ki.data_size != 4 || ki.data_type != 1718383648 {
      continue;
    }

    let val = smc.read_val(&key);
    if val.is_err() {
      continue;
    }

    let val = val.unwrap();
    let val = f32::from_le_bytes(val.data.clone().try_into().unwrap());
    if val < 10.0 || val > 120.0 {
      continue;
    }

    print!("{}={:.2}  ", key, val);
  }

  println!();
  divider("");

  Ok(())
}
