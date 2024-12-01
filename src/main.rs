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
  /// Print metrics in JSON format – can be used for piping
  Raw,

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
    Some(Commands::Raw) => {
      let mut sampler = Sampler::new()?;

      loop {
        let doc = sampler.get_metrics(msec)?;
        let doc = serde_json::to_string(&doc)?;
        println!("{}", doc);
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
