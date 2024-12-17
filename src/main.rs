mod app;
mod config;
mod debug;
mod metrics;
mod sources;

use app::App;
use clap::{Parser, Subcommand};
use metrics::Sampler;
use std::error::Error;

#[derive(Debug, Subcommand)]
enum Commands {
  /// Output metrics in JSON format (suitable for piping)
  #[command(alias = "raw")]
  Pipe {
    /// Number of samples to run for. Set to 0 to run indefinitely
    #[arg(short, long, default_value_t = 0)]
    samples: u32,
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
  #[arg(short, long, default_value_t = 1000)]
  interval: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();
  let msec = args.interval.max(100);

  match &args.command {
    Some(Commands::Pipe { samples }) => {
      let mut sampler = Sampler::new()?;
      let mut counter = 0u32;

      loop {
        let doc = sampler.get_metrics(msec)?;
        let doc = serde_json::to_string(&doc)?;
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
      app.run_loop(msec)?;
    }
  }

  Ok(())
}
