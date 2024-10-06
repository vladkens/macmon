use ratatui::style::Color;
use serde::{Deserialize, Serialize};

const COLORS_OPTIONS: [Color; 7] =
  [Color::Green, Color::Yellow, Color::Red, Color::Blue, Color::Magenta, Color::Cyan, Color::Reset];

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ViewType {
  Sparkline,
  Gauge,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
  pub view_type: ViewType,
  pub color: Color,
}

impl Config {
  fn get_config_path() -> Option<String> {
    let home = match std::env::var("HOME") {
      Ok(home) => home,
      Err(_) => return None,
    };

    let filepath = format!("{}/.config/macmon.json", home);
    let _ = std::fs::create_dir_all(std::path::Path::new(&filepath).parent().unwrap());
    Some(filepath)
  }

  pub fn load() -> Self {
    if let Some(path) = Self::get_config_path() {
      let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Self::default(),
      };

      let reader = std::io::BufReader::new(file);
      return match serde_json::from_reader(reader) {
        Ok(config) => config,
        Err(_) => Self::default(),
      };
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
}

impl Default for Config {
  fn default() -> Self {
    Self { color: COLORS_OPTIONS[0], view_type: ViewType::Sparkline }
  }
}
