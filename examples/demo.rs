//! A continuous monitoring loop with the minimal `macmon::Sampler` API: CPU tier and GPU active
//! percentages, CPU and GPU temperatures, and RAM usage. Run with `cargo run --example demo`.
//!
//! The presentation is inspired by [homm/pgauge](https://github.com/homm/pgauge).

use macmon::Sampler;
use owo_colors::OwoColorize;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let mut sampler = Sampler::new()?;

  // a column per CPU tier (core type), like ECPU and PCPU on M1-M4
  let tiers = &sampler.get_soc_info().cpu_tiers;
  let tiers =
    tiers.iter().map(|tier| format!("{:>6}", format!("{}CPU", tier.label)).bold().to_string());
  println!(
    "{} {} {} {} {}",
    tiers.collect::<Vec<_>>().join(" "),
    format!("{:>6}", "GPU").bold(),
    format!("{:>6}", "CPU °C").bold(),
    format!("{:>6}", "GPU °C").bold(),
    format!("{:>11}", "RAM GiB").bold(),
  );

  loop {
    let metrics = sampler.get_metrics(1000)?;

    let cpus = metrics.cpu_tiers.iter().map(|tier| format!("{:5.1}%", tier.active_ratio * 100.0));
    let cpus = cpus.map(|x| x.cyan().to_string()).collect::<Vec<_>>().join(" ");
    let gpu_load = format!("{:5.1}%", metrics.gpu_active_ratio * 100.0);
    let cpu_temp = metrics
      .temp
      .cpu_temp_avg
      .map_or_else(|| format!("{:>6}", "N/A"), |value| format!("{value:6.1}"));
    let gpu_temp = metrics
      .temp
      .gpu_temp_avg
      .map_or_else(|| format!("{:>6}", "N/A"), |value| format!("{value:6.1}"));
    let ram = format!(
      "{:5.1}/{:5.1}",
      metrics.memory.ram_usage as f64 / GIB,
      metrics.memory.ram_total as f64 / GIB,
    );

    println!(
      "{} {} {} {} {}",
      cpus,
      gpu_load.blue(),
      cpu_temp.yellow(),
      gpu_temp.cyan(),
      ram.green(),
    );
  }
}
