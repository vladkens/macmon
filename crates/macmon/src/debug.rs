use std::{thread, time::Duration};

use crate::ffi::Sampler;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

pub fn print_debug() -> WithError<()> {
  let mut sampler = Sampler::new()?;
  let soc = sampler.get_soc_info()?;
  thread::sleep(Duration::from_millis(100));
  let metrics = sampler.get_metrics()?;

  let value = serde_json::json!({
    "soc": soc,
    "metrics": metrics,
  });
  println!("{}", serde_json::to_string_pretty(&value)?);
  Ok(())
}
