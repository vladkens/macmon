//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.
//!
//! # Examples
//!
//! [`Sampler::get_metrics`] reads synchronous hardware metrics without waiting.
//! The caller controls the interval; the first delta starts at sampler creation:
//!
//! ```no_run
//! use macmon::Sampler;
//! use std::{thread, time::{Duration, Instant}};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!   let mut sampler = Sampler::new()?;
//!   let mut sampled_at = Instant::now();
//!
//!   loop {
//!     thread::sleep(Duration::from_secs(1).saturating_sub(sampled_at.elapsed()));
//!     sampled_at = Instant::now();
//!     let metrics = sampler.get_metrics()?;
//!
//!     println!("CPU power: {:.2} W", metrics.cpu_power);
//!     println!("GPU power: {:.2} W", metrics.gpu_power);
//!     println!("CPU frequency-scaled ratio: {:.1}%", metrics.cpu_scaled_ratio * 100.0);
//!     println!("CPU active residency: {:.1}%", metrics.cpu_active_ratio * 100.0);
//!     for tier in &metrics.cpu_tiers {
//!       println!("{}-CPU frequency: {} MHz", tier.label, tier.freq_mhz);
//!     }
//!   }
//! }
//! ```
//!
//! You can also collect metrics in a worker thread and send them through a channel:
//!
//! ```no_run
//! use std::{
//!   sync::mpsc::{self, Receiver, TryRecvError},
//!   thread,
//!   time::{Duration, Instant},
//! };
//!
//! use macmon::{Metrics, Sampler};
//!
//! fn spawn_sampler(interval_ms: u32) -> Receiver<Metrics> {
//!   let (tx, rx) = mpsc::channel();
//!
//!   thread::spawn(move || {
//!     let mut sampler = Sampler::new().expect("failed to create sampler");
//!     let mut sampled_at = Instant::now();
//!
//!     loop {
//!       thread::sleep(Duration::from_millis(interval_ms.into()).saturating_sub(sampled_at.elapsed()));
//!       sampled_at = Instant::now();
//!       let Ok(metrics) = sampler.get_metrics() else { break };
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
//!
//! Creating `Sampler` inside the worker also keeps its low-level macOS handles
//! on that thread. `get_metrics()` consumes little CPU, but its synchronous macOS
//! calls can block. For UI and async applications, use a worker thread to keep
//! the event loop or executor responsive.

mod clpc;

#[cfg(feature = "app")]
#[doc(hidden)]
pub mod diagnostics;
#[doc(hidden)]
pub mod metrics;
mod shared;
pub mod sources;

// Re-export the commonly used types.
#[doc(inline)]
pub use metrics::{
  CpuCoreMetrics, CpuTierMetrics, FanMetric, MemMetrics, Metrics, Sampler, TempMetrics,
};
#[doc(inline)]
pub use sources::{CpuTierInfo, SocInfo};
