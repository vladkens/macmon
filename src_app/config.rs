//! Persistent terminal UI settings.

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;

pub(crate) const TUI_MIN_MS: u32 = 250;
pub(crate) const TUI_MAX_MS: u32 = 10_000;

/// Graph style. Old configs used `Sparkline` / `Gauge`, which load as `Braille` / `Block`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy)]
pub enum ViewType {
  #[serde(alias = "Sparkline")]
  Braille,
  #[serde(alias = "Gauge")]
  Block,
}

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

/// Visible TUI panels. Fields missing in the config file default to visible.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy)]
#[serde(default)]
pub struct Panels {
  pub cpu: bool,
  pub gpu: bool,
  pub mem: bool,
  pub power: bool,
  pub proc: bool,
}

impl Default for Panels {
  fn default() -> Self {
    Self { cpu: true, gpu: true, mem: true, power: true, proc: true }
  }
}

impl Panels {
  /// Flips the panel bound to key `1`–`5` (cpu, gpu, mem, power, proc).
  /// Returns `false` and changes nothing for other keys.
  pub fn toggle(&mut self, key: char) -> bool {
    let shown = match key {
      '1' => &mut self.cpu,
      '2' => &mut self.gpu,
      '3' => &mut self.mem,
      '4' => &mut self.power,
      '5' => &mut self.proc,
      _ => return false,
    };

    *shown = !*shown;
    true
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
}

impl ProcSort {
  /// Next key in the `s` cycle: CPU → MEM → POWER → GPU → PID → NAME → CPU.
  pub fn next(self) -> Self {
    match self {
      Self::Cpu => Self::Mem,
      Self::Mem => Self::Power,
      Self::Power => Self::Gpu,
      Self::Gpu => Self::Pid,
      Self::Pid => Self::Name,
      Self::Name => Self::Cpu,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      Self::Cpu => "cpu",
      Self::Mem => "mem",
      Self::Power => "power",
      Self::Gpu => "gpu",
      Self::Pid => "pid",
      Self::Name => "name",
    }
  }
}

#[serde_inline_default]
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
  #[serde_inline_default(ViewType::Braille)]
  pub view_type: ViewType,

  #[serde_inline_default("default".to_string())]
  pub theme: String,

  #[serde_inline_default(1000)]
  pub interval: u32,

  #[serde_inline_default(false)]
  pub per_core_view: bool,

  #[serde_inline_default(RatioMode::Scaled)]
  pub ratio_mode: RatioMode,

  #[serde(default)]
  pub panels: Panels,

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

  pub fn set_theme(&mut self, name: &str) {
    self.theme = name.to_string();
    self.save();
  }

  pub fn next_view_type(&mut self) {
    self.view_type = match self.view_type {
      ViewType::Braille => ViewType::Block,
      ViewType::Block => ViewType::Braille,
    };
    self.save();
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

  pub fn toggle_per_core_view(&mut self) {
    self.per_core_view = !self.per_core_view;
    self.save();
  }

  pub fn toggle_ratio_mode(&mut self) {
    self.ratio_mode = match self.ratio_mode {
      RatioMode::Scaled => RatioMode::Active,
      RatioMode::Active => RatioMode::Scaled,
    };
    self.save();
  }

  /// Shows / hides the panel bound to key `1`–`5`; other keys are ignored.
  pub fn toggle_panel(&mut self, key: char) {
    if self.panels.toggle(key) {
      self.save();
    }
  }

  pub fn set_proc_sort(&mut self, sort: ProcSort, desc: bool) {
    self.proc_sort = sort;
    self.proc_sort_desc = desc;
    self.save();
  }
}

#[cfg(test)]
mod tests {
  use super::{Config, Panels, ProcSort, RatioMode, TUI_MAX_MS, TUI_MIN_MS, ViewType};

  fn parse(json: &str) -> Config {
    Config::from_reader(json.as_bytes())
  }

