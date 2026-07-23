//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.
//!
//! # Examples
//!
//! [`Sampler::get_metrics`] blocks the current thread while macmon collects
//! metrics over the requested interval:
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
//!     println!("CPU effective usage: {:.1}%", metrics.cpu_usage_ratio * 100.0);
//!     println!("CPU active residency: {:.1}%", metrics.cpu_active_ratio * 100.0);
//!   }
//! }
//! ```
//!
//! To keep an application thread responsive, create the sampler inside a
//! dedicated worker thread and send completed metrics through a channel:
//!
//! ```no_run
//! use std::{
//!   sync::mpsc::{self, Receiver, TryRecvError},
//!   thread,
//! };
//!
//! use macmon::{Metrics, Sampler};
//!
//! fn spawn_sampler(interval_ms: u32) -> Receiver<Metrics> {
//!   let (tx, rx) = mpsc::channel();
//!
//!   thread::spawn(move || {
//!     let mut sampler = Sampler::new().expect("failed to create sampler");
//!
//!     while let Ok(metrics) = sampler.get_metrics(interval_ms) {
//!       if tx.send(metrics).is_err() {
//!         break;
//!       }
//!     }
//!   });
//!
//!   rx
//! }
//!
//! fn main() {
//!   let metrics = spawn_sampler(1000);
//!
//!   loop {
//!     match metrics.try_recv() {
//!       Ok(metrics) => println!("CPU power: {:.2} W", metrics.cpu_power),
//!       Err(TryRecvError::Empty) => {}
//!       Err(TryRecvError::Disconnected) => break,
//!     }
//!
//!     // The application can continue doing other work here.
//!   }
//! }
//! ```

#[doc(hidden)]
pub mod metrics;
mod shared;
pub mod sources;

// Re-export commonly used types
#[doc(inline)]
pub use metrics::{CpuCoreMetrics, FanMetric, MemMetrics, Metrics, Sampler, TempMetrics};
#[doc(inline)]
pub use sources::SocInfo;
