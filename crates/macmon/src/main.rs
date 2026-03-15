mod app;
mod config;
mod debug;

use app::App;
use clap::{CommandFactory, Parser, Subcommand, parser::ValueSource};
use macmon_lib::metrics::{Metrics, Sampler};
use macmon_lib::sources::{SocInfo, get_soc_info};
use serde::Serialize;
use std::error::Error;
use std::{
  thread,
  time::{Duration, Instant},
};

#[derive(Serialize)]
struct PipeSample<'a> {
  timestamp: &'a str,
  #[serde(flatten)]
  metrics: &'a Metrics,
  #[serde(skip_serializing_if = "Option::is_none")]
  soc: Option<&'a SocInfo>,
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

  /// Update interval in milliseconds
  #[arg(short, long, global = true, default_value_t = 1000)]
  interval: u32,
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();

  match &args.command {
    Some(Commands::Pipe { samples, soc_info }) => {
      let mut sampler = Sampler::new()?;
      let mut counter = 0u32;

      let soc_info_val = if *soc_info { Some(get_soc_info()?) } else { None };
      let interval = Duration::from_millis(args.interval as u64);
      let mut last_update_started = Instant::now();

      loop {
        let elapsed = last_update_started.elapsed();
        if elapsed < interval {
          thread::sleep(interval - elapsed);
        }
        last_update_started = Instant::now();

        let metrics = sampler.get_metrics()?;
        let timestamp = chrono::Utc::now().to_rfc3339();
        let doc = serde_json::to_string(&PipeSample {
          metrics: &metrics,
          soc: soc_info_val.as_ref(),
          timestamp: &timestamp,
        })?;

        println!("{}", doc);

        counter += 1;
        if *samples > 0 && counter >= *samples {
          break;
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

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
