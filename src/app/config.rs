//! Persistent terminal UI settings.

use std::fs::{self, File};
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

impl ViewType {
  pub fn label(self) -> &'static str {
    match self {
      Self::Graph => "graph",
      Self::Gauge => "gauge",
    }
  }
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

/// Process list sort key, one per column of the process table (see `tui::proc_view`).
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

/// The real user behind `sudo` (`SUDO_UID` / `SUDO_GID` while running as root): files saved into
/// their home are handed back to them, so their own runs can still update the settings.
fn sudo_owner() -> Option<(u32, u32)> {
  let var = |name| std::env::var(name).ok();
  owner_behind_sudo(unsafe { libc::geteuid() }, var("SUDO_UID"), var("SUDO_GID"))
}

fn owner_behind_sudo(euid: u32, uid: Option<String>, gid: Option<String>) -> Option<(u32, u32)> {
  if euid != 0 {
    return None;
  }
  Some((uid?.parse().ok()?, gid?.parse().ok()?))
}

fn give_to(path: &Path, owner: Option<(u32, u32)>) {
  if let Some((uid, gid)) = owner {
    let _ = std::os::unix::fs::chown(path, Some(uid), Some(gid));
  }
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

  /// Writes the settings to their file.
  pub fn save(&self) {
    let Some(path) = &self.path else { return };
    let owner = sudo_owner();
    if let Some(dir) = path.parent().filter(|dir| !dir.exists())
      && fs::create_dir_all(dir).is_ok()
    {
      give_to(dir, owner);
    }

    if let Ok(file) = File::create(path) {
      let _ = serde_json::to_writer_pretty(BufWriter::new(file), self);
      give_to(path, owner);
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

/// A config file in the temp directory for tests, removed with its directory when dropped.
#[cfg(test)]
pub(crate) struct TempConfig(PathBuf);

#[cfg(test)]
impl TempConfig {
  /// A path no other test uses, with no file there yet. Each test gets a directory of its own:
  /// with a shared one, a test removing it could race another creating its file there.
  pub(crate) fn new(name: &str) -> Self {
    let dir = std::env::temp_dir().join(format!("macmon-test-{}-{name}", std::process::id()));
    let path = dir.join("macmon.json");
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
    let _ = self.0.parent().map(fs::remove_dir);
  }
}

#[cfg(test)]
mod tests {
  use super::{Config, ProcSort, RatioMode, TUI_MIN_MS, TempConfig, ViewType, owner_behind_sudo};

  #[test]
  fn files_saved_under_sudo_go_to_the_real_user() {
    let id = |s: &str| Some(s.to_string());
    assert_eq!(owner_behind_sudo(0, id("501"), id("20")), Some((501, 20)));
    // not root, root without sudo, or broken variables: files stay as created
    assert_eq!(owner_behind_sudo(501, id("501"), id("20")), None);
    assert_eq!(owner_behind_sudo(0, None, None), None);
    assert_eq!(owner_behind_sudo(0, id("x"), id("20")), None);
  }

  fn parse(json: &str) -> Config {
    Config::from_reader(json.as_bytes())
  }

  #[test]
  fn released_configs_load() {
    // config of released versions: `color` and `per_core_view` are ignored, the rest is kept
    let cfg = parse(
      r#"{
        "view_type": "Gauge",
        "color": "Red",
        "interval": 500,
        "per_core_view": true,
        "ratio_mode": "Active"
      }"#,
    );
    assert_eq!(cfg.view_type, ViewType::Gauge);
    assert_eq!((cfg.interval, cfg.ratio_mode), (500, RatioMode::Active));
    assert_eq!((cfg.show_procs, cfg.proc_sort, cfg.proc_sort_desc), (true, ProcSort::Cpu, true));

    // the graph keeps its released name both ways, so released versions read our configs too
    assert_eq!(parse(r#"{"view_type": "Sparkline"}"#).view_type, ViewType::Graph);
    for (view_type, name) in [(ViewType::Graph, "Sparkline"), (ViewType::Gauge, "Gauge")] {
      let json = serde_json::to_string(&Config { view_type, ..Config::default() }).unwrap();
      assert!(json.contains(&format!(r#""view_type":"{name}""#)), "{json}");
    }
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

    // anything but a JSON object gives the defaults
    assert_eq!(parse("not json").interval, 1000);
  }

  #[test]
  fn settings_are_saved_to_the_file() {
    let file = TempConfig::new("settings_are_saved_to_the_file");
    // no file yet: the defaults, saved there on the first change
    let mut cfg = Config::load_from(Some(file.path()));
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
    cfg.set_proc_sort(ProcSort::Name, false);
    assert_eq!((saved("proc_sort"), saved("proc_sort_desc")), ("Name".into(), false.into()));

    // the next run starts where this one stopped
    let cfg = Config::load_from(Some(file.path()));
    assert_eq!((cfg.show_procs, cfg.view_type), (false, ViewType::Gauge));
    assert_eq!((cfg.ratio_mode, cfg.interval), (RatioMode::Active, 1250));
    assert_eq!((cfg.proc_sort, cfg.proc_sort_desc), (ProcSort::Name, false));
  }

  #[test]
  fn interval_from_the_command_line_is_not_saved() {
    let file = TempConfig::new("interval_from_the_command_line");
    let mut cfg = Config::load_from(Some(file.path()));
    cfg.inc_interval();
    assert_eq!(file.saved()["interval"], 1250);

    // `-i 500`: used for this run, clamped like a saved value
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
  }
}
