//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.

pub mod app;
pub mod config;
pub mod debug;
pub mod metrics;
pub mod sources;

#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench {
  use crate::{metrics, sources};

  pub fn ioreport_channels_filter(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
    metrics::ioreport_channels_filter(group, subgroup, channel, unit)
  }

  pub fn init_smc() -> sources::WithError<(sources::SMC, Vec<String>, Vec<String>)> {
    metrics::init_smc()
  }
}

// Re-export commonly used types
pub use app::App;
pub use config::{Config, ViewType};
pub use metrics::{MemMetrics, Metrics, Sampler, TempMetrics, zero_div};
pub use sources::{SocInfo, get_soc_info};
