//! The macmon command-line application.

use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use clap::parser::ValueSource;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

mod config;
mod find_clpc;
mod procs;
mod serve;
mod stress;
mod tui;

use macmon::diagnostics::print_debug;
use macmon::{Metrics, Sampler};
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
  /// Repeated OCR with its main compute stage assigned to the Neural Engine
  Ane,
  /// Continuous CPU, GPU, and ANE load
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

  /// Find CLPC power counters with CPU/GPU/ANE loads
  FindClpc {
    /// Verify against Apple powermetrics (sudo for powermetrics only)
    #[arg(long)]
    powermetrics: bool,
  },

  /// Generate load for testing metrics
  Stress {
    /// Load pattern to generate
    #[arg(value_enum, default_value = "pulse")]
    mode: StressMode,

    /// Number of CPU worker threads. Ignored in GPU and ANE modes
    #[arg(short, long)]
    workers: Option<usize>,

    /// Stop after this many seconds (after ANE warmup in ane/all). Runs until Ctrl-C when omitted
    #[arg(short, long)]
    duration: Option<u64>,

    /// Pulse all workloads: SECONDS busy, then SECONDS idle (default: 2 when enabled)
    #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "2",
      value_parser = clap::value_parser!(u64).range(1..))]
    pulse: Option<u64>,
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

fn clock(seconds: u64) -> String {
  format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn run_stress(
  mode: StressMode,
  workers: Option<usize>,
  duration: Option<u64>,
  pulse: Option<u64>,
) -> Result<(), Box<dyn Error>> {
  if pulse.is_some() && mode != StressMode::All {
    return Err(
      "--pulse is supported with 'stress all'; use 'stress pulse' for the CPU-only pattern".into(),
    );
  }

  let uses_ane = matches!(mode, StressMode::Ane | StressMode::All);
  if uses_ane && duration == Some(0) {
    return Ok(());
  }

  let cpu_count = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
  let workers = match mode {
    StressMode::Pulse => workers.unwrap_or(cpu_count.div_ceil(2)),
    StressMode::Cpu | StressMode::All => workers.unwrap_or(cpu_count),
    StressMode::Gpu | StressMode::Ane => 1,
  }
  .max(1);
  let plural = if workers == 1 { "" } else { "s" };
  let mut label = match mode {
    StressMode::Pulse => format!("CPU pulse · {workers} worker{plural}"),
    StressMode::Cpu => format!("CPU · {workers} worker{plural}"),
    StressMode::Gpu => "GPU".to_string(),
    StressMode::Ane => "ANE OCR".to_string(),
    StressMode::All => format!("CPU + GPU + ANE · {workers} CPU worker{plural}"),
  };
  if let Some(seconds) = pulse {
    label.push_str(&format!(" · {seconds}s busy / {seconds}s idle"));
  }

  let mut ane = if uses_ane { Some(stress::AneLoad::prepare()?) } else { None };
  let started = Instant::now();
  let spinner = io::stderr().is_terminal().then(|| {
    let (done, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
      let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
      let mut frame = 0;

      loop {
        let elapsed = started.elapsed().as_secs();
        let timing = match duration {
          Some(total) => format!(
            "{} elapsed · {} remaining",
            clock(elapsed.min(total)),
            clock(total.saturating_sub(elapsed))
          ),
          None => format!("{} elapsed · Ctrl-C to stop", clock(elapsed)),
        };
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[2K{} {label} · {timing}", frames[frame]);
        let _ = stderr.flush();

        match receiver.recv_timeout(Duration::from_millis(100)) {
          Err(mpsc::RecvTimeoutError::Timeout) => frame = (frame + 1) % frames.len(),
          _ => break,
        }
      }
    });
    (done, handle)
  });

  let result = match mode {
    StressMode::Pulse => {
      stress::run_pattern(workers, duration);
      Ok(())
    }
    StressMode::Cpu => {
      stress::run_cpu(workers, duration);
      Ok(())
    }
    StressMode::Gpu => stress::run_gpu(duration),
    StressMode::Ane => ane.as_mut().expect("ANE workload prepared").run(duration),
    StressMode::All => {
      stress::run_all(workers, duration, pulse, ane.as_mut().expect("ANE workload prepared"))
    }
  };

  if let Some((done, handle)) = spinner {
    let _ = done.send(());
    let _ = handle.join();
    let mut stderr = io::stderr().lock();
    let _ = write!(stderr, "\r\x1b[2K");
    let _ = stderr.flush();
  }

  if let Some(ane) = ane {
    eprintln!(
      "ANE OCR · {} requests completed in {:.1}s",
      ane.completed_requests(),
      started.elapsed().as_secs_f64()
    );
  }

  result
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();

  match &args.command {
    Some(Commands::Pipe { samples, soc_info }) => {
      // Debug override: require CLPC CPU/GPU/ANE counters without fallback.
      let force_clpc = std::env::var("MACMON_FORCE_CLPC").is_ok_and(|x| x == "1");
      let mut sampler = if force_clpc { Sampler::with_clpc()? } else { Sampler::new()? };
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
    Some(Commands::FindClpc { powermetrics }) => find_clpc::run(*powermetrics)?,
    Some(Commands::Stress { mode, workers, duration, pulse }) => {
      run_stress(*mode, *workers, *duration, *pulse)?;
    }
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn zero_duration_ane_and_all_skip_warmup() {
    for mode in [StressMode::Ane, StressMode::All] {
      run_stress(mode, None, Some(0), None).unwrap();
    }
  }

  #[test]
  fn parses_optional_pulse_interval_for_all() {
    for (args, expected) in [
      (vec!["macmon", "stress", "all"], None),
      (vec!["macmon", "stress", "all", "--pulse"], Some(2)),
      (vec!["macmon", "stress", "all", "--pulse", "3"], Some(3)),
      (vec!["macmon", "stress", "all", "--pulse", "--duration", "10"], Some(2)),
    ] {
      let cli = Cli::try_parse_from(args).unwrap();
      let Some(Commands::Stress { mode: StressMode::All, pulse, .. }) = cli.command else {
        panic!("expected stress all");
      };
      assert_eq!(pulse, expected);
    }
    for value in ["0", "-1", "nope"] {
      assert!(Cli::try_parse_from(["macmon", "stress", "all", "--pulse", value]).is_err());
    }
  }

  #[test]
  fn pulse_requires_all_and_zero_duration_skips_load() {
    assert!(run_stress(StressMode::Cpu, None, Some(0), Some(2)).is_err());
    run_stress(StressMode::All, None, Some(0), Some(2)).unwrap();
  }
}
