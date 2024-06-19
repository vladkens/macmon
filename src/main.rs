pub mod app;
pub mod ioreport;
pub mod metrics;

use clap::{Parser, Subcommand};
use std::error::Error;

#[derive(Debug, Subcommand)]
enum Commands {
  /// Print raw metrics data instead of TUI
  Raw,

  /// Print diagnostic information (all possible metrics)
  Diagnostic,
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
  let info = metrics::get_soc_info()?;
  let interval = args.interval.max(100);

  match &args.command {
    // Some(Commands::Diagnostic) => metrics::print_diagnostic(info, args.interval)?,
    Some(Commands::Raw) => {
      let mut sampler = metrics::get_metrics_sampler(info)?;

      loop {
        let metrics = sampler.get_metrics(interval)?;
        println!("{:?}", metrics);
      }
    }
    _ => {
      let mut app = app::App::new(info);
      app.run_loop(interval)?;
    }
  }

  Ok(())
}
