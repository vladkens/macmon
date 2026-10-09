//! Diagnostic report used by the `macmon debug` command.

use std::collections::BTreeMap;
use std::time::Duration;

use core_foundation::base::{CFRelease, CFShow};

use crate::shared::{ioreport_channels_filter, is_clpc_energy_channel, is_pmp_ane_channel};
use crate::sources::{
  HwInfo, IOHIDSensors, IOReport, IOServiceIterator, SMC, cfdict_keys, cfio_get_props,
  cfio_get_residencies, cfio_integer_value, cfio_watts, cpu_cluster_types, get_dvfs_mhz,
  hw_from_profiler_report, hw_native, is_pmgr_node, libc_ram, libc_swap, profiler_report,
  sysctl_str, sysctl_u32,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

fn debug_channels(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
  group == "Energy Model"
    || group == "Energy Counters"
    || group == "CPU Stats"
    || group == "GPU Stats"
    || is_pmp_ane_channel(group, subgroup, channel, unit)
    || is_clpc_energy_channel(group, subgroup, channel, unit)
}

fn print_divider(msg: &str) {
  if msg.is_empty() {
    println!("{}", "-".repeat(80));
    return;
  }

  let len = 80 - msg.len() - 2 - 3;
  println!("\n--- {} {}", msg, "-".repeat(len));
}

// Native and Profiler side by side; `-` in the Profiler column when system_profiler failed.
fn print_hw(native: &HwInfo, profiler: Option<&HwInfo>) {
  println!("{:<8} {:<24} Profiler (deprecated)", "", "Native");
  let row = |name: &str, value: fn(&HwInfo) -> String| {
    println!("{name:<8} {:<24} {}", value(native), profiler.map_or("-".into(), value));
  };
  row("Chip", |x| x.chip_name.clone());
  row("Model", |x| x.mac_model.clone());
  row("Memory", |x| format!("{} GB", x.memory_gb));
  row("CPU", |x| format!("{}{} + {}{}", x.ecpu_cores, x.ecpu_label, x.pcpu_cores, x.pcpu_label));
  row("GPU", |x| format!("{} cores", x.gpu_cores));
}

// `E x6, M x4, P x2` for the cluster-type of each CPU core.
fn cluster_type_counts(types: &[String]) -> String {
  let mut counts = BTreeMap::<&str, usize>::new();
  for x in types {
    *counts.entry(x).or_default() += 1;
  }
  if counts.is_empty() {
    return "-".into();
  }
  counts.iter().map(|(x, n)| format!("{x} x{n}")).collect::<Vec<_>>().join(", ")
}

pub fn print_debug() -> WithError<()> {
  let os_ver = sysctl_str("kern.osproductversion").unwrap_or("Unknown".into());
  let os_build = sysctl_str("kern.osversion").unwrap_or("Unknown".into());
  println!("macmon {} | OS: macOS {os_ver} ({os_build})", env!("CARGO_PKG_VERSION"));

  print_divider("Hardware");
  let report = profiler_report();
  let profiler = report.as_ref().map(hw_from_profiler_report);
  let native = hw_native();
  match &native {
    Ok(native) => print_hw(native, profiler.as_ref().ok()),
    Err(err) => println!("Native: error={err}"),
  }
  if let Err(err) = &profiler {
    println!("Profiler: error={err}");
  }

  // Inputs of the CPU rows (perflevels for Native, number_processors for Profiler), and the
  // IORegistry core types as an independent reference
  print_divider("CPU topology");
  let nperflevels = sysctl_u32("hw.nperflevels");
  println!("{:<18} {}", "hw.nperflevels", nperflevels.map_or("-".into(), |x| x.to_string()));
  for i in 0..nperflevels.unwrap_or(0) {
    let name = sysctl_str(&format!("hw.perflevel{i}.name")).unwrap_or("-".into());
    let cores = sysctl_u32(&format!("hw.perflevel{i}.physicalcpu"));
    let cores = cores.map_or("-".into(), |x| x.to_string());
    println!("{:<18} {name} x{cores}", format!("hw.perflevel{i}"));
  }
  match cpu_cluster_types() {
    Ok(types) => println!("{:<18} {}", "cluster-type", cluster_type_counts(&types)),
    Err(err) => println!("{:<18} error={err}", "cluster-type"),
  }
  let procs =
    report.as_ref().ok().and_then(|x| x["SPHardwareDataType"][0]["number_processors"].as_str());
  println!("{:<18} {}", "number_processors", procs.unwrap_or("-"));

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
    if is_pmgr_node(&name) {
      println!("[{name}]");
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

#[cfg(test)]
mod tests {
  use super::cluster_type_counts;

  #[test]
  fn counts_cluster_types() {
    let types = |x: &str| x.chars().map(String::from).collect::<Vec<_>>();
    // cpu0-7 of this M2, and cpu0-11 of an M6 (exelban/stats#3668)
    assert_eq!(cluster_type_counts(&types("EEEEPPPP")), "E x4, P x4");
    assert_eq!(cluster_type_counts(&types("EEEEEEPPMMMM")), "E x6, M x4, P x2");
    assert_eq!(cluster_type_counts(&types("EE-")), "- x1, E x2");
    assert_eq!(cluster_type_counts(&[]), "-");
  }
}
