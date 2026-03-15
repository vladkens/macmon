use std::{thread, time::Duration};

use crate::ffi::{Sampler, get_soc_info};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

pub fn print_debug() -> WithError<()> {
  unsafe { std::env::set_var("MACMON_DEBUG", "1") };
  let mut sampler = Sampler::new()?;
  let soc = get_soc_info()?;
  thread::sleep(Duration::from_millis(100));
  let metrics = sampler.get_metrics()?;

  let value = serde_json::json!({
    "soc": soc,
    "metrics": metrics,
  });
  println!("{}", serde_json::to_string_pretty(&value)?);
  Ok(())
}
