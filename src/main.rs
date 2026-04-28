use clap::{CommandFactory, Parser, Subcommand, parser::ValueSource};
use macmon::config::INTERVAL_MIN;
use macmon::{App, Sampler, debug};
use std::error::Error;
use std::sync::{Arc, RwLock};
use std::{
  thread,
  time::{Duration, Instant},
};

mod serve;

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
}

/// Sudoless performance monitoring CLI tool for Apple Silicon processors
/// https://github.com/vladkens/macmon
#[derive(Debug, Parser)]
#[command(version, verbatim_doc_comment)]
struct Cli {
  #[command(subcommand)]
  command: Option<Commands>,

  /// Update interval in milliseconds (minimum: 100)
  #[arg(short, long, global = true, default_value_t = 1000)]
  interval: u32,
}

fn wait_until_next_sample(last_sampled_at: &mut Instant, interval: Duration) {
  let mut now = Instant::now();
  let elapsed = now.duration_since(*last_sampled_at);
  if elapsed < interval {
    thread::sleep(interval - elapsed);
    now += interval - elapsed;
  }
  *last_sampled_at = now;
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();

  match &args.command {
    Some(Commands::Pipe { samples, soc_info }) => {
      let mut sampler = Sampler::new()?;
      let mut counter = 0u32;

      let soc_info_val = if *soc_info { Some(sampler.get_soc_info().clone()) } else { None };
      let interval = Duration::from_millis(args.interval.max(INTERVAL_MIN) as u64);
      let mut last_update_started = Instant::now();

      loop {
        wait_until_next_sample(&mut last_update_started, interval);
        let doc = sampler.get_metrics()?;

        let mut doc = serde_json::to_value(&doc)?;
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
    Some(Commands::Serve { port, install, uninstall }) => {
      if *install || *uninstall {
        serve::launchd(*port, *install)?;
        return Ok(());
      }
      let mut sampler = Sampler::new()?;
      let soc = Arc::new(sampler.get_soc_info().clone());
      let shared: serve::SharedMetrics = Arc::new(RwLock::new(None));
      let interval = Duration::from_millis(args.interval.max(INTERVAL_MIN) as u64);
      let mut last_update_started = Instant::now();

      let shared_http = Arc::clone(&shared);
      let soc_http = Arc::clone(&soc);
      let port = *port;
      thread::spawn(move || {
        if let Err(e) = serve::run(port, shared_http, soc_http) {
          eprintln!("server error: {e}");
        }
      });

      loop {
        wait_until_next_sample(&mut last_update_started, interval);
        match sampler.get_metrics() {
          Ok(m) => *shared.write().unwrap() = Some(m),
          Err(e) => eprintln!("sampling error: {e}"),
        }
      }
    }
    Some(Commands::Debug) => debug::print_debug()?,
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
