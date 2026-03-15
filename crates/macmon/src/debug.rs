use std::{thread, time::Duration};

use macmon_lib::metrics::{Metrics, Sampler};
use macmon_lib::sources::{SocInfo, get_soc_info};
use serde::Serialize;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize)]
struct DebugOutput<'a> {
  soc: &'a SocInfo,
  metrics: &'a Metrics,
}

pub fn print_debug() -> WithError<()> {
  unsafe { std::env::set_var("MACMON_DEBUG", "1") };
  let mut sampler = Sampler::new()?;
  let soc = get_soc_info()?;
  thread::sleep(Duration::from_millis(100));
  let metrics = sampler.get_metrics()?;

  println!("{}", serde_json::to_string_pretty(&DebugOutput { soc: &soc, metrics: &metrics })?);
  Ok(())
}

#[cfg(test)]
#[path = "debug_test.rs"]
mod tests;
