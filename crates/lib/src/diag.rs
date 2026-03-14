use std::sync::OnceLock;
use std::time::Instant;

const START_ENV: &str = "MACMON_DEBUG";
static FALLBACK_START: OnceLock<Instant> = OnceLock::new();

pub(crate) fn startup_log(stage: impl AsRef<str>) {
  if !matches!(std::env::var(START_ENV).as_deref(), Ok("1")) {
    return;
  }

  let elapsed = FALLBACK_START.get_or_init(Instant::now).elapsed().as_secs_f64();
  eprintln!("macmon[{elapsed:.3}s] {}", stage.as_ref());
}
