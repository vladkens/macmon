//! Persistent terminal UI settings.

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;

pub(crate) const TUI_MIN_MS: u32 = 250;
pub(crate) const TUI_MAX_MS: u32 = 10_000;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy)]
pub enum RatioMode {
  Scaled,
  Active,
}

impl RatioMode {
  pub fn label(self) -> &'static str {
    match self {
      Self::Scaled => "scaled",
      Self::Active => "active",
    }
  }
}

/// Process list sort key.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
pub enum ProcSort {
  Cpu,
  Mem,
  Power,
  Gpu,
  Pid,
  Name,
  User,
}

impl ProcSort {
  /// Next key in the `s` cycle: CPU → MEM → POWER → GPU → PID → NAME → USER → CPU.
  pub fn next(self) -> Self {
    match self {
      Self::Cpu => Self::Mem,
      Self::Mem => Self::Power,
      Self::Power => Self::Gpu,
      Self::Gpu => Self::Pid,
      Self::Pid => Self::Name,
      Self::Name => Self::User,
      Self::User => Self::Cpu,
    }
  }
}

/// Settings saved in `~/.config/macmon.json`. Fields of older versions (`color`, `theme`,
/// `view_type`, `per_core_view`, `panels`) are ignored, so old files keep loading.
#[serde_inline_default]
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
  #[serde_inline_default(1000)]
  pub interval: u32,

  #[serde_inline_default(RatioMode::Scaled)]
  pub ratio_mode: RatioMode,

  /// Process list below the metrics (`p`).
  #[serde_inline_default(true)]
  pub show_procs: bool,

  #[serde_inline_default(ProcSort::Cpu)]
  pub proc_sort: ProcSort,

  #[serde_inline_default(true)]
  pub proc_sort_desc: bool,
}

impl Default for Config {
  fn default() -> Self {
    serde_json::from_str("{}").unwrap()
  }
}

impl Config {
  fn normalize(mut self) -> Self {
    self.interval = self.interval.clamp(TUI_MIN_MS, TUI_MAX_MS);
    self
  }

  fn get_config_path() -> Option<String> {
    // keep tests from reading or overwriting the user's real config
    if cfg!(test) {
      return None;
    }

    let home = match std::env::var("HOME") {
      Ok(home) => home,
      Err(_) => return None,
    };

    let filepath = format!("{}/.config/macmon.json", home);
    let _ = std::fs::create_dir_all(std::path::Path::new(&filepath).parent().unwrap());
    Some(filepath)
  }

  /// Parses a config file; malformed content falls back to defaults.
  fn from_reader(reader: impl std::io::Read) -> Self {
    serde_json::from_reader::<_, Self>(reader).unwrap_or_default().normalize()
  }

  pub fn load() -> Self {
    match Self::get_config_path().and_then(|path| std::fs::File::open(path).ok()) {
      Some(file) => Self::from_reader(std::io::BufReader::new(file)),
      None => Self::default().normalize(),
    }
  }

  pub fn save(&self) {
    if let Some(path) = Self::get_config_path() {
      let file = match std::fs::File::create(path) {
        Ok(file) => file,
        Err(_) => return,
      };

      let writer = std::io::BufWriter::new(file);
      let _ = serde_json::to_writer_pretty(writer, self);
    }
  }

  pub fn dec_interval(&mut self) {
    let step = 250;
    self.interval = (self.interval.saturating_sub(step).div_ceil(step) * step).max(TUI_MIN_MS);
    self.save();
  }

  pub fn inc_interval(&mut self) {
    let step = 250;
    self.interval = (self.interval.saturating_add(step) / step * step).min(TUI_MAX_MS);
    self.save();
  }

  pub fn toggle_ratio_mode(&mut self) {
    self.ratio_mode = match self.ratio_mode {
      RatioMode::Scaled => RatioMode::Active,
      RatioMode::Active => RatioMode::Scaled,
    };
    self.save();
  }

  pub fn toggle_procs(&mut self) {
    self.show_procs = !self.show_procs;
    self.save();
  }

  pub fn set_proc_sort(&mut self, sort: ProcSort, desc: bool) {
    self.proc_sort = sort;
    self.proc_sort_desc = desc;
    self.save();
  }
}

#[cfg(test)]
mod tests {
  use super::{Config, ProcSort, RatioMode, TUI_MAX_MS, TUI_MIN_MS};

  fn parse(json: &str) -> Config {
    Config::from_reader(json.as_bytes())
  }

