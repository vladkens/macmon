//! Metric history stores used by the terminal UI.

use crate::config::RatioMode;
use macmon::{FanMetric, MemMetrics, Metrics, SocInfo};

/// Samples kept for the history graphs, newest first: one per column, enough to fill the widest
/// box (CPU power, a third of the width) of a terminal about 3000 columns wide.
const HISTORY_LEN: usize = 1024;
/// Latest samples behind the power average and maximum.
const STATS_LEN: usize = 128;
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
}

impl RatioSeries {
  fn push(&mut self, ratio: f64) {
    // rounded like the percent in the box title, so the graph agrees with it
    self.items.insert(0, (ratio * 100.0).round() as u64);
    self.items.truncate(HISTORY_LEN);
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

/// History of one CPU cluster.
#[derive(Debug, Default)]
pub(super) struct ClusterStore {
  /// Tier label: `E` / `P` on M1–M4, `P` / `S` on M5+.
  pub(super) label: String,
  /// Core count, for the chip summary.
  pub(super) count: usize,
  pub(super) freq: FreqStore,
}

/// Histories of the CPU clusters, lowest tier first. The library reports two tiers today; the TUI
/// takes any number of them.
#[derive(Debug, Default)]
pub(super) struct CpuClusters {
  pub(super) items: Vec<ClusterStore>,
}

impl CpuClusters {
  /// Clusters of `(tier label, core count)`, lowest tier first, without samples yet.
  pub(super) fn new<'a>(tiers: impl IntoIterator<Item = (&'a str, usize)>) -> Self {
    let cluster = |(label, count): (&str, usize)| ClusterStore {
      label: label.to_string(),
      count,
      freq: FreqStore::default(),
    };
    Self { items: tiers.into_iter().map(cluster).collect() }
  }

  /// The two clusters the library reports, with the labels and core counts of `soc`, so their
  /// boxes and the chip summary are complete before the first metrics sample.
  pub(super) fn from_soc(soc: &SocInfo) -> Self {
    let tiers = [(&soc.ecpu_label, soc.ecpu_cores), (&soc.pcpu_label, soc.pcpu_cores)];
    Self::new(tiers.map(|(label, count)| (label.as_str(), usize::from(count))))
  }

  /// Adds a sample per cluster, in cluster order (see `cluster_samples`).
  pub(super) fn push(&mut self, samples: &[FreqSample]) {
    for (cluster, &sample) in self.items.iter_mut().zip(samples) {
      cluster.freq.push(sample);
    }
  }
}

/// Samples of the two CPU clusters of a metrics sample, lowest tier first, as `CpuClusters::from_soc`
/// orders them.
pub(super) fn cluster_samples(data: &Metrics) -> [FreqSample; 2] {
  [
    FreqSample::new(data.ecpu_freq_mhz, data.ecpu_scaled_ratio, data.ecpu_active_ratio),
    FreqSample::new(data.pcpu_freq_mhz, data.pcpu_scaled_ratio, data.pcpu_active_ratio),
  ]
}

/// Power history (mW, newest first) with the smoothed current value, and the average and maximum
/// of the latest `STATS_LEN` samples.
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
    self.items.truncate(HISTORY_LEN);

    let stats = &self.items[..self.items.len().min(STATS_LEN)];
    self.top_value = avg2(was_top, value);
    self.avg_value = stats.iter().sum::<u64>() as f64 / stats.len() as f64 / 1000.0;
    self.max_value = stats.iter().max().map_or(0, |v| *v) as f64 / 1000.0;
  }
}

/// Latest RAM / SWAP usage with the RAM usage history (bytes, newest first).
#[derive(Debug, Default)]
pub(super) struct MemoryStore {
  pub(super) items: Vec<u64>,
  pub(super) ram_usage: u64,
  pub(super) ram_total: u64,
  pub(super) swap_usage: u64,
  pub(super) swap_total: u64,
}

impl MemoryStore {
  pub(super) fn push(&mut self, value: MemMetrics) {
    self.items.insert(0, value.ram_usage);
    self.items.truncate(HISTORY_LEN);

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

/// Share of `value` in `total`; 0 for a zero total (no sample yet).
pub(super) fn ratio(value: f64, total: f64) -> f64 {
  if total == 0.0 { 0.0 } else { value / total }
}

#[cfg(test)]
mod tests {
  use macmon::{FanMetric, MemMetrics, Metrics, SocInfo};

  use super::{CpuClusters, FanStore, FreqSample, FreqStore};
  use super::{HISTORY_LEN, MAX_TEMPS, MemoryStore, PowerStore, STATS_LEN, TempStore};
  use super::{avg2, cluster_samples, ratio};
  use crate::config::RatioMode;

  fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "expected {expected}, got {actual}");
  }

  #[test]
  fn avg2_skips_zero_previous_value() {
    assert_eq!(avg2(0.0, 4.0), 4.0);
    assert_eq!(avg2(2.0, 4.0), 3.0);
  }

  #[test]
  fn ratio_of_zero_total_is_zero() {
    assert_eq!(ratio(3.0, 4.0), 0.75);
    assert_eq!(ratio(3.0, 0.0), 0.0);
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
    for i in 0..(HISTORY_LEN + 10) {
      store.push(i as f64);
    }

    assert_eq!(store.items.len(), HISTORY_LEN);
    assert_eq!(store.items[0], ((HISTORY_LEN + 9) * 1000) as u64);
  }

