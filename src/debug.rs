use core_foundation::base::CFRelease;

use crate::sources::{
  cfdict_keys, cfio_get_props, cfio_get_residencies, cfio_watts, get_dvfs_mhz, run_system_profiler,
  IOHIDSensors, IOReport, IOServiceIterator, SMC,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

fn print_divider(msg: &str) {
  if msg.len() == 0 {
    println!("{}", "-".repeat(80));
    return;
  }

  let len = 80 - msg.len() - 2 - 3;
  println!("\n--- {} {}", msg, "-".repeat(len));
}

pub fn print_debug() -> WithError<()> {
  let out = run_system_profiler()?;

  let chip = out["SPHardwareDataType"][0]["chip_type"].as_str().unwrap().to_string();
  let model = out["SPHardwareDataType"][0]["machine_model"].as_str().unwrap().to_string();
  let os_ver = out["SPSoftwareDataType"][0]["os_version"].as_str().unwrap().to_string();
  let procs = out["SPHardwareDataType"][0]["number_processors"].as_str().unwrap().to_string();
  println!("Chip: {} | Model: {} | OS: {} | {}", chip, model, os_ver, procs);

  print_divider("AppleARMIODevice");
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

  print_divider("IOReport");
  let channels = vec![
    ("Energy Model", None),
    ("CPU Stats", Some("CPU Complex Performance States")),
    ("CPU Stats", Some("CPU Core Performance States")),
    ("GPU Stats", Some("GPU Performance States")),
  ];

  let dur = 100;
  let ior = IOReport::new(channels)?;
  for x in ior.get_sample(dur) {
    let msg = format!("{} :: {} :: {} ({}) =", x.group, x.subgroup, x.channel, x.unit);
    match x.unit.as_str() {
      "24Mticks" => println!("{} {:?}", msg, cfio_get_residencies(x.item)),
      _ => println!("{} {:.2}W", msg, cfio_watts(x.item, &x.unit, dur)?),
    }
  }

  print_divider("SMC temp sensors");
  const FLOAT_TYPE: u32 = 1718383648; // FourCC: "flt "

  let mut smc = SMC::new()?;
  let keys = smc.read_all_keys().unwrap_or(vec![]);
  for key in &keys {
    if !key.starts_with("T") {
      continue;
    }

    let ki = smc.read_key_info(&key)?;
    if !(ki.data_type == FLOAT_TYPE && ki.data_size == 4) {
      continue;
    }

    let val = smc.read_val(&key);
    if val.is_err() {
      continue;
    }

    let val = val.unwrap();
    let val = f32::from_le_bytes(val.data.clone().try_into().unwrap());
    if val < 20.0 || val > 99.0 {
      continue;
    }

    print!("{}={:.2}  ", key, val);
  }

  println!(""); // close previous line

  print_divider("IOHID");
  let hid = IOHIDSensors::new()?;
  for (key, val) in hid.get_metrics() {
    println!("{:>32}: {:6.2}", key, val);
  }

  Ok(())
}
