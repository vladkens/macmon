use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use std::path::PathBuf;

const COLORS_OPTIONS: [Color; 7] =
  [Color::Green, Color::Yellow, Color::Red, Color::Blue, Color::Magenta, Color::Cyan, Color::Reset];

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ViewType {
  Sparkline,
  Gauge,
}

#[serde_inline_default]
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
  #[serde_inline_default(ViewType::Sparkline)]
  pub view_type: ViewType,

  #[serde_inline_default(COLORS_OPTIONS[0])]
  pub color: Color,

  #[serde_inline_default(1000)]
  pub interval: u32,
}

impl Default for Config {
  fn default() -> Self {
    serde_json::from_str("{}").unwrap()
  }
}

impl Config {
  fn get_config_dir() -> Option<PathBuf> {
    let home = match std::env::var("HOME") {
      Ok(home) => home,
      Err(_) => return None,
    };

    let dir = PathBuf::from(home).join(".config");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
  }

  fn get_config_path() -> Option<String> {
    let path = Self::get_config_dir()?.join("macmon.json");
    Some(path.to_string_lossy().to_string())
  }

  pub fn get_server_pid_path() -> Option<PathBuf> {
    Some(Self::get_config_dir()?.join("macmon-server.pid"))
  }

  pub fn load() -> Self {
    if let Some(path) = Self::get_config_path() {
      let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Self::default(),
      };

      let reader = std::io::BufReader::new(file);
      return serde_json::from_reader(reader).unwrap_or_default();
    }

    Self::default()
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

  pub fn next_color(&mut self) {
    self.color = match COLORS_OPTIONS.iter().position(|&c| c == self.color) {
      Some(idx) => COLORS_OPTIONS[(idx + 1) % COLORS_OPTIONS.len()],
      None => COLORS_OPTIONS[0],
    };
    self.save();
  }

  pub fn next_view_type(&mut self) {
    self.view_type = match self.view_type {
      ViewType::Sparkline => ViewType::Gauge,
      ViewType::Gauge => ViewType::Sparkline,
    };
    self.save();
  }

  pub fn dec_interval(&mut self) {
    let step = 250;
    self.interval = (self.interval.saturating_sub(step).div_ceil(step) * step).max(step);
    self.save();
  }

  pub fn inc_interval(&mut self) {
    let step = 250;
    self.interval = (self.interval.saturating_add(step) / step * step).min(10_000);
    self.save();
  }
}
