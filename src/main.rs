pub mod app;
pub mod cfutil;
pub mod metrics;

use clap::Parser;
use std::error::Error;

/// Sudoless performance monitoring CLI tool for Apple Silicon processors
/// https://github.com/vladkens/macmon
#[derive(Debug, Parser)]
#[command(version, verbatim_doc_comment)]
struct Cli {
  /// Update interval in milliseconds
  #[arg(short, long, default_value_t = 1000)]
  interval: u64,

  /// Print raw data instead of TUI
  #[arg(long, default_value_t = false)]
  raw: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
  let args = Cli::parse();
  let info = metrics::initialize().unwrap();

  if args.raw {
    let mut subs = metrics::SubsChan::new(info)?;
    loop {
      let data = subs.sample(args.interval)?;
      println!("{:?}", data);
    }
  } else {
    let mut app = app::App::new(info);
    app.run_loop(args.interval).unwrap();
  }

  Ok(())
}
