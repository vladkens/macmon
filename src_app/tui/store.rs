//! Metric history stores used by the terminal UI.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::RatioMode;
use macmon::{CpuCoreMetrics, FanMetric, MemMetrics};

pub(super) const MAX_SPARKLINE: usize = 128;
const MAX_TEMPS: usize = 8;

#[derive(Debug, Default, Clone)]
pub(super) struct RatioSeries {
  pub(super) items: Vec<u64>, // Recent percentages (0..=100), newest first.
  pub(super) ratio: f64,      // Latest ratio (0.0..=1.0).
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct FreqSample {
  freq_mhz: u64,
  scaled_ratio: f64,
  active_ratio: f64,
}

impl FreqSample {
  pub(super) fn new(freq_mhz: u32, scaled_ratio: f32, active_ratio: f32) -> Self {
    Self {
      freq_mhz: freq_mhz as u64,
      scaled_ratio: scaled_ratio as f64,
      active_ratio: active_ratio as f64,
    }
  }

  fn from_core(core: &CpuCoreMetrics) -> Self {
    Self::new(core.freq_mhz, core.scaled_ratio, core.active_ratio)
  }
}

impl RatioSeries {
  fn push(&mut self, ratio: f64) {
    self.items.insert(0, (ratio * 100.0) as u64);
    self.items.truncate(MAX_SPARKLINE);
    self.ratio = ratio;
  }
}

/// One frequency with parallel scaled and active ratio histories.
#[derive(Debug, Default, Clone)]
pub(super) struct FreqStore {
  pub(super) freq_mhz: u64,
  scaled: RatioSeries,
  active: RatioSeries,
}

impl FreqStore {
  pub(super) fn push(&mut self, sample: FreqSample) {
    self.freq_mhz = sample.freq_mhz;
    self.scaled.push(sample.scaled_ratio);
    self.active.push(sample.active_ratio);
  }

