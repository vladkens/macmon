//! Persistent terminal UI settings.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use serde_json::{Map, Value};

pub(crate) const TUI_MIN_MS: u32 = 250;
pub(crate) const TUI_MAX_MS: u32 = 10_000;

/// Look of the CPU cluster, GPU and RAM boxes (`v`). Saved under the names of released versions,
/// so their configs keep the user's choice and they read ours.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
pub enum ViewType {
  /// History graph (`Sparkline` in released versions).
  #[serde(rename = "Sparkline")]
  Graph,
  /// Bar filled to the current load.
  Gauge,
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

/// Settings saved in `~/.config/macmon.json`. Unknown fields (`color` and `per_core_view` of
/// released versions) are ignored, so old files keep loading.
#[serde_inline_default]
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
  /// Graph or gauge in the CPU cluster, GPU and RAM boxes (`v`).
  #[serde_inline_default(ViewType::Graph)]
  pub view_type: ViewType,

  /// Saved interval; `interval()` is the one in use.
  #[serde_inline_default(1000)]
  interval: u32,

  #[serde_inline_default(RatioMode::Scaled)]
  pub ratio_mode: RatioMode,

  /// Process list below the metrics (`p`).
  #[serde_inline_default(true)]
  pub show_procs: bool,

  #[serde_inline_default(ProcSort::Cpu)]
  pub proc_sort: ProcSort,

  #[serde_inline_default(true)]
  pub proc_sort_desc: bool,

  /// Interval from `-i` for this run only: used instead of the saved one, never saved. `-` / `+`
  /// replace both.
  #[serde(skip)]
  run_interval: Option<u32>,

  /// File the settings are saved to on every change; `None` keeps them in memory only.
  #[serde(skip)]
  path: Option<PathBuf>,
}

impl Default for Config {
  fn default() -> Self {
    serde_json::from_str("{}").unwrap()
  }
}

/// Whether macmon runs as root through `sudo`.
fn under_sudo() -> bool {
  sudo_root(unsafe { libc::geteuid() }, std::env::var_os("SUDO_UID"))
}

fn sudo_root(euid: u32, sudo_uid: Option<OsString>) -> bool {
  euid == 0 && sudo_uid.is_some_and(|uid| !uid.is_empty())
}

impl Config {
  fn normalize(mut self) -> Self {
    self.interval = self.interval.clamp(TUI_MIN_MS, TUI_MAX_MS);
    self
  }

  /// `~/.config/macmon.json`; none in tests, so they never read or overwrite the user's settings.
  fn default_path() -> Option<PathBuf> {
    if cfg!(test) {
      return None;
    }

    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join(".config").join("macmon.json"))
  }

  /// Parses a config file. A field with a bad value (wrong type, unknown name) gets its default
  /// and the other fields keep theirs; anything but a JSON object gives the defaults.
  fn from_reader(reader: impl Read) -> Self {
    let Ok(Value::Object(fields)) = serde_json::from_reader(reader) else {
      return Self::default().normalize();
    };

    // each field on its own, next to the defaults of the others
    let valid = |(key, value): &(String, Value)| {
      let field = Map::from_iter([(key.clone(), value.clone())]);
      serde_json::from_value::<Self>(Value::Object(field)).is_ok()
    };
    let fields: Map<String, Value> = fields.into_iter().filter(valid).collect();
    serde_json::from_value::<Self>(Value::Object(fields)).unwrap_or_default().normalize()
  }

  pub fn load() -> Self {
    Self::load_from(Self::default_path())
  }

  /// Settings from the file at `path` (the defaults when it is missing), saved back to it.
  pub(crate) fn load_from(path: Option<PathBuf>) -> Self {
    let file = path.as_ref().and_then(|path| File::open(path).ok());
    let cfg = match file {
      Some(file) => Self::from_reader(BufReader::new(file)),
      None => Self::default().normalize(),
    };
    Self { path, ..cfg }
  }

  /// Saves the settings. Under `sudo` (which keeps `HOME`) only an existing file is rewritten: a
  /// new file or directory would belong to root, and the user's own runs couldn't save any more.
  pub fn save(&self) {
    self.write(under_sudo());
  }

  /// Writes the settings to their file; with `existing_only`, only to a file that exists.
  fn write(&self, existing_only: bool) {
    let Some(path) = &self.path else { return };
    let file = if existing_only {
      OpenOptions::new().write(true).truncate(true).open(path)
    } else {
      if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
      }
      File::create(path)
    };

    if let Ok(file) = file {
      let _ = serde_json::to_writer_pretty(BufWriter::new(file), self);
    }
  }

  /// Update interval in use: the one from `-i`, else the saved one.
  pub fn interval(&self) -> u32 {
    self.run_interval.unwrap_or(self.interval)
  }

  /// Uses `msec` (clamped) for this run without saving it, as `-i` does.
  pub fn set_run_interval(&mut self, msec: u32) {
    self.run_interval = Some(msec.clamp(TUI_MIN_MS, TUI_MAX_MS));
  }

  /// Sets and saves the interval, which replaces one from `-i`.
  fn set_interval(&mut self, msec: u32) {
    self.interval = msec;
    self.run_interval = None;
    self.save();
  }

  pub fn dec_interval(&mut self) {
    let step = 250;
    self.set_interval((self.interval().saturating_sub(step).div_ceil(step) * step).max(TUI_MIN_MS));
  }

  pub fn inc_interval(&mut self) {
    let step = 250;
    self.set_interval((self.interval().saturating_add(step) / step * step).min(TUI_MAX_MS));
  }

  pub fn toggle_view_type(&mut self) {
    self.view_type = match self.view_type {
      ViewType::Graph => ViewType::Gauge,
      ViewType::Gauge => ViewType::Graph,
    };
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

/// A config file in the temp directory for tests, removed when dropped.
#[cfg(test)]
pub(crate) struct TempConfig(PathBuf);

#[cfg(test)]
impl TempConfig {
  /// A path no other test uses, with no file there yet.
  pub(crate) fn new(name: &str) -> Self {
    let dir = std::env::temp_dir().join(format!("macmon-test-{}", std::process::id()));
    let path = dir.join(format!("{name}.json"));
    let _ = fs::remove_file(&path);
    Self(path)
  }

  pub(crate) fn path(&self) -> PathBuf {
    self.0.clone()
  }

  /// The saved settings as JSON.
  pub(crate) fn saved(&self) -> Value {
    let text = fs::read_to_string(&self.0).expect("settings saved");
    serde_json::from_str(&text).expect("settings are JSON")
  }
}

#[cfg(test)]
impl Drop for TempConfig {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
    // the directory goes with the last file
    let _ = self.0.parent().map(fs::remove_dir);
  }
}

