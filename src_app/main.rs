//! The macmon command-line application.

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, parser::ValueSource};
use std::error::Error;
use std::sync::{Arc, RwLock};
use std::thread;

mod config;
mod serve;
mod stress;
mod tui;

use macmon::{Metrics, Sampler, diagnostics::print_debug};
use tui::App;

// JSON output keeps the v0.7 field names as deprecated aliases.
#[derive(serde::Serialize)]
struct JsonMetrics<'a> {
  #[serde(flatten)]
  metrics: &'a Metrics,
  cpu_usage_pct: f32,
  ecpu_usage: (u32, f32),
  pcpu_usage: (u32, f32),
  gpu_usage: (u32, f32),
}

fn metrics_to_json_value(metrics: &Metrics) -> Result<serde_json::Value, serde_json::Error> {
  serde_json::to_value(JsonMetrics {
    metrics,
    cpu_usage_pct: metrics.cpu_scaled_ratio,
    ecpu_usage: (metrics.ecpu_freq_mhz, metrics.ecpu_scaled_ratio),
    pcpu_usage: (metrics.pcpu_freq_mhz, metrics.pcpu_scaled_ratio),
    gpu_usage: (metrics.gpu_freq_mhz, metrics.gpu_scaled_ratio),
  })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StressMode {
  /// Cyclic CPU load: one second busy, one second idle
  Pulse,
  /// Continuous CPU-only load
  Cpu,
  /// Continuous GPU-only load
  Gpu,
  /// Continuous CPU and GPU load
  All,
}

#[derive(Debug, Subcommand)]
enum Commands {
  /// Output metrics in JSON format (suitable for piping)
  #[command(alias = "raw")]
  Pipe {
    /// Number of samples to run for. Set to 0 to run indefinitely
    #[arg(short, long, default_value_t = 0)]
    samples: u32,

    /// Include SoC information in the output
    #[arg(long, default_value_t = false)]
    soc_info: bool,
  },

  /// Serve metrics over HTTP (JSON at /json, Prometheus at /metrics)
  Serve {
    /// Host address to listen on
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// Port to listen on
    #[arg(short, long, default_value_t = 9090)]
    port: u16,

    /// Install as a launchd service (auto-start on login)
    #[arg(long, default_value_t = false)]
    install: bool,

    /// Uninstall the launchd service
    #[arg(long, default_value_t = false)]
    uninstall: bool,
  },

  /// Print debug information
  Debug,

  /// Generate load for testing metrics
  Stress {
    /// Load pattern to generate
    #[arg(value_enum, default_value = "pulse")]
    mode: StressMode,

    /// Number of CPU worker threads. Ignored in GPU mode
    #[arg(short, long)]
    workers: Option<usize>,

    /// Stop after this many seconds. Runs until Ctrl-C when omitted
    #[arg(short, long)]
    duration: Option<u64>,
  },
}

/// Sudoless performance monitoring CLI tool for Apple Silicon processors
/// https://github.com/vladkens/macmon
#[derive(Debug, Parser)]
#[command(version, verbatim_doc_comment)]
struct Cli {
  #[command(subcommand)]
  command: Option<Commands>,

  /// Update interval in milliseconds
  #[arg(short, long, global = true, default_value_t = 1000)]
  interval: u32,
}

fn run_stress(
  mode: StressMode,
  workers: Option<usize>,
  duration: Option<u64>,
) -> Result<(), Box<dyn Error>> {
  let cpu_count = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

  match mode {
    StressMode::Pulse => stress::run_pattern(workers.unwrap_or(cpu_count.div_ceil(2)), duration),
    StressMode::Cpu => stress::run_cpu(workers.unwrap_or(cpu_count), duration),
    StressMode::Gpu => stress::run_gpu(duration)?,
    StressMode::All => stress::run_all(workers.unwrap_or(cpu_count), duration)?,
  }

  Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();

  match &args.command {
    Some(Commands::Pipe { samples, soc_info }) => {
      let mut sampler = Sampler::new()?;
      let mut counter = 0u32;

      let soc_info_val = if *soc_info { Some(sampler.get_soc_info().clone()) } else { None };

      loop {
        let doc = sampler.get_metrics(args.interval.max(100))?;

        let mut doc = metrics_to_json_value(&doc)?;
        if let Some(ref soc) = soc_info_val {
          doc["soc"] = serde_json::to_value(soc)?;
        }
        doc["timestamp"] = serde_json::to_value(chrono::Utc::now().to_rfc3339())?;
        let doc = serde_json::to_string(&doc)?;

        println!("{}", doc);

        counter += 1;
        if *samples > 0 && counter >= *samples {
          break;
        }
      }
    }
    Some(Commands::Serve { host, port, install, uninstall }) => {
      if *install || *uninstall {
        serve::launchd(host, *port, *install)?;
        return Ok(());
      }
      let mut sampler = Sampler::new()?;
      let soc = Arc::new(sampler.get_soc_info().clone());
      let shared: serve::SharedMetrics = Arc::new(RwLock::new(None));

      let shared_http = Arc::clone(&shared);
      let soc_http = Arc::clone(&soc);
      let host = host.clone();
      let port = *port;
      thread::spawn(move || {
        if let Err(e) = serve::run(&host, port, shared_http, soc_http) {
          eprintln!("server error: {e}");
        }
      });

      loop {
        match sampler.get_metrics(args.interval.max(100)) {
          Ok(m) => *shared.write().unwrap() = Some(m),
          Err(e) => eprintln!("sampling error: {e}"),
        }
      }
    }
    Some(Commands::Debug) => print_debug()?,
    Some(Commands::Stress { mode, workers, duration }) => run_stress(*mode, *workers, *duration)?,
    _ => {
      let mut app = App::new()?;

      let matches = Cli::command().get_matches();
      let msec = match matches.value_source("interval") {
        Some(ValueSource::CommandLine) => Some(args.interval),
        _ => None,
      };

      app.run_loop(msec)?;
    }
  }

  Ok(())
}