  #[test]
  fn power_stats_cover_latest_samples_only() {
    // a 50 W peak, then STATS_LEN samples of 1 and 3 W: the graph keeps the peak, the average and
    // maximum don't
    let mut store = PowerStore::default();
    store.push(50.0);
    for i in 0..STATS_LEN {
      store.push(if i % 2 == 0 { 1.0 } else { 3.0 });
    }

    assert_eq!(store.items.len(), STATS_LEN + 1);
    assert_eq!(store.items.last(), Some(&50_000));
    assert_close(store.avg_value, 2.0);
    assert_close(store.max_value, 3.0);

    // one sample less: the peak is still in the window
    let mut store = PowerStore::default();
    store.push(50.0);
    for _ in 1..STATS_LEN {
      store.push(1.0);
    }
    assert_close(store.max_value, 50.0);
    assert_close(store.avg_value, (50.0 + (STATS_LEN - 1) as f64) / STATS_LEN as f64);
  }

  #[test]
  fn freq_store_keeps_long_history() {
    let mut store = FreqStore::default();
    for _ in 0..(HISTORY_LEN + 5) {
      store.push(FreqSample::new(1000, 0.25, 0.5));
    }
    store.push(FreqSample::new(3000, 0.75, 1.0));

    assert_eq!(store.freq_mhz, 3000);
    for mode in [RatioMode::Scaled, RatioMode::Active] {
      assert_eq!(store.ratio(mode).items.len(), HISTORY_LEN, "{mode:?}");
    }
    // newest first
    let scaled = store.ratio(RatioMode::Scaled);
    assert_eq!(scaled.items[..2], [75, 25]);
    assert_close(scaled.ratio, 0.75);
    assert_eq!(store.ratio(RatioMode::Active).items[..2], [100, 50]);
  }

  #[test]
  fn ratio_history_rounds_like_the_title() {
    // the metrics are f32: 0.42 is 0.41999998, still 42 % in the title and the graph
    let mut store = FreqStore::default();
    for ratio in [0.42f32, 0.77, 0.004, 0.006, 0.996, 1.0] {
      store.push(FreqSample::new(1000, ratio, ratio));
    }
    assert_eq!(store.ratio(RatioMode::Scaled).items, [100, 100, 1, 0, 77, 42]);
    assert_eq!(format!("{:.0}", 0.42f32 as f64 * 100.0), "42");
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
  fn cpu_clusters_take_a_sample_each() {
    // three tiers, like M6 (6E + 4P + 2S)
    let mut clusters = CpuClusters::new([("E", 6), ("P", 4), ("S", 2)]);
    let sample = |ratio: f32| FreqSample::new(1000, ratio, ratio);
    clusters.push(&[sample(0.1), sample(0.2), sample(0.3)]);
    clusters.push(&[sample(0.4), sample(0.5), sample(0.6)]);

    let labels: Vec<(&str, usize)> =
      clusters.items.iter().map(|c| (c.label.as_str(), c.count)).collect();
    assert_eq!(labels, [("E", 6), ("P", 4), ("S", 2)]);
    let history = |i: usize| clusters.items[i].freq.ratio(RatioMode::Scaled).items.clone();
    assert_eq!([history(0), history(1), history(2)], [[40, 10], [50, 20], [60, 30]]);
  }

  #[test]
  fn cpu_clusters_from_soc_and_metrics() {
    let soc = SocInfo {
      ecpu_cores: 6,
      pcpu_cores: 4,
      ecpu_label: "P".to_string(),
      pcpu_label: "S".to_string(),
      ..Default::default()
    };
    let data = Metrics {
      ecpu_freq_mhz: 2000,
      ecpu_scaled_ratio: 0.5,
      pcpu_freq_mhz: 4000,
      pcpu_active_ratio: 0.25,
      ..Default::default()
    };

    // labels and core counts before any sample
    let mut clusters = CpuClusters::from_soc(&soc);
    let labels: Vec<(&str, usize)> =
      clusters.items.iter().map(|c| (c.label.as_str(), c.count)).collect();
    assert_eq!(labels, [("P", 6), ("S", 4)]);
    assert!(clusters.items.iter().all(|c| c.freq.ratio(RatioMode::Scaled).items.is_empty()));

    clusters.push(&cluster_samples(&data));
    let [p, s] = [&clusters.items[0].freq, &clusters.items[1].freq];
    assert_eq!((p.freq_mhz, p.ratio(RatioMode::Scaled).ratio), (2000, 0.5));
    assert_eq!((s.freq_mhz, s.ratio(RatioMode::Active).ratio), (4000, 0.25));
  }

  #[test]
  fn memory_store_tracks_usage() {
    let mut store = MemoryStore::default();
    store.push(MemMetrics { ram_total: 100, ram_usage: 60, swap_total: 10, swap_usage: 4 });
    store.push(MemMetrics { ram_total: 100, ram_usage: 40, swap_total: 10, swap_usage: 2 });

    assert_eq!((store.ram_usage, store.ram_total), (40, 100));
    assert_eq!((store.swap_usage, store.swap_total), (2, 10));
    // RAM history for the graph, newest first
    assert_eq!(store.items, [40, 60]);

    for i in 0..HISTORY_LEN as u64 {
      store.push(MemMetrics { ram_total: 100, ram_usage: i, swap_total: 0, swap_usage: 0 });
    }
    assert_eq!(store.items.len(), HISTORY_LEN);
    assert_eq!(store.items[0], HISTORY_LEN as u64 - 1);
  }

  #[test]
  fn fan_store_labels() {
    let fan = |rpm| FanMetric { name: String::new(), rpm, max_rpm: None };
    let mut store = FanStore::default();
    assert_eq!(store.label(), "");

    // the original format
    store.push(vec![fan(1200)]);
    assert_eq!(store.label(), "Fan 1200 RPM");

    store.push(vec![fan(1200), fan(1350)]);
    assert_eq!(store.label(), "Fans 1200/1350 RPM");
  }
}
