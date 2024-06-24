pub mod app;
pub mod metrics;
pub mod sources;

use app::App;
use clap::{Parser, Subcommand};
use metrics::Sampler;
use std::error::Error;

#[derive(Debug, Subcommand)]
enum Commands {
  /// Print raw metrics data instead of TUI
  Raw,

  /// Print diagnostic information (all possible metrics)
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

  /// Print raw data instead of TUI
  #[arg(long, default_value_t = false)]
  raw: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();
  let msec = args.interval.max(100);

  match &args.command {
    Some(Commands::Raw) => {
      let mut sampler = Sampler::new()?;

      loop {
        let metrics = sampler.get_metrics(msec)?;
        println!("{:?}", metrics);
      }
    }
    _ => {
      let mut app = App::new()?;
      app.run_loop(msec)?;
    }
  }

  Ok(())
}