#[cfg(test)]
mod tests {
  use std::ffi::OsString;
  use std::fs;

  use super::{
    Config, ProcSort, RatioMode, TUI_MAX_MS, TUI_MIN_MS, TempConfig, ViewType, sudo_root,
  };

  fn parse(json: &str) -> Config {
    Config::from_reader(json.as_bytes())
  }

  fn assert_defaults(cfg: &Config) {
    assert_eq!(cfg.view_type, ViewType::Graph);
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
    for json in ["", "not json", "[1, 2]", "42", r#"{"interval": 500"#] {
      assert_defaults(&parse(json));
    }
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

    // the chart view of released versions is kept
    assert_eq!(cfg.view_type, ViewType::Gauge);
    assert_eq!(cfg.interval, 500);
    assert_eq!(cfg.ratio_mode, RatioMode::Active);
    assert!(cfg.show_procs);
    assert_eq!(cfg.proc_sort, ProcSort::Cpu);
    assert!(cfg.proc_sort_desc);

    // fields of earlier builds of this redesign
    let cfg = parse(r#"{"theme": "nord", "panels": {"proc": false}, "show_procs": false}"#);
    assert!(!cfg.show_procs);
    assert_eq!(cfg.interval, 1000);
  }

  #[test]
  fn bad_values_fall_back_one_field_at_a_time() {
    // each bad value gets its default, the good ones stay
    let cfg = parse(
      r#"{
        "view_type": "Braille",
        "interval": 500,
        "ratio_mode": 3,
        "show_procs": "no",
        "proc_sort": "Bogus",
        "proc_sort_desc": false
      }"#,
    );
    assert_eq!((cfg.view_type, cfg.ratio_mode), (ViewType::Graph, RatioMode::Scaled));
    assert_eq!((cfg.show_procs, cfg.proc_sort), (true, ProcSort::Cpu));
    assert_eq!((cfg.interval, cfg.proc_sort_desc), (500, false));

    let cfg = parse(r#"{"view_type": "Gauge", "interval": "fast", "proc_sort": null}"#);
    assert_eq!(
      (cfg.view_type, cfg.interval, cfg.proc_sort),
      (ViewType::Gauge, 1000, ProcSort::Cpu)
    );
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
    for old in ["color", "per_core_view", "path", "run_interval"] {
      assert!(!json.contains(old), "{old} in {json}");
    }

    let cfg = parse(&json);
    assert!(!cfg.show_procs);
    assert_eq!(cfg.proc_sort, ProcSort::Power);
    assert!(!cfg.proc_sort_desc);
  }

  #[test]
  fn view_type_uses_released_names() {
    // configs of released versions
    assert_eq!(parse(r#"{"view_type": "Sparkline"}"#).view_type, ViewType::Graph);
    assert_eq!(parse(r#"{"view_type": "Gauge"}"#).view_type, ViewType::Gauge);

    // saved under the same names, so released versions read it back
    for (view_type, name) in [(ViewType::Graph, "Sparkline"), (ViewType::Gauge, "Gauge")] {
      let json = serde_json::to_string(&Config { view_type, ..Config::default() }).unwrap();
      assert!(json.contains(&format!(r#""view_type":"{name}""#)), "{json}");
      assert_eq!(parse(&json).view_type, view_type);
    }

    // an unknown value falls back to the graph and keeps the other settings
    for value in [r#""Braille""#, r#""Block""#, r#""gauge""#, "42", "null", "{}"] {
      let cfg =
        parse(&format!(r#"{{"view_type": {value}, "interval": 500, "show_procs": false}}"#));
      assert_eq!(cfg.view_type, ViewType::Graph, "{value}");
      assert_eq!((cfg.interval, cfg.show_procs), (500, false), "{value}");
    }
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
  fn proc_sort_cycle_wraps() {
    use ProcSort::*;
    let mut sorts = vec![Cpu];
    for _ in 0..7 {
      sorts.push(sorts.last().unwrap().next());
    }
    assert_eq!(sorts, [Cpu, Mem, Power, Gpu, Pid, Name, User, Cpu]);
  }

  #[test]
  fn every_change_is_saved_to_the_file() {
    let file = TempConfig::new("every_change");
    // no file yet: the defaults, saved there on the first change
    let mut cfg = Config::load_from(Some(file.path()));
    assert_defaults(&cfg);
    assert!(!file.path().exists());

    let saved = |field: &str| file.saved()[field].clone();
    cfg.toggle_procs();
    assert_eq!(saved("show_procs"), false);
    cfg.toggle_view_type();
    assert_eq!(saved("view_type"), "Gauge");
    cfg.toggle_ratio_mode();
    assert_eq!(saved("ratio_mode"), "Active");
    cfg.inc_interval();
    assert_eq!(saved("interval"), 1250);
    cfg.dec_interval();
    assert_eq!(saved("interval"), 1000);
    cfg.set_proc_sort(ProcSort::Name, false);
    assert_eq!((saved("proc_sort"), saved("proc_sort_desc")), ("Name".into(), false.into()));

    // the next run starts where this one stopped
    let cfg = Config::load_from(Some(file.path()));
    assert_eq!((cfg.show_procs, cfg.view_type), (false, ViewType::Gauge));
    assert_eq!((cfg.ratio_mode, cfg.interval), (RatioMode::Active, 1000));
    assert_eq!((cfg.proc_sort, cfg.proc_sort_desc), (ProcSort::Name, false));

    // and back
    let mut cfg = cfg;
    cfg.toggle_view_type();
    assert_eq!(file.saved()["view_type"], "Sparkline");
    assert_eq!(Config::load_from(Some(file.path())).view_type, ViewType::Graph);
  }

  #[test]
  fn interval_from_the_command_line_is_not_saved() {
    let file = TempConfig::new("interval_from_the_command_line");
    let mut cfg = Config::load_from(Some(file.path()));
    cfg.inc_interval();
    assert_eq!(file.saved()["interval"], 1250);

    // `-i 500`: used for this run, clamped like a saved value
    cfg.set_run_interval(500);
    assert_eq!(cfg.interval(), 500);
    cfg.set_run_interval(10);
    assert_eq!(cfg.interval(), TUI_MIN_MS);
    cfg.set_run_interval(500);

    // other settings save the interval of the file, not the one from `-i`
    cfg.toggle_ratio_mode();
    assert_eq!((file.saved()["interval"].clone(), cfg.interval()), (1250.into(), 500));
    assert_eq!(Config::load_from(Some(file.path())).interval(), 1250);

    // `-` / `+` step from the interval in use and save it
    cfg.dec_interval();
    assert_eq!((file.saved()["interval"].clone(), cfg.interval()), (250.into(), 250));
    cfg.toggle_procs();
    assert_eq!(file.saved()["interval"], 250);
    cfg.set_run_interval(2000);
    cfg.inc_interval();
    assert_eq!((file.saved()["interval"].clone(), cfg.interval()), (2250.into(), 2250));
  }

  #[test]
  fn settings_without_a_file_stay_in_memory() {
    let mut cfg = Config::default();
    cfg.toggle_procs();
    assert!(!cfg.show_procs);
    assert!(Config::load_from(None).show_procs);
  }

  #[test]
  fn under_sudo_only_an_existing_file_is_rewritten() {
    let file = TempConfig::new("under_sudo");
    let mut cfg = Config::load_from(Some(file.path()));
    cfg.show_procs = false;

    // no file, no directory: nothing is created that root would own
    cfg.write(true);
    assert!(!file.path().exists());

    // a file the user's own runs created is rewritten in place
    cfg.write(false);
    cfg.show_procs = true;
    cfg.write(true);
    assert_eq!(file.saved()["show_procs"], true);
    assert!(fs::read_to_string(file.path()).unwrap().contains("\"interval\": 1000"));

    // root through sudo; root logged in, a user, an empty SUDO_UID aren't
    let uid = |uid: &str| Some(OsString::from(uid));
    assert!(sudo_root(0, uid("501")));
    assert!(!sudo_root(0, None));
    assert!(!sudo_root(0, uid("")));
    assert!(!sudo_root(501, uid("501")));
    assert!(!sudo_root(501, None));
  }
}
