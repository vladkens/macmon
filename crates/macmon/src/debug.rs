use crate::ffi::Sampler;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

pub fn print_debug() -> WithError<()> {
  let mut sampler = Sampler::new()?;
  let soc = sampler.get_soc_info()?;
  let metrics = sampler.get_metrics(100)?;

  let value = serde_json::json!({
    "soc": soc,
    "metrics": metrics,
  });
  println!("{}", serde_json::to_string_pretty(&value)?);
  Ok(())
}
