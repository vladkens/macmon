pub(crate) mod diag;
pub mod ffi;
pub mod metrics;
pub(crate) mod platform;
pub mod sources;

#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench {
  pub use crate::platform::{IOReport, SMC};

  pub struct SmcInit {
    _smc: SMC,
    _sensors: crate::metrics::SmcSensors,
  }

  pub fn ioreport_channels_filter(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
    crate::metrics::ioreport_channels_filter(group, subgroup, channel, unit)
  }

  pub fn init_smc() -> crate::platform::WithError<SmcInit> {
    let (smc, sensors) = crate::metrics::init_smc()?;
    Ok(SmcInit { _smc: smc, _sensors: sensors })
  }
}
