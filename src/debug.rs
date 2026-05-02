use core_foundation::base::{CFRelease, CFShow};

use crate::metrics::ioreport_channels_filter;
use crate::sources::{
  IOHIDSensors, IOReport, IOServiceIterator, SMC, cfdict_keys, cfio_get_props,
  cfio_get_residencies, cfio_integer_value, cfio_watts, get_dvfs_mhz, run_system_profiler,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

fn debug_channels(group: &str, _subgroup: &str, _channel: &str, _unit: &str) -> bool {
  group == "Energy Model" || group == "CPU Stats" || group == "GPU Stats"
}

fn print_divider(msg: &str) {
  if msg.is_empty() {
    println!("{}", "-".repeat(80));
    return;
  }

  let len = 80 - msg.len() - 2 - 3;
  println!("\n--- {} {}", msg, "-".repeat(len));
}

pub fn print_debug() -> WithError<()> {
  let out = run_system_profiler()?;

  let chip =
    out["SPHardwareDataType"][0]["chip_type"].as_str().unwrap_or("Unknown chip").to_string();
  let model =
    out["SPHardwareDataType"][0]["machine_model"].as_str().unwrap_or("Unknown model").to_string();
  let os_ver =
    out["SPSoftwareDataType"][0]["os_version"].as_str().unwrap_or("Unknown OS version").to_string();
  let procs = out["SPHardwareDataType"][0]["number_processors"]
    .as_str()
    .unwrap_or("Unknown processors")
    .to_string();
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

        let Some((volts, freqs)) = get_dvfs_mhz(item, &key) else {
          println!("{:>32}: (not found)", key);
          continue;
        };
        let volts = volts.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(" ");
        let freqs = freqs.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(" ");
        println!("{:>32}: (v) {}", key, volts);
        println!("{:>32}: (f) {}", key, freqs);
      }

      unsafe { CFRelease(item as _) }
    }
  }

  print_divider("IOReport");
  let dur = 100;
  let ior = IOReport::new(Some(debug_channels))?;
  for x in ior.get_sample(dur) {
    let subscribed = ioreport_channels_filter(&x.group, &x.subgroup, &x.channel, &x.unit);
    let msg = format!(
      "{} :: {} :: {} ({}{}) =",
      x.group,
      x.subgroup,
      x.channel,
      x.unit,
      if subscribed { ", subscribed" } else { "" }
    );
    match x.unit.as_str() {
      "24Mticks" => println!("{msg} {:?}", cfio_get_residencies(x.item)),
      "mJ" | "uJ" | "nJ" => println!("{msg} {:.2}W", cfio_watts(x.item, &x.unit, dur)?),
      "events" | "B" | "KiB" | "MiB" | "ns" | "us" | "ms" | "s" | "" => {
        println!("{msg} {} {}", cfio_integer_value(x.item), x.unit)
      }
      _ => {
        println!("{msg} {:?}", x.item);
        unsafe { CFShow(x.item as _) };
      }
    }
  }

  print_divider("SMC temp sensors");

  let mut smc = SMC::new()?;
  let keys = smc.read_all_keys().unwrap_or(vec![]);
  for key in &keys {
    if !key.starts_with("T") {
      continue;
    }

    let Ok(val) = smc.read_float_val(key) else { continue };
    // if val < 20.0 || val > 99.0 {
    //   continue;
    // }

    print!("{}={:04.1}  ", key, val);
  }

  println!(); // close previous line

  print_divider("IOHID");
  let hid = IOHIDSensors::new()?;
  for (key, val) in hid.get_metrics() {
    println!("{:>32}: {:6.2}", key, val);
  }

  Ok(())
}