  pub(super) fn ratio(&self, mode: RatioMode) -> &RatioSeries {
    match mode {
      RatioMode::Scaled => &self.scaled,
      RatioMode::Active => &self.active,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CoreId {
  pub(super) die_id: usize,
  pub(super) core_id: usize,
}

impl From<&CpuCoreMetrics> for CoreId {
  fn from(core: &CpuCoreMetrics) -> Self {
    Self { die_id: core.die_id, core_id: core.core_id }
  }
}

#[derive(Debug, Default)]
pub(super) struct CpuFreqStore {
  pub(super) aggregate: FreqStore,
  pub(super) cores: BTreeMap<CoreId, FreqStore>,
}

impl CpuFreqStore {
  pub(super) fn push(&mut self, aggregate: FreqSample, cores: &[CpuCoreMetrics]) {
    self.aggregate.push(aggregate);

    let mut seen = BTreeSet::new();
    for core in cores {
      let id = CoreId::from(core);
      self.cores.entry(id).or_default().push(FreqSample::from_core(core));
      seen.insert(id);
    }

    for (id, store) in &mut self.cores {
      if !seen.contains(id) {
        store.push(FreqSample::default());
      }
    }
  }

  pub(super) fn has_multiple_dies(&self) -> bool {
    let Some(first) = self.cores.keys().next() else { return false };
    self.cores.keys().any(|id| id.die_id != first.die_id)
  }

  /// Per-core meter labels (`E0`, or `D1 E0` with `with_die`) and latest ratios, in core order.
  pub(super) fn core_ratios(
    &self,
    cluster: &str,
    mode: RatioMode,
    with_die: bool,
  ) -> Vec<(String, f64)> {
    let label = |id: &CoreId| {
      if with_die {
        format!("D{} {cluster}{}", id.die_id, id.core_id)
      } else {
        format!("{cluster}{}", id.core_id)
      }
    };

    self.cores.iter().map(|(id, core)| (label(id), core.ratio(mode).ratio)).collect()
  }
}

#[derive(Debug, Default)]
pub(super) struct PowerStore {
  pub(super) items: Vec<u64>,
  pub(super) top_value: f64,
  pub(super) max_value: f64,
  pub(super) avg_value: f64,
}

impl PowerStore {
  pub(super) fn push(&mut self, value: f64) {
    let was_top = if !self.items.is_empty() { self.items[0] as f64 / 1000.0 } else { 0.0 };

    self.items.insert(0, (value * 1000.0) as u64);
    self.items.truncate(MAX_SPARKLINE);

    self.top_value = avg2(was_top, value);
    self.avg_value = self.items.iter().sum::<u64>() as f64 / self.items.len() as f64 / 1000.0;
    self.max_value = self.items.iter().max().map_or(0, |v| *v) as f64 / 1000.0;
  }
}

#[derive(Debug, Default)]
pub(super) struct MemoryStore {
  /// RAM usage history, newest first.
  pub(super) items: Vec<u64>,
  pub(super) ram_usage: u64,
  pub(super) ram_total: u64,
  pub(super) swap_usage: u64,
  pub(super) swap_total: u64,
}

impl MemoryStore {
  pub(super) fn push(&mut self, value: MemMetrics) {
    self.items.insert(0, value.ram_usage);
    self.items.truncate(MAX_SPARKLINE);

    self.ram_usage = value.ram_usage;
    self.ram_total = value.ram_total;
    self.swap_usage = value.swap_usage;
    self.swap_total = value.swap_total;
  }
}

#[derive(Debug, Default)]
pub(super) struct TempStore {
  items: Vec<f32>,
}

impl TempStore {
  pub(super) fn last(&self) -> f32 {
    *self.items.first().unwrap_or(&0.0)
  }

  pub(super) fn push(&mut self, value: f32) {
    // https://www.tunabellysoftware.com/blog/files/tg-pro-apple-silicon-m3-series-support.html
    // https://github.com/vladkens/macmon/issues/12
    let value = if value == 0.0 { self.trend_ema(0.8) } else { value };
    if value == 0.0 {
      return; // skip if not sensor available
    }

    self.items.insert(0, value);
    self.items.truncate(MAX_TEMPS);
  }

  // https://en.wikipedia.org/wiki/Exponential_smoothing
  fn trend_ema(&self, alpha: f32) -> f32 {
    if self.items.len() < 2 {
      return 0.0;
    }

    // starts from most recent value, so need to be reversed
    let mut iter = self.items.iter().rev();
    let mut ema = *iter.next().unwrap_or(&0.0);

    for &item in iter {
      ema = alpha * item + (1.0 - alpha) * ema;
    }

    ema
  }
}

#[derive(Debug, Default)]
pub(super) struct FanStore {
  items: Vec<FanMetric>,
}

impl FanStore {
  pub(super) fn push(&mut self, value: Vec<FanMetric>) {
    self.items = value;
  }

  pub(super) fn label(&self) -> String {
    match self.items.as_slice() {
      [] => "".to_string(),
      [fan] => format!("Fan {} RPM", fan.rpm),
      fans => {
        let values = fans.iter().map(|fan| fan.rpm.to_string()).collect::<Vec<_>>().join("/");
        format!("Fans {values} RPM")
      }
    }
  }
}

// get average of two values, used to smooth out metrics
// see: https://github.com/vladkens/macmon/issues/10
fn avg2<T: num_traits::Float>(a: T, b: T) -> T {
  if a == T::zero() { b } else { (a + b) / T::from(2.0).unwrap() }
}

#[cfg(test)]
mod tests {
  use macmon::{CpuCoreMetrics, FanMetric, MemMetrics};

  use super::{CoreId, CpuFreqStore, FanStore, FreqSample, MAX_SPARKLINE, MemoryStore};
  use super::{MAX_TEMPS, PowerStore, TempStore, avg2};
  use crate::config::RatioMode;

  fn core(die_id: usize, core_id: usize, freq_mhz: u32, ratio: f32) -> CpuCoreMetrics {
    CpuCoreMetrics { die_id, core_id, freq_mhz, scaled_ratio: ratio, active_ratio: ratio }
  }

  fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "expected {expected}, got {actual}");
  }

  #[test]
  fn avg2_skips_zero_previous_value() {
    assert_eq!(avg2(0.0, 4.0), 4.0);
    assert_eq!(avg2(2.0, 4.0), 3.0);
  }

  #[test]
  fn power_store_tracks_avg_max_and_smoothed_top() {
    let mut store = PowerStore::default();

    store.push(2.0);
    assert_eq!(store.items, vec![2000]);
    assert_close(store.top_value, 2.0); // no previous value, no smoothing
    assert_close(store.avg_value, 2.0);
    assert_close(store.max_value, 2.0);

    store.push(4.0);
    assert_eq!(store.items, vec![4000, 2000]);
    assert_close(store.top_value, 3.0); // average of previous and current
    assert_close(store.avg_value, 3.0);
    assert_close(store.max_value, 4.0);

    store.push(0.0);
    assert_close(store.top_value, 2.0);
    assert_close(store.avg_value, 2.0);
    assert_close(store.max_value, 4.0);
  }

  #[test]
  fn power_store_caps_history() {
    let mut store = PowerStore::default();
    for i in 0..(MAX_SPARKLINE + 10) {
      store.push(i as f64);
    }

    assert_eq!(store.items.len(), MAX_SPARKLINE);
    assert_eq!(store.items[0], ((MAX_SPARKLINE + 9) * 1000) as u64);
  }

  #[test]
  fn temp_store_skips_zero_without_history() {
    let mut store = TempStore::default();
    store.push(0.0);
    assert!(store.items.is_empty());
    assert_eq!(store.last(), 0.0);

    // one value is not enough to estimate a trend
    store.push(50.0);
    store.push(0.0);
    assert_eq!(store.items, vec![50.0]);
    assert_eq!(store.last(), 50.0);
  }

  #[test]
  fn temp_store_replaces_zero_with_trend() {
    let mut store = TempStore::default();
    store.push(50.0);
    store.push(52.0);
    store.push(0.0);

    // ema from oldest to newest: 0.8 * 52 + 0.2 * 50
    assert_eq!(store.items.len(), 3);
    assert!((store.last() - 51.6).abs() < 1e-4, "got {}", store.last());
  }

  #[test]
  fn temp_store_caps_history() {
    let mut store = TempStore::default();
    for i in 1..=(MAX_TEMPS + 5) {
      store.push(i as f32);
    }

    assert_eq!(store.items.len(), MAX_TEMPS);
    assert_eq!(store.last(), (MAX_TEMPS + 5) as f32);
  }

  #[test]
  fn cpu_freq_store_pushes_idle_sample_for_missing_core() {
    let mut store = CpuFreqStore::default();
    let aggregate = FreqSample::new(2000, 0.5, 0.6);

    store.push(aggregate, &[core(0, 0, 2000, 0.4), core(0, 1, 2400, 0.8)]);
    store.push(aggregate, &[core(0, 0, 1800, 0.2)]);

    assert_eq!(store.cores.len(), 2);
    assert_eq!(store.aggregate.freq_mhz, 2000);
    assert_eq!(store.aggregate.ratio(RatioMode::Scaled).items, vec![50, 50]);
    assert_eq!(store.aggregate.ratio(RatioMode::Active).items, vec![60, 60]);

    let core0 = &store.cores[&CoreId { die_id: 0, core_id: 0 }];
    assert_eq!(core0.freq_mhz, 1800);
    assert_eq!(core0.ratio(RatioMode::Scaled).items, vec![20, 40]);

    let core1 = &store.cores[&CoreId { die_id: 0, core_id: 1 }];
    assert_eq!(core1.freq_mhz, 0);
    assert_eq!(core1.ratio(RatioMode::Scaled).items, vec![0, 80]);
    assert_eq!(core1.ratio(RatioMode::Scaled).ratio, 0.0);
    assert_eq!(core1.ratio(RatioMode::Active).items, vec![0, 80]);
  }

  #[test]
  fn cpu_freq_store_detects_multiple_dies() {
    let mut store = CpuFreqStore::default();
    assert!(!store.has_multiple_dies());

    let aggregate = FreqSample::default();
    store.push(aggregate, &[core(0, 0, 1000, 0.1), core(0, 1, 1000, 0.1)]);
    assert!(!store.has_multiple_dies());

    store.push(aggregate, &[core(1, 0, 1000, 0.1)]);
    assert!(store.has_multiple_dies());
  }

  #[test]
  fn cpu_freq_store_core_ratios() {
    let mut store = CpuFreqStore::default();
    assert!(store.core_ratios("E", RatioMode::Scaled, false).is_empty());

    let mut cores = [core(1, 0, 1000, 0.25), core(0, 1, 1000, 0.5), core(0, 0, 1000, 0.75)];
    cores[0].active_ratio = 1.0;
    store.push(FreqSample::default(), &cores);

    // sorted by die, then core
    let labels = |with_die| -> Vec<String> {
      store.core_ratios("P", RatioMode::Scaled, with_die).into_iter().map(|(l, _)| l).collect()
    };
    assert_eq!(labels(false), ["P0", "P1", "P0"]);
    assert_eq!(labels(true), ["D0 P0", "D0 P1", "D1 P0"]);

    let ratios = |mode| -> Vec<f64> {
      store.core_ratios("P", mode, false).into_iter().map(|(_, r)| r).collect()
    };
    assert_eq!(ratios(RatioMode::Scaled), [0.75, 0.5, 0.25]);
    assert_eq!(ratios(RatioMode::Active), [0.75, 0.5, 1.0]);
  }

  #[test]
  fn memory_store_tracks_usage_and_history() {
    let mut store = MemoryStore::default();
    store.push(MemMetrics { ram_total: 100, ram_usage: 60, swap_total: 10, swap_usage: 4 });
    store.push(MemMetrics { ram_total: 100, ram_usage: 40, swap_total: 10, swap_usage: 2 });

    assert_eq!(store.items, vec![40, 60]);
    assert_eq!((store.ram_usage, store.ram_total), (40, 100));
    assert_eq!((store.swap_usage, store.swap_total), (2, 10));
  }

  #[test]
  fn fan_store_labels() {
    let fan = |rpm| FanMetric { name: String::new(), rpm, max_rpm: None };
    let mut store = FanStore::default();
    assert_eq!(store.label(), "");

    store.push(vec![fan(1200)]);
    assert_eq!(store.label(), "Fan 1200 RPM");

    store.push(vec![fan(1200), fan(1350)]);
    assert_eq!(store.label(), "Fans 1200/1350 RPM");
  }
}
