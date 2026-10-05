//! Power candidates, histogram estimates and per-component source selection.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::shared::{is_clpc_energy_channel, is_pmp_ane_channel, is_pmp_cpu_channel};
use crate::sources::{
  ChannelId, ChannelInfo, IOReportIteratorItem, cfio_format, cfio_get_residencies, cfio_id,
  cfio_watts,
};

/// Power source override for diagnostic sampling. Normal applications should use `Auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PowerMode {
  /// Select independently for CPU, GPU and ANE, preferring CLPC.
  #[default]
  Auto,
  /// Require valid CLPC counters for all three components, including idle zeroes.
  Clpc,
  /// Read Energy Model directly for all three components, even if it is not updating.
  EnergyModel,
  /// Require the approximate CPU PMP histogram estimate; GPU and ANE remain automatic.
  Pmp,
}

impl std::str::FromStr for PowerMode {
  type Err = &'static str;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value {
      "auto" => Ok(Self::Auto),
      "clpc" => Ok(Self::Clpc),
      "energy-model" => Ok(Self::EnergyModel),
      "pmp" => Ok(Self::Pmp),
      _ => Err("expected auto, clpc, energy-model, or pmp"),
    }
  }
}

const CPU: usize = 0;
const CLPC: usize = 0;
const ENERGY_MODEL: usize = 1;
const PMP: usize = 2;
const COMPONENTS: [&str; 3] = ["CPU", "GPU", "ANE"];
const SOURCES: [&str; 3] = ["CLPC", "Energy Model", "PMP"];
const VERIFY_SECONDS: f64 = 3.0;
const NO_PMP_OBSERVATIONS: &str =
  "no PMP observations in a driver/group; zero power is not established";

pub(crate) fn channel_source(g: &str, s: &str, c: &str, u: &str) -> Option<(usize, usize)> {
  if is_pmp_cpu_channel(g, s, c) {
    return Some((CPU, PMP));
  }
  if is_pmp_ane_channel(g, s, c, u) {
    return Some((2, PMP));
  }
  if g == "Energy Model" || is_clpc_energy_channel(g, s, c, u) {
    let component = if c.ends_with("CPU Energy") {
      CPU
    } else if c == "GPU Energy" {
      1
    } else if c.starts_with("ANE") {
      2
    } else {
      return None;
    };
    return Some((component, if g == "CLPC" { CLPC } else { ENERGY_MODEL }));
  }
  None
}

#[derive(Clone, Debug, PartialEq)]
enum ChannelDelta {
  Scalar(f32),
  Histogram(Vec<(String, i64)>),
}

type Reading<T> = Result<T, &'static str>;

#[derive(Default)]
pub(crate) struct PowerFrame {
  values: HashMap<ChannelId, Reading<ChannelDelta>>,
  kinds: HashMap<ChannelId, (usize, usize)>,
}

impl PowerFrame {
  pub(crate) fn with_issues(issues: &HashMap<ChannelId, &'static str>) -> Self {
    Self {
      values: issues.iter().map(|(&id, &reason)| (id, Err(reason))).collect(),
      ..Self::default()
    }
  }

  fn insert(&mut self, id: ChannelId, value: Reading<ChannelDelta>) {
    use std::collections::hash_map::Entry;
    match self.values.entry(id) {
      Entry::Vacant(entry) => {
        entry.insert(value);
      }
      Entry::Occupied(mut entry) => {
        if entry.get() != &value {
          let _ = entry.insert(Err("conflicting duplicate channel"));
        }
      }
    }
  }

  pub(crate) fn read(
    &mut self,
    item: &IOReportIteratorItem,
    elapsed: Duration,
    issues: &HashMap<ChannelId, &'static str>,
  ) -> bool {
    let Some(kind) = channel_source(&item.group, &item.subgroup, &item.channel, &item.unit) else {
      return false;
    };
    let id = cfio_id(item.item);
    self.kinds.insert(id, kind);
    let value = if let Some(&reason) = issues.get(&id) {
      Err(reason)
    } else if kind == (CPU, PMP) {
      if cfio_format(item.item) == 2 {
        Ok(ChannelDelta::Histogram(cfio_get_residencies(item.item)))
      } else {
        Err("PMP histogram is not a state channel")
      }
    } else {
      cfio_watts(item.item, &item.unit, elapsed)
        .map_err(|_| "invalid scalar format or energy unit")
        .and_then(|watts| {
          if watts.is_finite() && watts >= 0.0 {
            Ok(ChannelDelta::Scalar(watts))
          } else {
            Err("invalid energy delta or counter reset")
          }
        })
    };
    self.insert(id, value);
    true
  }
}

