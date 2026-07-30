use macmon::Sampler;
use owo_colors::OwoColorize;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let mut sampler = Sampler::new()?;

  println!(
    "{} {} {} {} {} {}",
    format!("{:>6}", "ECPU").bold(),
    format!("{:>6}", "PCPU").bold(),
    format!("{:>6}", "GPU").bold(),
    format!("{:>6}", "CPU °C").bold(),
    format!("{:>6}", "GPU °C").bold(),
    format!("{:>11}", "RAM GiB").bold(),
  );

  loop {
    let metrics = sampler.get_metrics(1000)?;

    let ecpu = format!("{:5.1}%", metrics.ecpu_active_ratio * 100.0);
    let pcpu = format!("{:5.1}%", metrics.pcpu_active_ratio * 100.0);
    let gpu_load = format!("{:5.1}%", metrics.gpu_active_ratio * 100.0);
    let cpu_temp = format!("{:6.1}", metrics.temp.cpu_temp_avg);
    let gpu_temp = format!("{:6.1}", metrics.temp.gpu_temp_avg);
    let ram = format!(
      "{:5.1}/{:5.1}",
      metrics.memory.ram_usage as f64 / GIB,
      metrics.memory.ram_total as f64 / GIB,
    );

    println!(
      "{} {} {} {} {} {}",
      ecpu.cyan(),
      pcpu.magenta(),
      gpu_load.blue(),
      cpu_temp.yellow(),
      gpu_temp.cyan(),
      ram.green(),
    );
  }
}
