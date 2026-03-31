//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.

pub mod app;
pub mod config;
pub mod debug;
pub mod metrics;
pub mod sources;

// Re-export commonly used types
pub use app::App;
pub use config::{Config, ViewType};
pub use metrics::{MemMetrics, Metrics, Sampler, TempMetrics, zero_div};
pub use sources::SocInfo;