#[derive(Clone, Copy, Debug)]
struct Histogram {
  weighted: f64,
  ticks: f64,
  active: bool,
  saturated: bool,
}

// Midpoint interpretation: PWE's clusterWatts and Wattly's histogramMeanWatts.
// https://github.com/kenshinice-ai/pwemacmonitor/blob/cd2c6ac3e346f7f36f7aaa82deb668e44090ba68/Sources/Core/Sampler.swift#L696-L711
// https://github.com/jjundev/project_wattly/blob/a2be80ba05fc2d09d4deb6a7a6a2b539df4e52f0/Wattly/Core/PowerHistogram.swift#L33-L56
// We parse every upper boundary individually, allowing nonuniform bins.
// Initial hypothesis, not an Apple contract: labels are successive upper bounds,
// the first lower bound is zero, and SRAM is excluded by the channel classifier.
// The final numeric label gives a nominal midpoint for a possibly open bin. Any
// observations there are flagged; no finite error bound can be claimed.
fn histogram(states: &[(String, i64)]) -> Reading<Histogram> {
  if states.is_empty() {
    return Err("empty PMP state layout");
  }

  let mut result = Histogram { weighted: 0.0, ticks: 0.0, active: false, saturated: false };
  let mut lower = 0.0;
  for (i, (label, count)) in states.iter().enumerate() {
    let upper = label
      .trim()
      .strip_suffix('W')
      .and_then(|value| value.trim().parse::<f64>().ok())
      .filter(|upper| upper.is_finite() && *upper > lower)
      .ok_or("invalid or unordered PMP watt boundaries")?;
    if *count < 0 {
      return Err("PMP counter reset");
    }

    result.weighted += *count as f64 * (lower + upper) / 2.0;
    result.ticks += *count as f64;
    result.active |= i > 0 && *count > 0;
    result.saturated |= i == states.len() - 1 && *count > 0;
    lower = upper;
  }
  if !result.weighted.is_finite() {
    return Err("PMP estimate overflow");
  }
  Ok(result)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Estimate {
  watts: f32,
  active: bool,
  saturated: bool,
}

// Observed-maximum normalization follows PWE's histogramTickRate / clusterTicks:
// https://github.com/kenshinice-ai/pwemacmonitor/blob/cd2c6ac3e346f7f36f7aaa82deb668e44090ba68/Sources/Core/Sampler.swift#L547-L563
// A rate is learned separately for each driver/group (not across dies). The
// observed maximum compensates for missing observations after power gating.
// It remains an estimate: startup while every cluster is gated can underestimate
// the full cadence. Rebase drops the learned rates; no fixed hardware rate is used.
#[derive(Default)]
struct PmpNormalizer {
  rates: HashMap<(u64, String), f64>,
  denominators: HashMap<(u64, String), f64>,
}

impl PmpNormalizer {
  fn estimate(
    &mut self,
    clusters: &[(ChannelId, String, Histogram)],
    dt: f64,
  ) -> Reading<Estimate> {
    if clusters.is_empty() {
      return Err("not discovered");
    }
    if !dt.is_finite() || dt <= 0.0 {
      return Err("invalid PMP interval");
    }

    let mut current: HashMap<(u64, String), f64> = HashMap::new();
    for (id, group, histogram) in clusters {
      let ticks = current.entry((id.0, group.clone())).or_default();
      *ticks = ticks.max(histogram.ticks);
    }
    if current.values().any(|ticks| *ticks <= 0.0) {
      return Err(NO_PMP_OBSERVATIONS);
    }

    for (domain, ticks) in current {
      let rate = self.rates.entry(domain.clone()).or_default();
      *rate = rate.max(ticks / dt);
      self.denominators.insert(domain, *rate * dt);
    }
    let mut result = Estimate::default();
    for (id, group, histogram) in clusters {
      let denominator = self.denominators[&(id.0, group.clone())];
      result.watts += (histogram.weighted / denominator) as f32;
      result.active |= histogram.active;
      result.saturated |= histogram.saturated;
    }
    if !result.watts.is_finite() {
      return Err("PMP estimate overflow");
    }
    Ok(result)
  }
}

#[derive(Default)]
struct CandidateHealth {
  zero_seconds: f64,
  evidence_seconds: f64,
  recovery_seconds: f64,
  was_positive: bool,
  stale: bool,
  status: &'static str,
}

impl CandidateHealth {
  fn observe(&mut self, reading: Reading<Estimate>, dt: f64, check_updates: bool, evidence: bool) {
    let Ok(value) = reading else {
      self.status = reading.unwrap_err();
      self.zero_seconds = 0.0;
      self.evidence_seconds = 0.0;
      self.recovery_seconds = 0.0;
      self.was_positive = false;
      return;
    };
    if !check_updates {
      self.status =
        if value.watts == 0.0 { "valid zero; activity not established" } else { "updating" };
      return;
    }

    // Energy Model: require 3 elapsed seconds without increments, including
    // >=1 second of activity evidence for this component. CPU can use residency
    // or PMP bins above the first; GPU/ANE require a positive independent energy
    // source. Idle-only zeroes stay uncertain. A positive sample after a stale period
    // may include old accumulated energy; discard it and require a further 3 s
    // of consecutive updates before making the candidate eligible again.
    if value.watts > 0.0 {
      self.zero_seconds = 0.0;
      self.evidence_seconds = 0.0;
      if self.stale {
        if self.was_positive {
          self.recovery_seconds += dt;
        }
        if self.recovery_seconds >= VERIFY_SECONDS {
          self.stale = false;
        }
      }
      self.was_positive = true;
      self.status =
        if self.stale { "waiting for sustained updates after staleness" } else { "updating" };
    } else {
      self.was_positive = false;
      self.recovery_seconds = 0.0;
      self.zero_seconds = (self.zero_seconds + dt).min(VERIFY_SECONDS);
      if evidence {
        self.evidence_seconds = (self.evidence_seconds + dt).min(1.0);
      }
      self.stale |= self.zero_seconds >= VERIFY_SECONDS && self.evidence_seconds >= 1.0;
      self.status = if self.stale {
        "not updating despite component activity"
      } else if self.zero_seconds >= VERIFY_SECONDS {
        "zero; activity not established (unverified)"
      } else {
        "zero; waiting for evidence of component activity"
      };
    }
  }
}

#[derive(Default)]
struct Selection {
  source: Option<usize>,
  missing_seconds: f64,
}

pub(crate) struct PowerState {
  pub(crate) channels: Vec<ChannelInfo>,
  mode: PowerMode,
  selected: [Selection; 3],
  health: [[CandidateHealth; 3]; 3],
  readings: [[Reading<Estimate>; 3]; 3],
  pmp: PmpNormalizer,
  frame: PowerFrame,
  elapsed: Duration,
  pub(crate) refresh: bool,
}

impl PowerState {
  pub(crate) fn new(channels: &[ChannelInfo], mode: PowerMode) -> Self {
    let mut seen = HashSet::new();
    let channels = channels
      .iter()
      .filter(|c| {
        channel_source(&c.group, &c.subgroup, &c.channel, &c.unit).is_some() && seen.insert(c.id)
      })
      .cloned()
      .collect();
    Self {
      channels,
      mode,
      selected: std::array::from_fn(|_| Selection::default()),
      health: std::array::from_fn(|_| std::array::from_fn(|_| CandidateHealth::default())),
      readings: [[Err("not sampled"); 3]; 3],
      pmp: PmpNormalizer::default(),
      frame: PowerFrame::default(),
      elapsed: Duration::ZERO,
      refresh: false,
    }
  }

  pub(crate) fn reset(&mut self) {
    *self = Self::new(&self.channels, self.mode);
  }

  pub(crate) fn resubscribe(&self, channels: &[ChannelInfo]) -> Self {
    let mut channels = channels.to_vec();
    // Re-enumeration must not turn a missing die/cluster into a complete CPU sum.
    // Retain the expected physical channels for this sampler's lifetime.
    for old in &self.channels {
      if !channels.iter().any(|channel| channel.id == old.id) {
        let mut missing = old.clone();
        missing.subscribed = false;
        channels.push(missing);
      }
    }
    Self::new(&channels, self.mode)
  }

  fn collect(&mut self, component: usize, source: usize, dt: f64) -> Reading<Estimate> {
    if self.frame.kinds.iter().any(|(id, kind)| {
      *kind == (component, source) && !self.channels.iter().any(|channel| channel.id == *id)
    }) {
      self.refresh = true;
      return Err("power channel set changed; resubscribing");
    }

    let mut count = 0;
    let mut result = Estimate::default();
    let mut clusters = Vec::new();
    for channel in &self.channels {
      if channel_source(&channel.group, &channel.subgroup, &channel.channel, &channel.unit)
        != Some((component, source))
      {
        continue;
      }
      count += 1;
      if !channel.subscribed {
        return Err("discovered but excluded by subscription");
      }
      let value = self
        .frame
        .values
        .get(&channel.id)
        .ok_or("channel missing from sample")?
        .as_ref()
        .map_err(|e| *e)?;
      match value {
        ChannelDelta::Scalar(watts) => {
          if !watts.is_finite() || *watts < 0.0 {
            return Err("invalid energy delta or counter reset");
          }
          result.watts += watts;
        }
        ChannelDelta::Histogram(states) => {
          clusters.push((channel.id, channel.group.clone(), histogram(states)?));
        }
      }
    }
    if count == 0 {
      return Err("not discovered");
    }
    if component == CPU && source == PMP {
      return self.pmp.estimate(&clusters, dt);
    }
    if !result.watts.is_finite() || result.watts < 0.0 {
      return Err("invalid aggregate energy");
    }
    Ok(result)
  }

  pub(crate) fn sample(
    &mut self,
    frame: PowerFrame,
    elapsed: Duration,
    cpu_active: f32,
  ) -> Result<[f32; 3], String> {
    self.frame = frame;
    self.elapsed = elapsed;
    let dt = elapsed.as_secs_f64();
    for component in 0..3 {
      for source in 0..3 {
        self.readings[component][source] = self.collect(component, source, dt);
      }
    }
    // Invalid windows must not carry a cadence estimate into a new epoch. A fully
    // gated interval is unavailable, but does not invalidate the previously learned rate.
    if self.readings[CPU][PMP].is_err() && self.readings[CPU][PMP] != Err(NO_PMP_OBSERVATIONS) {
      self.pmp = PmpNormalizer::default();
    }
    for component in 0..3 {
      let evidence = self.readings[component][CLPC].is_ok_and(|value| value.watts > 0.0)
        || if component == CPU {
          cpu_active > 0.01 || self.readings[CPU][PMP].is_ok_and(|value| value.active)
        } else {
          self.readings[component][PMP].is_ok_and(|value| value.watts > 0.0)
        };
      for source in 0..3 {
        self.health[component][source].observe(
          self.readings[component][source],
          dt,
          source == ENERGY_MODEL,
          evidence,
        );
      }
    }

    let mut watts = [0.0; 3];
    for component in 0..3 {
      let forced = match self.mode {
        PowerMode::Clpc => Some(CLPC),
        PowerMode::EnergyModel => Some(ENERGY_MODEL),
        PowerMode::Pmp if component == CPU => Some(PMP),
        _ => None,
      };
      if let Some(source) = forced {
        self.selected[component].source = Some(source);
        watts[component] = self.readings[component][source]
          .map_err(|reason| {
            format!(
              "{} power unavailable from {}: {}; fallback is disabled",
              COMPONENTS[component], SOURCES[source], reason
            )
          })?
          .watts;
        continue;
      }

      let usable = |source: usize| {
        self.readings[component][source].is_ok() && !self.health[component][source].stale
      };
      let selection = &mut self.selected[component];
      if let Some(source) = selection.source {
        if usable(source) {
          selection.missing_seconds = 0.0;
        } else if self.readings[component][source] == Err("channel missing from sample") {
          selection.missing_seconds += dt;
          if selection.missing_seconds < VERIFY_SECONDS {
            continue;
          }
          self.refresh = true;
          selection.source = None;
        } else {
          selection.source = None;
        }
      }
      if selection.source.is_none() {
        selection.source = (0..3).find(|&source| usable(source));
        selection.missing_seconds = 0.0;
      }
      if let Some(source) = selection.source {
        watts[component] = self.readings[component][source].unwrap().watts;
      }
    }
    Ok(watts)
  }

  #[cfg(feature = "app")]
  pub(crate) fn diagnostic(&self) -> String {
    use std::fmt::Write;
    let mut out = format!("Power interval {:.6}s ({:?})\n", self.elapsed.as_secs_f64(), self.mode);
    for component in 0..3 {
      let selected = self.selected[component]
        .source
        .map(|source| SOURCES[source])
        .unwrap_or("unavailable (numeric fallback 0)");
      writeln!(out, "{} selected={selected}", COMPONENTS[component]).unwrap();
      for source in 0..3 {
        let value = match self.readings[component][source] {
          Ok(value) => format!(
            "{:.6}W{}",
            value.watts,
            if value.saturated { ", top bin occupied; may underestimate" } else { "" }
          ),
          Err(reason) => reason.to_string(),
        };
        writeln!(out, "  {}: {value}; {}", SOURCES[source], self.health[component][source].status)
          .unwrap();
      }
    }
    for channel in &self.channels {
      writeln!(
        out,
        "  {} :: {} :: {} driver={:#x} channel={:#x} format={} unit={} subscribed={}",
        channel.group,
        channel.subgroup,
        channel.channel,
        channel.id.0,
        channel.id.1,
        channel.format,
        channel.unit,
        channel.subscribed
      )
      .unwrap();
      if let Some(Ok(ChannelDelta::Histogram(states))) = self.frame.values.get(&channel.id) {
        let denominator = self.pmp.denominators.get(&(channel.id.0, channel.group.clone()));
        writeln!(out, "    approximate PMP; midpoint bins, no SRAM; cadence inferred from observed maximum (may be incomplete at startup); denominator={denominator:?}; states={states:?}").unwrap();
      }
    }
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn channel(driver: u64, id: u64, group: &str, name: &str) -> ChannelInfo {
    let histogram = group.starts_with("PMP") && !name.starts_with("ANE");
    ChannelInfo {
      id: (driver, id),
      group: group.into(),
      subgroup: if histogram { "Energy" } else { "Energy Counters" }.into(),
      channel: name.into(),
      unit: if histogram { "events" } else { "nJ" }.into(),
      format: if histogram { 2 } else { 1 },
      subscribed: true,
    }
  }

  fn states(counts: [i64; 3]) -> Vec<(String, i64)> {
    [" 0.250 W ", "0.750W", "2W"]
      .into_iter()
      .zip(counts)
      .map(|(name, count)| (name.into(), count))
      .collect()
  }

  fn frame(items: &[(ChannelId, ChannelDelta)]) -> PowerFrame {
    let mut frame = PowerFrame::default();
    for (id, value) in items {
      frame.insert(*id, Ok(value.clone()));
    }
    frame
  }

  fn scalar(watts: f32) -> ChannelDelta {
    ChannelDelta::Scalar(watts)
  }
  fn hist(counts: [i64; 3]) -> ChannelDelta {
    ChannelDelta::Histogram(states(counts))
  }

  #[test]
  fn clpc_precedence_zero_deduplication_and_strict_mode() {
    let channels = [
      channel(1, 1, "CLPC", "CPU Energy"),
      channel(1, 1, "CLPC", "CPU Energy"),
      channel(1, 2, "Energy Model", "CPU Energy"),
      channel(1, 3, "PMP", "PACC"),
      channel(1, 4, "CLPC", "GPU Energy"),
      channel(1, 5, "CLPC", "ANE"),
    ];
    for mode in [PowerMode::Auto, PowerMode::Clpc] {
      for watts in [0.0, 1.25, 20.0] {
        let mut state = PowerState::new(&channels, mode);
        let values = frame(&[
          ((1, 1), scalar(watts)),
          ((1, 1), scalar(watts)),
          ((1, 2), scalar(2.0)),
          ((1, 3), hist([0, 100, 0])),
          ((1, 4), scalar(0.0)),
          ((1, 5), scalar(0.0)),
        ]);
        assert_eq!(state.sample(values, Duration::from_secs(1), 0.0).unwrap(), [watts, 0.0, 0.0]);
        assert_eq!(state.selected[CPU].source, Some(CLPC));
      }
    }
    let mut values = frame(&[((1, 2), scalar(3.0))]);
    values.insert((1, 1), Err("invalid energy delta or counter reset"));
    let mut state = PowerState::new(&channels, PowerMode::Auto);
    assert_eq!(state.sample(values, Duration::from_secs(1), 0.0).unwrap()[CPU], 3.0);
    for missing in [(1, 1), (1, 4), (1, 5)] {
      let mut state = PowerState::new(&channels, PowerMode::Clpc);
      let mut values =
        frame(&[((1, 1), scalar(1.0)), ((1, 4), scalar(0.0)), ((1, 5), scalar(0.0))]);
      values.values.remove(&missing);
      assert!(state.sample(values, Duration::from_secs(1), 0.0).is_err());
    }
  }

  #[test]
  fn stale_cpu_uses_elapsed_time_and_does_not_switch_on_a_late_update() {
    let channels = [
      channel(1, 1, "Energy Model", "CPU Energy"),
      channel(1, 2, "PMP", "PACC0"),
      channel(1, 3, "Energy Model", "GPU Energy"),
      channel(1, 4, "Energy Model", "ANE"),
    ];
    for milliseconds in [100, 500, 1000] {
      let mut state = PowerState::new(&channels, PowerMode::Auto);
      let mut elapsed = 0;
      while elapsed < 4000 {
        let value = state
          .sample(
            frame(&[
              ((1, 1), scalar(0.0)),
              ((1, 2), hist([0, milliseconds, 0])),
              ((1, 3), scalar(2.0)),
              ((1, 4), scalar(0.0)),
            ]),
            Duration::from_millis(milliseconds as u64),
            0.2,
          )
          .unwrap();
        elapsed += milliseconds;
        assert_eq!(&value[1..], &[2.0, 0.0]);
        if elapsed < 3000 {
          assert_eq!(value[CPU], 0.0);
        }
        if elapsed >= 3100 {
          assert_eq!(value[CPU], 0.5);
        }
      }
      let value = state
        .sample(
          frame(&[
            ((1, 1), scalar(50.0)),
            ((1, 2), hist([0, milliseconds, 0])),
            ((1, 3), scalar(2.0)),
            ((1, 4), scalar(0.0)),
          ]),
          Duration::from_millis(milliseconds as u64),
          0.2,
        )
        .unwrap();
      assert_eq!(value[CPU], 0.5);
      assert_eq!(state.selected[CPU].source, Some(PMP));
    }
  }

  #[test]
  fn legacy_energy_values_idle_uncertainty_and_component_independence() {
    let channels = [
      channel(1, 1, "Energy Model", "CPU Energy"),
      channel(2, 1, "Energy Model", "DIE_1_CPU Energy"),
      channel(1, 2, "Energy Model", "GPU Energy"),
      channel(1, 3, "Energy Model", "ANE"),
      channel(1, 4, "PMP", "ANE"),
    ];
    let mut state = PowerState::new(&channels, PowerMode::Auto);
    for cpu in [1.0, 0.0, 0.5] {
      let result = state
        .sample(
          frame(&[
            ((1, 1), scalar(cpu)),
            ((2, 1), scalar(cpu)),
            ((1, 2), scalar(0.7)),
            ((1, 3), scalar(0.0)),
            ((1, 4), scalar(0.8)),
          ]),
          Duration::from_millis(500),
          0.1,
        )
        .unwrap();
      assert_eq!(result, [cpu * 2.0, 0.7, 0.0]);
    }
    for _ in 0..10 {
      state
        .sample(
          frame(&[
            ((1, 1), scalar(0.0)),
            ((2, 1), scalar(0.0)),
            ((1, 2), scalar(0.7)),
            ((1, 3), scalar(0.0)),
          ]),
          Duration::from_secs(1),
          0.0,
        )
        .unwrap();
    }
    assert!(!state.health[CPU][ENERGY_MODEL].stale);
    state.reset();
    let result = state
      .sample(frame(&[((1, 2), scalar(0.7)), ((1, 4), scalar(0.8))]), Duration::from_secs(1), 0.1)
      .unwrap();
    assert_eq!(result, [0.0, 0.7, 0.8]);
    assert!(state.selected[CPU].source.is_none());
  }

  #[test]
  fn histogram_boundaries_resets_and_saturation() {
    let h = histogram(&states([2, 3, 5])).unwrap();
    assert_eq!(h.ticks, 10.0);
    assert_eq!(h.weighted, 8.625);
    assert!(h.saturated);
    assert!(histogram(&states([1, 0, 0])).unwrap().weighted == 0.125);
    assert_eq!(histogram(&states([0, 0, 0])).unwrap().ticks, 0.0);
    assert!(histogram(&states([-1, 0, 0])).is_err());
    assert!(histogram(&[]).is_err());
    for labels in [["1W", "0.5W"], ["1W", "1W"], ["nanW", "2W"], ["1W", ">2W"], ["1J", "2W"]] {
      let states = labels.map(|x| (x.to_owned(), 1));
      assert!(histogram(&states).is_err());
    }
  }

  #[test]
  fn pmp_domains_duplicates_missing_clusters_and_gating() {
    let channels = [
      channel(1, 1, "PMP0", "PACC"),
      channel(1, 2, "PMP0", "MACC0"),
      channel(2, 1, "PMP1", "PACC"),
      channel(2, 1, "PMP1", "PACC"),
    ];
    let mut state = PowerState::new(&channels, PowerMode::Pmp);
    let warm = frame(&[
      ((1, 1), hist([0, 100, 0])),
      ((1, 2), hist([0, 100, 0])),
      ((2, 1), hist([0, 200, 0])),
    ]);
    assert_eq!(state.sample(warm, Duration::from_secs(1), 0.5).unwrap()[CPU], 1.5);
    // Domain 1: half an interval and a gated cluster; domain 2: a quarter.
    let idle =
      frame(&[((1, 1), hist([0, 50, 0])), ((1, 2), hist([0, 0, 0])), ((2, 1), hist([0, 50, 0]))]);
    assert_eq!(state.sample(idle, Duration::from_secs(1), 0.1).unwrap()[CPU], 0.375);
    let missing = frame(&[((1, 1), hist([0, 100, 0])), ((2, 1), hist([0, 200, 0]))]);
    assert!(state.sample(missing, Duration::from_secs(1), 0.5).is_err());
    assert!(state.pmp.rates.is_empty());
    let mut channels = channels;
    channels[1].subscribed = false;
    let mut state = PowerState::new(&channels, PowerMode::Auto);
    let result = state
      .sample(
        frame(&[((1, 1), hist([0, 100, 0])), ((2, 1), hist([0, 200, 0]))]),
        Duration::from_secs(1),
        0.5,
      )
      .unwrap();
    assert_eq!(result[CPU], 0.0);
    assert_eq!(state.readings[CPU][PMP], Err("discovered but excluded by subscription"));
  }

  #[test]
  fn invalid_scalar_cannot_be_hidden_by_another_die() {
    let channels = [
      channel(1, 1, "CLPC", "CPU Energy"),
      channel(2, 1, "CLPC", "CPU Energy"),
      channel(1, 2, "Energy Model", "CPU Energy"),
    ];
    for value in [f32::NAN, f32::INFINITY, -1.0, i64::MIN as f32] {
      let mut state = PowerState::new(&channels, PowerMode::Auto);
      let values = frame(&[((1, 1), scalar(value)), ((2, 1), scalar(3.0)), ((1, 2), scalar(2.0))]);
      assert_eq!(state.sample(values, Duration::from_secs(1), 0.5).unwrap()[CPU], 2.0);
    }
  }

  #[test]
  fn ane_needs_its_own_activity_evidence_to_leave_zero_energy_model() {
    let channels = [
      channel(1, 1, "Energy Model", "CPU Energy"),
      channel(1, 2, "CLPC", "GPU Energy"),
      channel(1, 3, "Energy Model", "ANE"),
      channel(1, 4, "PMP", "ANE"),
    ];
    let mut state = PowerState::new(&channels, PowerMode::Auto);
    for _ in 0..10 {
      let result = state
        .sample(
          frame(&[
            ((1, 1), scalar(0.0)),
            ((1, 2), scalar(10.0)),
            ((1, 3), scalar(0.0)),
            ((1, 4), scalar(0.0)),
          ]),
          Duration::from_secs(1),
          0.0,
        )
        .unwrap();
      assert_eq!(result, [0.0, 10.0, 0.0]);
      assert!(!state.health[CPU][ENERGY_MODEL].stale);
      assert!(!state.health[2][ENERGY_MODEL].stale);
    }
    for _ in 0..3 {
      state
        .sample(
          frame(&[
            ((1, 1), scalar(0.0)),
            ((1, 2), scalar(10.0)),
            ((1, 3), scalar(0.0)),
            ((1, 4), scalar(0.5)),
          ]),
          Duration::from_secs(1),
          0.0,
        )
        .unwrap();
    }
    assert_eq!(state.selected[2].source, Some(PMP));
    assert!(!state.health[CPU][ENERGY_MODEL].stale);
  }

  #[test]
  fn reenumeration_does_not_forget_a_missing_cluster_or_ignore_a_new_one() {
    let channels = [channel(1, 1, "PMP", "EACC0"), channel(1, 2, "PMP", "PACC0")];
    let state = PowerState::new(&channels, PowerMode::Auto);
    let mut state = state.resubscribe(&channels[..1]);
    assert_eq!(
      state.sample(frame(&[((1, 1), hist([0, 100, 0]))]), Duration::from_secs(1), 0.5).unwrap()
        [CPU],
      0.0
    );
    assert_eq!(state.readings[CPU][PMP], Err("discovered but excluded by subscription"));
    let mut state = PowerState::new(&channels[..1], PowerMode::Auto);
    let mut values = frame(&[((1, 1), hist([0, 100, 0])), ((1, 2), hist([0, 100, 0]))]);
    values.kinds.insert((1, 2), (CPU, PMP));
    assert_eq!(state.sample(values, Duration::from_secs(1), 0.5).unwrap()[CPU], 0.0);
    assert!(state.refresh);
  }

  #[test]
  fn empty_pmp_intervals_are_unavailable_and_renewed_baselines_clear_cadence() {
    let channels = [channel(1, 1, "PMP", "PACC")];
    let mut state = PowerState::new(&channels, PowerMode::Pmp);
    state.sample(frame(&[((1, 1), hist([0, 100, 0]))]), Duration::from_secs(1), 0.5).unwrap();
    assert!(
      state.sample(frame(&[((1, 1), hist([0, 0, 0]))]), Duration::from_secs(1), 0.0).is_err()
    );
    assert_eq!(state.pmp.rates[&(1, "PMP".into())], 100.0);
    assert_eq!(
      state.sample(frame(&[((1, 1), hist([0, 50, 0]))]), Duration::from_secs(1), 0.5).unwrap()[CPU],
      0.25
    );
    state.reset();
    assert!(state.pmp.rates.is_empty());
    let issues = HashMap::from([((1, 1), "state layout changed; rebasing")]);
    assert!(state.sample(PowerFrame::with_issues(&issues), Duration::from_secs(1), 0.5).is_err());
  }

  #[test]
  fn loss_and_rebase_reconsider_sources_without_stale_values() {
    let channels =
      [channel(1, 1, "CLPC", "CPU Energy"), channel(1, 2, "Energy Model", "CPU Energy")];
    let mut state = PowerState::new(&channels, PowerMode::Auto);
    assert_eq!(
      state
        .sample(frame(&[((1, 1), scalar(5.0)), ((1, 2), scalar(2.0))]), Duration::from_secs(1), 0.1)
        .unwrap()[CPU],
      5.0
    );
    for expected in [0.0, 0.0, 2.0] {
      assert_eq!(
        state.sample(frame(&[((1, 2), scalar(2.0))]), Duration::from_secs(1), 0.1).unwrap()[CPU],
        expected
      );
    }
    assert!(state.refresh);
    // A recovered higher-priority source does not displace a working selection.
    assert_eq!(
      state
        .sample(frame(&[((1, 1), scalar(5.0)), ((1, 2), scalar(2.0))]), Duration::from_secs(1), 0.1)
        .unwrap()[CPU],
      2.0
    );
    state.reset();
    assert_eq!(
      state
        .sample(frame(&[((1, 1), scalar(5.0)), ((1, 2), scalar(2.0))]), Duration::from_secs(1), 0.1)
        .unwrap()[CPU],
      5.0
    );
  }

  #[test]
  fn forced_modes_do_not_fallback_and_energy_model_exposes_frozen_zero() {
    let channels = [
      channel(1, 1, "CLPC", "CPU Energy"),
      channel(1, 2, "Energy Model", "CPU Energy"),
      channel(1, 3, "Energy Model", "GPU Energy"),
      channel(1, 4, "Energy Model", "ANE"),
    ];
    let mut energy = PowerState::new(&channels, PowerMode::EnergyModel);
    for _ in 0..5 {
      assert_eq!(
        energy
          .sample(
            frame(&[
              ((1, 1), scalar(9.0)),
              ((1, 2), scalar(0.0)),
              ((1, 3), scalar(2.0)),
              ((1, 4), scalar(0.0))
            ]),
            Duration::from_secs(1),
            0.5
          )
          .unwrap(),
        [0.0, 2.0, 0.0]
      );
    }
    let mut pmp = PowerState::new(&channels, PowerMode::Pmp);
    assert!(
      pmp
        .sample(frame(&[((1, 1), scalar(9.0))]), Duration::from_secs(1), 0.5)
        .unwrap_err()
        .contains("PMP")
    );
    let mut conflicting = frame(&[((1, 1), scalar(1.0)), ((1, 1), scalar(2.0))]);
    assert_eq!(conflicting.values.remove(&(1, 1)), Some(Err("conflicting duplicate channel")));
  }
}