  fn assert_defaults(cfg: &Config) {
    assert_eq!(cfg.interval, 1000);
    assert_eq!(cfg.ratio_mode, RatioMode::Scaled);
    assert!(cfg.show_procs);
    assert_eq!(cfg.proc_sort, ProcSort::Cpu);
    assert!(cfg.proc_sort_desc);
  }

  #[test]
  fn empty_json_loads_defaults() {
    assert_defaults(&parse("{}"));
    assert_defaults(&Config::default());
  }

  #[test]
  fn malformed_json_loads_defaults() {
    assert_defaults(&parse(""));
    assert_defaults(&parse("not json"));
    assert_defaults(&parse(r#"{"interval": "fast"}"#));
  }

  #[test]
  fn old_config_fields_are_ignored() {
    // config of released versions
    let cfg = parse(
      r#"{
        "view_type": "Gauge",
        "color": "Red",
        "interval": 500,
        "per_core_view": true,
        "ratio_mode": "Active"
      }"#,
    );

    assert_eq!(cfg.interval, 500);
    assert_eq!(cfg.ratio_mode, RatioMode::Active);
    assert!(cfg.show_procs);
    assert_eq!(cfg.proc_sort, ProcSort::Cpu);
    assert!(cfg.proc_sort_desc);

    // themes, graph styles, panels and the cores row of earlier redesign builds, unknown values too
    for json in [
      r#"{"view_type": "Sparkline", "color": "Green"}"#,
      r#"{"view_type": "Braille", "theme": "nord"}"#,
      r#"{"view_type": "Block", "theme": "dracula"}"#,
      r#"{"view_type": "Unknown", "theme": 42, "color": null}"#,
      r#"{"per_core_view": true, "panels": {"cpu": false, "proc": false}}"#,
      r#"{"per_core_view": "yes", "panels": [1, 2]}"#,
    ] {
      assert_defaults(&parse(json));
    }
    let cfg = parse(r#"{"panels": {"proc": false}, "show_procs": false, "interval": 2000}"#);
    assert!(!cfg.show_procs);
    assert_eq!(cfg.interval, 2000);
  }

  #[test]
  fn interval_is_clamped_on_load() {
    assert_eq!(parse(r#"{"interval": 10}"#).interval, TUI_MIN_MS);
    assert_eq!(parse(r#"{"interval": 999999}"#).interval, TUI_MAX_MS);
  }

  #[test]
  fn new_fields_round_trip() {
    let cfg = Config {
      show_procs: false,
      proc_sort: ProcSort::Power,
      proc_sort_desc: false,
      ..Config::default()
    };

    let json = serde_json::to_string(&cfg).unwrap();
    for old in ["color", "theme", "view_type", "per_core_view", "panels"] {
      assert!(!json.contains(old), "{old} in {json}");
    }

    let cfg = parse(&json);
    assert!(!cfg.show_procs);
    assert_eq!(cfg.proc_sort, ProcSort::Power);
    assert!(!cfg.proc_sort_desc);
  }

  #[test]
  fn all_sort_keys_parse() {
    for (name, key) in [
      ("Cpu", ProcSort::Cpu),
      ("Mem", ProcSort::Mem),
      ("Power", ProcSort::Power),
      ("Gpu", ProcSort::Gpu),
      ("Pid", ProcSort::Pid),
      ("Name", ProcSort::Name),
      ("User", ProcSort::User),
    ] {
      assert_eq!(parse(&format!(r#"{{"proc_sort": "{name}"}}"#)).proc_sort, key);
    }
  }

  #[test]
  fn toggle_procs_flips_process_list() {
    let mut cfg = Config::default();
    cfg.toggle_procs();
    assert!(!cfg.show_procs);
    cfg.toggle_procs();
    assert!(cfg.show_procs);
  }

  #[test]
  fn proc_sort_cycle_wraps() {
    use ProcSort::*;
    let mut sorts = vec![Cpu];
    for _ in 0..7 {
      sorts.push(sorts.last().unwrap().next());
    }
    assert_eq!(sorts, [Cpu, Mem, Power, Gpu, Pid, Name, User, Cpu]);
  }

  #[test]
  fn set_proc_sort_updates_both_fields() {
    let mut cfg = Config::default();
    cfg.set_proc_sort(ProcSort::Name, false);
    assert_eq!(cfg.proc_sort, ProcSort::Name);
    assert!(!cfg.proc_sort_desc);
  }
}
