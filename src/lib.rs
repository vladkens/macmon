//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.
//!
//! # Examples
//!
//! Use [`Sampler::get_metrics`] when macmon should manage the polling interval
//! and return smoothed metrics:
//!
//! ```no_run
//! use macmon::Sampler;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!   let mut sampler = Sampler::new()?;
//!
//!   loop {
//!     let metrics = sampler.get_metrics(1000)?;
//!
//!     println!("CPU power: {:.2} W", metrics.cpu_power);
//!     println!("GPU power: {:.2} W", metrics.gpu_power);
//!     println!("CPU usage: {:.1}%", metrics.cpu_usage_pct * 100.0);
//!   }
//! }
//! ```
//!
//! Use [`Sampler::get_metrics_now`] when the caller owns scheduling:
//!
//! ```no_run
//! use std::{thread, time::Duration};
//!
//! use macmon::Sampler;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!   let mut sampler = Sampler::new()?;
//!
//!   loop {
//!     thread::sleep(Duration::from_millis(1000));
//!
//!     let Some(metrics) = sampler.get_metrics_now(1500)? else {
//!       continue;
//!     };
//!
//!     println!("CPU power: {:.2} W", metrics.cpu_power);
//!     println!("GPU power: {:.2} W", metrics.gpu_power);
//!     println!("CPU usage: {:.1}%", metrics.cpu_usage_pct * 100.0);
//!   }
//! }
//! ```

#[doc(hidden)]
pub mod metrics;
mod shared;
pub mod sources;

// Re-export commonly used types
#[doc(inline)]
pub use metrics::{FanMetric, MemMetrics, Metrics, Sampler, TempMetrics};
#[doc(inline)]
pub use sources::SocInfo;
