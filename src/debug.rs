use core_foundation::base::{CFRelease, CFShow};
use std::time::Duration;

use crate::shared::ioreport_channels_filter;
use crate::sources::{
  HwInfo, IOHIDSensors, IOReport, IOServiceIterator, SMC, cfdict_keys, cfio_get_props,
  cfio_get_residencies, cfio_integer_value, cfio_watts, get_dvfs_mhz, hw_from_profiler, hw_native,
  libc_ram, libc_swap, sysctl_str,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

fn debug_channels(group: &str, _subgroup: &str, _channel: &str, _unit: &str) -> bool {
  group == "Energy Model"
    || group == "Energy Counters"
    || group == "CPU Stats"
    || group == "GPU Stats"
}

fn print_divider(msg: &str) {
  if msg.is_empty() {
    println!("{}", "-".repeat(80));
    return;
  }

  let len = 80 - msg.len() - 2 - 3;
  println!("\n--- {} {}", msg, "-".repeat(len));
}

fn print_hw_row(name: &str, profiler: impl std::fmt::Display, native: impl std::fmt::Display) {
  println!("{name:<8} {profiler:<20} {native}");
}

fn print_hw(profiler: &HwInfo, native: &HwInfo) {
  println!("{:<8} {:<20} Native", "", "Profiler");
  print_hw_row("Chip", &profiler.chip_name, &native.chip_name);
  print_hw_row("Model", &profiler.mac_model, &native.mac_model);
  print_hw_row("Memory", format!("{} GB", profiler.memory_gb), format!("{} GB", native.memory_gb));
  print_hw_row(
    "CPU",
    format!(
      "{}{} + {}{}",
      profiler.ecpu_cores, profiler.ecpu_label, profiler.pcpu_cores, profiler.pcpu_label
    ),
    format!(
      "{}{} + {}{}",
      native.ecpu_cores, native.ecpu_label, native.pcpu_cores, native.pcpu_label
    ),
  );
  print_hw_row(
    "GPU",
    format!("{} cores", profiler.gpu_cores),
    format!("{} cores", native.gpu_cores),
  );
}

pub fn print_debug() -> WithError<()> {
  let os_ver = sysctl_str("kern.osproductversion").unwrap_or("Unknown".into());
  let os_build = sysctl_str("kern.osversion").unwrap_or("Unknown".into());
  println!("macmon {} | OS: macOS {os_ver} ({os_build})", env!("CARGO_PKG_VERSION"));

  print_divider("Hardware");
  let profiler = hw_from_profiler();
  let native = hw_native();
  match (&profiler, &native) {
    (Ok(profiler), Ok(native)) => print_hw(profiler, native),
    _ => {
      println!("Profiler: {profiler:?}");
      println!("Native: {native:?}");
    }
  }

  print_divider("Memory");
  match libc_ram() {
    Ok((used, total)) => println!("RAM  used={used} bytes total={total} bytes"),
    Err(err) => println!("RAM  error={err}"),
  }
  match libc_swap() {
    Ok((used, total)) => println!("Swap used={used} bytes total={total} bytes"),
    Err(err) => println!("Swap error={err}"),
  }

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
  let ior = IOReport::with_filter(Some(debug_channels))?;
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
      "mJ" | "uJ" | "nJ" => {
        println!("{msg} {:.2}W", cfio_watts(x.item, &x.unit, Duration::from_millis(dur))?)
      }
      "events" | "B" | "KiB" | "MiB" | "ns" | "us" | "ms" | "s" | "" => {
        println!("{msg} {} {}", cfio_integer_value(x.item), x.unit)
      }
      _ => {
        println!("{msg} {:?}", x.item);
        unsafe { CFShow(x.item as _) };
      }
    }
  }

  let mut smc = SMC::new()?;
  print_divider("SMC system sensors");
  match smc.read_float_val("PSTR") {
    Ok(watts) => println!("PSTR={watts:.2}W"),
    Err(err) => println!("PSTR error={err}"),
  }

  print_divider("SMC temp sensors");
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

  print_divider("SMC fan sensors");
  for key in &keys {
    let is_fan_key = key.len() == 4 && key.starts_with('F');
    let is_fan_id_key = key.len() == 4
      && key.starts_with('F')
      && key.as_bytes()[1].is_ascii_digit()
      && key.ends_with("ID");
    if !(is_fan_key || is_fan_id_key) {
      continue;
    }

    let ki = smc.read_key_info(key)?;
    let val = smc.read_val(key);
    if val.is_err() {
      continue;
    }

    let val = val.unwrap();
    println!("{} type={} size={} bytes={:?}", key, val.unit, ki.data_size, val.data);
  }

  print_divider("IOHID");
  let hid = IOHIDSensors::new()?;
  for (key, val) in hid.get_metrics() {
    println!("{:>32}: {:6.2}", key, val);
  }

  Ok(())
}
