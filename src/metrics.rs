use core_foundation::dictionary::CFDictionaryRef;

use crate::sources::{
  cfio_get_residencies, cfio_watts, libc_ram_info, libc_swap_info, IOHIDSensors, IOReport, SocInfo,
  SMC,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const CPU_POWER_SUBG: &str = "CPU Complex Performance States";
const GPU_POWER_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default)]
pub struct TempMetrics {
  pub cpu_temp_avg: f32, // Celsius
  pub gpu_temp_avg: f32, // Celsius
}

#[derive(Debug, Default)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default)]
pub struct Metrics {
  pub temp: TempMetrics,
  pub memory: MemMetrics,
  pub ecpu_usage: (u32, f32), // freq, percent_from_max
  pub pcpu_usage: (u32, f32), // freq, percent_from_max
  pub gpu_usage: (u32, f32),  // freq, percent_from_max
  pub cpu_power: f32,         // Watts
  pub gpu_power: f32,         // Watts
  pub ane_power: f32,         // Watts
  pub all_power: f32,         // Watts
}

// MARK: Helpers

fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  return if b == zero { zero } else { a / b };
}

fn calc_freq(item: CFDictionaryRef, freqs: &Vec<u32>) -> (u32, f32) {
  let residencies = cfio_get_residencies(item);
  let (len1, len2) = (residencies.len(), freqs.len());
  assert!(len1 > len2, "cacl_freq invalid data: {} vs {}", len1, len2); // todo?

  // first is IDLE for CPU and OFF for GPU
  let usage = residencies.iter().map(|x| x.1 as f64).skip(1).sum::<f64>();
  let total = residencies.iter().map(|x| x.1 as f64).sum::<f64>();
  let count = freqs.len();

  let mut freq = 0f64;
  for i in 0..count {
    let percent = zero_div(residencies[i + 1].1 as _, usage);
    freq += percent * freqs[i] as f64;
  }

  let percent = zero_div(usage, total);
  let max_freq = freqs.last().unwrap().clone() as f64;
  let from_max = (freq * percent) / max_freq;

  (freq as u32, from_max as f32)
}

fn init_smc() -> WithError<(SMC, Vec<String>, Vec<String>)> {
  let mut smc = SMC::new()?;

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    let key = match smc.read_key_info(&name) {
      Ok(key) => key,
      Err(_) => continue,
    };

    if key.data_size != 4 || key.data_type != 1718383648 {
      continue;
    }

    let _ = match smc.read_val(&name) {
      Ok(val) => val,
      Err(_) => continue,
    };

    match name {
      name if name.starts_with("Tp") => cpu_sensors.push(name.clone()),
      name if name.starts_with("Tg") => gpu_sensors.push(name.clone()),
      _ => (),
    }
  }

  // println!("{} {}", cpu_sensors.len(), gpu_sensors.len());
  Ok((smc, cpu_sensors, gpu_sensors))
}

// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  ior: IOReport,
  hid: IOHIDSensors,
  smc: SMC,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let channels = vec![
      ("Energy Model", None),              // cpu+gpu+ane power
      ("CPU Stats", Some(CPU_POWER_SUBG)), // cpu freq by cluster
      ("GPU Stats", Some(GPU_POWER_SUBG)), // gpu freq
    ];

    let soc = SocInfo::new()?;
    let ior = IOReport::new(channels)?;
    let hid = IOHIDSensors::new()?;
    let (smc, smc_cpu_keys, smc_gpu_keys) = init_smc()?;

    Ok(Sampler { soc, ior, hid, smc, smc_cpu_keys, smc_gpu_keys })
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = self.smc.read_val(sensor)?;
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
      cpu_metrics.push(val);
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = self.smc.read_val(sensor)?;
      let val = f32::from_le_bytes(val.data[0..4].try_into().unwrap());
      gpu_metrics.push(val);
    }

    let cpu_temp_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_temp_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp_hid(&mut self) -> WithError<TempMetrics> {
    let metrics = self.hid.get_metrics();

    let mut cpu_values = Vec::new();
    let mut gpu_values = Vec::new();

    for (name, value) in &metrics {
      if name.starts_with("pACC MTR Temp Sensor") || name.starts_with("eACC MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        cpu_values.push(*value);
        continue;
      }

      if name.starts_with("GPU MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        gpu_values.push(*value);
        continue;
      }
    }

    let cpu_temp_avg = zero_div(cpu_values.iter().sum(), cpu_values.len() as f32);
    let gpu_temp_avg = zero_div(gpu_values.iter().sum(), gpu_values.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
  }

  fn get_temp(&mut self) -> WithError<TempMetrics> {
    // HID for M1, SMC for M2/M3
    match self.smc_cpu_keys.len() > 0 {
      true => self.get_temp_smc(),
      false => self.get_temp_hid(),
    }
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram_info()?;
    let (swap_usage, swap_total) = libc_swap_info()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  pub fn get_metrics(&mut self, duration: u64) -> WithError<Metrics> {
    let mut rs = Metrics::default();

    for x in self.ior.get_sample(duration) {
      if x.group == "CPU Stats" && x.subgroup == CPU_POWER_SUBG {
        match x.channel.as_str() {
          "ECPU" => rs.ecpu_usage = calc_freq(x.item, &self.soc.ecpu_freqs),
          "PCPU" => rs.pcpu_usage = calc_freq(x.item, &self.soc.pcpu_freqs),
          _ => {}
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_POWER_SUBG {
        match x.channel.as_str() {
          "GPUPH" => rs.gpu_usage = calc_freq(x.item, &self.soc.gpu_freqs[1..].to_vec()),
          _ => {}
        }
      }

      if x.group == "Energy Model" {
        match x.channel.as_str() {
          "CPU Energy" => rs.cpu_power += cfio_watts(x.item, &x.unit, duration)?,
          "GPU Energy" => rs.gpu_power += cfio_watts(x.item, &x.unit, duration)?,
          c if c.starts_with("ANE") => rs.ane_power += cfio_watts(x.item, &x.unit, duration)?,
          _ => {}
        }
      }
    }

    rs.all_power = rs.cpu_power + rs.gpu_power + rs.ane_power;
    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

    Ok(rs)
  }
}