  fn assert_defaults(cfg: &Config) {
    assert_eq!(cfg.view_type, ViewType::Braille);
    assert_eq!(cfg.theme, "default");
    assert_eq!(cfg.interval, 1000);
    assert!(!cfg.per_core_view);
    assert_eq!(cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(cfg.panels, Panels::default());
    assert_eq!(cfg.proc_sort, ProcSort::Cpu);
    assert!(cfg.proc_sort_desc);
  }

  #[test]
  fn empty_json_loads_defaults() {
    assert_defaults(&parse("{}"));
    assert_defaults(&Config::default());

    let panels = Panels::default();
    assert!(panels.cpu && panels.gpu && panels.mem && panels.power && panels.proc);
  }

  #[test]
  fn malformed_json_loads_defaults() {
    assert_defaults(&parse(""));
    assert_defaults(&parse("not json"));
    assert_defaults(&parse(r#"{"view_type": "Unknown"}"#));
  }

  #[test]
  fn old_config_with_gauge_loads() {
    let cfg = parse(
      r#"{
        "view_type": "Gauge",
        "color": "Red",
        "interval": 500,
        "per_core_view": true,
        "ratio_mode": "Active"
      }"#,
    );

    assert_eq!(cfg.view_type, ViewType::Block);
    assert_eq!(cfg.theme, "default");
    assert_eq!(cfg.interval, 500);
    assert!(cfg.per_core_view);
    assert_eq!(cfg.ratio_mode, RatioMode::Active);
    assert_eq!(cfg.panels, Panels::default());
    assert_eq!(cfg.proc_sort, ProcSort::Cpu);
    assert!(cfg.proc_sort_desc);
  }

  #[test]
  fn old_config_with_sparkline_loads() {
    let cfg = parse(r#"{"view_type": "Sparkline", "color": "Green"}"#);
    assert_eq!(cfg.view_type, ViewType::Braille);
    assert_eq!(cfg.theme, "default");
  }

  #[test]
  fn interval_is_clamped_on_load() {
    assert_eq!(parse(r#"{"interval": 10}"#).interval, TUI_MIN_MS);
    assert_eq!(parse(r#"{"interval": 999999}"#).interval, TUI_MAX_MS);
  }

  #[test]
  fn partial_panels_default_to_visible() {
    let cfg = parse(r#"{"panels": {"proc": false, "gpu": false}}"#);
    assert_eq!(cfg.panels, Panels { gpu: false, proc: false, ..Panels::default() });
  }

  #[test]
  fn new_fields_round_trip() {
    let cfg = Config {
      view_type: ViewType::Block,
      theme: "nord".to_string(),
      panels: Panels { mem: false, ..Panels::default() },
      proc_sort: ProcSort::Power,
      proc_sort_desc: false,
      ..Config::default()
    };

    let json = serde_json::to_string(&cfg).unwrap();
    assert!(!json.contains("color"));
    assert!(json.contains(r#""view_type":"Block""#));

    let cfg = parse(&json);
    assert_eq!(cfg.view_type, ViewType::Block);
    assert_eq!(cfg.theme, "nord");
    assert_eq!(cfg.panels, Panels { mem: false, ..Panels::default() });
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
    ] {
      assert_eq!(parse(&format!(r#"{{"proc_sort": "{name}"}}"#)).proc_sort, key);
    }
  }

  #[test]
  fn toggle_panel_flips_one_panel() {
    let all = Panels::default();
    let cases = [
      ('1', Panels { cpu: false, ..all }),
      ('2', Panels { gpu: false, ..all }),
      ('3', Panels { mem: false, ..all }),
      ('4', Panels { power: false, ..all }),
      ('5', Panels { proc: false, ..all }),
    ];

    for (key, hidden) in cases {
      let mut cfg = Config::default();
      cfg.toggle_panel(key);
      assert_eq!(cfg.panels, hidden, "key {key}");
      cfg.toggle_panel(key);
      assert_eq!(cfg.panels, all, "key {key}");
    }
  }

  #[test]
  fn toggle_panel_ignores_other_keys() {
    let mut panels = Panels::default();
    for key in ['0', '6', '9', 'a', ' '] {
      assert!(!panels.toggle(key));
    }
    assert_eq!(panels, Panels::default());
    assert!(panels.toggle('5'));
    assert!(!panels.proc);
  }

  #[test]
  fn set_theme_updates_name() {
    let mut cfg = Config::default();
    cfg.set_theme("dracula");
    assert_eq!(cfg.theme, "dracula");
  }

  #[test]
  fn proc_sort_cycle_wraps() {
    let mut sort = ProcSort::Cpu;
    let mut labels = vec![sort.label()];
    for _ in 0..6 {
      sort = sort.next();
      labels.push(sort.label());
    }
    assert_eq!(labels, ["cpu", "mem", "power", "gpu", "pid", "name", "cpu"]);
  }

  #[test]
  fn set_proc_sort_updates_both_fields() {
    let mut cfg = Config::default();
    cfg.set_proc_sort(ProcSort::Name, false);
    assert_eq!(cfg.proc_sort, ProcSort::Name);
    assert!(!cfg.proc_sort_desc);
  }
}
