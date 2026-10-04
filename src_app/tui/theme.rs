//! Color themes and load gradients.

use ratatui::style::Color;

const fn rgb(hex: u32) -> Color {
  Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Named color palette. Built-in palettes are defined in RGB; `adapt` maps them to the
/// xterm-256 palette for terminals without truecolor support.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
  pub name: &'static str,
  pub border: Color,
  pub title: Color,
  pub text: Color,
  pub dim: Color,
  pub selection: Color,
  /// Load gradient stops: low, mid, high.
  stops: [Color; 3],
  truecolor: bool,
}

const fn theme(
  name: &'static str,
  [border, title, text, dim, selection]: [Color; 5],
  stops: [Color; 3],
) -> Theme {
  Theme { name, border, title, text, dim, selection, stops, truecolor: true }
}

pub const THEMES: [Theme; 6] = [
  theme(
    "default",
    [rgb(0x4e9a68), rgb(0x8fd6a4), Color::Reset, rgb(0x808080), rgb(0x3a4a6a)],
    [rgb(0x50f095), rgb(0xf2e266), rgb(0xfa1e1e)],
  ),
  theme(
    "nord",
    [rgb(0x81a1c1), rgb(0x88c0d0), rgb(0xd8dee9), rgb(0x4c566a), rgb(0x434c5e)],
    [rgb(0xa3be8c), rgb(0xebcb8b), rgb(0xbf616a)],
  ),
  theme(
    "dracula",
    [rgb(0xbd93f9), rgb(0xff79c6), rgb(0xf8f8f2), rgb(0x6272a4), rgb(0x44475a)],
    [rgb(0x50fa7b), rgb(0xf1fa8c), rgb(0xff5555)],
  ),
  theme(
    "gruvbox",
    [rgb(0x928374), rgb(0xfabd2f), rgb(0xebdbb2), rgb(0x665c54), rgb(0x504945)],
    [rgb(0xb8bb26), rgb(0xfabd2f), rgb(0xfb4934)],
  ),
  theme(
    "tokyo-night",
    [rgb(0x7aa2f7), rgb(0x7dcfff), rgb(0xc0caf5), rgb(0x565f89), rgb(0x283457)],
    [rgb(0x9ece6a), rgb(0xe0af68), rgb(0xf7768e)],
  ),
  theme(
    "mono",
    [Color::Reset, Color::Reset, Color::Reset, Color::DarkGray, Color::DarkGray],
    [Color::Reset, Color::Reset, Color::Reset],
  ),
];

impl Default for Theme {
  fn default() -> Self {
    THEMES[0]
  }
}

impl Theme {
  /// Resolves a theme by name (unknown names fall back to `default`) for the given color support.
  pub fn new(name: &str, truecolor: bool) -> Self {
    Self::find(name).map_or(THEMES[0], |idx| THEMES[idx]).adapt(truecolor)
  }

  /// Next built-in theme (wraps around), keeping the current color support.
  pub fn next(&self) -> Self {
    let idx = Self::find(self.name).unwrap_or(0);
    THEMES[(idx + 1) % THEMES.len()].adapt(self.truecolor)
  }

  fn find(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name == name)
  }

  /// Maps RGB colors to the xterm-256 palette when the terminal lacks truecolor support.
  fn adapt(self, truecolor: bool) -> Self {
    let fit = |c| fit_color(c, truecolor);
    Self {
      border: fit(self.border),
      title: fit(self.title),
      text: fit(self.text),
      dim: fit(self.dim),
      selection: fit(self.selection),
      truecolor,
      ..self
    }
  }

  /// Load color for `t` in `0.0..=1.0` (low → mid → high), clamped outside the range.
  pub fn gradient(&self, t: f64) -> Color {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let [low, mid, high] = self.stops;
    let color = if t <= 0.5 { lerp(low, mid, t * 2.0) } else { lerp(mid, high, (t - 0.5) * 2.0) };
    fit_color(color, self.truecolor)
  }
}

fn lerp(a: Color, b: Color, t: f64) -> Color {
  match (a, b) {
    (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
      let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
      Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
    }
    // non-RGB stops can't be blended, pick the nearest one
    _ => {
      if t < 0.5 {
        a
      } else {
        b
      }
    }
  }
}

/// True when `COLORTERM` advertises 24-bit color support.
pub fn detect_truecolor() -> bool {
  supports_truecolor(std::env::var("COLORTERM").ok().as_deref())
}

fn supports_truecolor(colorterm: Option<&str>) -> bool {
  matches!(colorterm, Some("truecolor" | "24bit"))
}

fn fit_color(color: Color, truecolor: bool) -> Color {
  match color {
    Color::Rgb(r, g, b) if !truecolor => Color::Indexed(rgb_to_256(r, g, b)),
    color => color,
  }
}

/// Nearest xterm-256 color: either a 6x6x6 color cube entry (16..=231) or a gray ramp entry
/// (232..=255).
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
  const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
  let cube_idx = |v: u8| {
    if v < 48 {
      0
    } else if v < 115 {
      1
    } else {
      (v - 35) / 40
    }
  };
  let dist = |(r2, g2, b2): (u8, u8, u8)| {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
    d(r, r2) + d(g, g2) + d(b, b2)
  };

  let (ri, gi, bi) = (cube_idx(r), cube_idx(g), cube_idx(b));
  let cube = (LEVELS[ri as usize], LEVELS[gi as usize], LEVELS[bi as usize]);

  let avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
  let gray_idx = if avg > 238 { 23 } else { avg.saturating_sub(3) / 10 };
  let gray = 8 + 10 * gray_idx;

  if dist((gray, gray, gray)) < dist(cube) { 232 + gray_idx } else { 16 + 36 * ri + 6 * gi + bi }
}

#[cfg(test)]
mod tests {
  use ratatui::style::Color;

  use super::{THEMES, Theme, rgb_to_256, supports_truecolor};

  fn by_name(name: &str) -> Theme {
    Theme::new(name, true)
  }

  #[test]
  fn gradient_endpoints_and_midpoint() {
    let theme = by_name("default");
    assert_eq!(theme.gradient(0.0), Color::Rgb(0x50, 0xf0, 0x95));
    assert_eq!(theme.gradient(0.5), Color::Rgb(0xf2, 0xe2, 0x66));
    assert_eq!(theme.gradient(1.0), Color::Rgb(0xfa, 0x1e, 0x1e));
  }

  #[test]
  fn gradient_interpolates_between_stops() {
    let theme = by_name("default");
    // halfway between low (0x50f095) and mid (0xf2e266)
    assert_eq!(theme.gradient(0.25), Color::Rgb(0xa1, 0xe9, 0x7e));
    // halfway between mid (0xf2e266) and high (0xfa1e1e)
    assert_eq!(theme.gradient(0.75), Color::Rgb(0xf6, 0x80, 0x42));
  }

  #[test]
  fn gradient_clamps_out_of_range() {
    let theme = by_name("nord");
    assert_eq!(theme.gradient(-1.0), theme.gradient(0.0));
    assert_eq!(theme.gradient(2.0), theme.gradient(1.0));
    assert_eq!(theme.gradient(f64::NAN), theme.gradient(0.0));
  }

  #[test]
  fn gradient_with_non_rgb_stops() {
    let theme = by_name("mono");
    for t in [0.0, 0.3, 0.5, 0.8, 1.0] {
      assert_eq!(theme.gradient(t), Color::Reset);
    }
  }

  #[test]
  fn gradient_without_truecolor_is_indexed() {
    let theme = Theme::new("default", false);
    assert_eq!(theme.gradient(0.0), Color::Indexed(rgb_to_256(0x50, 0xf0, 0x95)));
    assert_eq!(theme.gradient(1.0), Color::Indexed(196));
    assert!(matches!(theme.gradient(0.37), Color::Indexed(_)));
  }

  #[test]
  fn maps_pure_colors_to_256() {
    assert_eq!(rgb_to_256(0, 0, 0), 16);
    assert_eq!(rgb_to_256(255, 255, 255), 231);
    assert_eq!(rgb_to_256(255, 0, 0), 196);
    assert_eq!(rgb_to_256(0, 255, 0), 46);
    assert_eq!(rgb_to_256(0, 0, 255), 21);
    assert_eq!(rgb_to_256(255, 255, 0), 226);
    assert_eq!(rgb_to_256(95, 135, 175), 67);
  }

  #[test]
  fn maps_grays_to_256() {
    assert_eq!(rgb_to_256(8, 8, 8), 232);
    assert_eq!(rgb_to_256(128, 128, 128), 244);
    assert_eq!(rgb_to_256(130, 130, 130), 244);
    assert_eq!(rgb_to_256(238, 238, 238), 255);
    assert_eq!(rgb_to_256(250, 250, 250), 231); // closer to cube white than to gray 238
  }

  #[test]
  fn adapt_maps_only_rgb_colors() {
    let theme = Theme::new("default", false);
    assert_eq!(theme.text, Color::Reset);
    assert!(matches!(theme.border, Color::Indexed(_)));
    assert!(matches!(theme.dim, Color::Indexed(_)));

    let mono = Theme::new("mono", false);
    assert_eq!(mono.border, Color::Reset);
    assert_eq!(mono.dim, Color::DarkGray);

    let truecolor = Theme::new("default", true);
    assert!(matches!(truecolor.border, Color::Rgb(..)));
  }

  #[test]
  fn detects_truecolor_from_colorterm() {
    assert!(supports_truecolor(Some("truecolor")));
    assert!(supports_truecolor(Some("24bit")));
    assert!(!supports_truecolor(Some("256color")));
    assert!(!supports_truecolor(Some("")));
    assert!(!supports_truecolor(None));
  }

  #[test]
  fn theme_cycle_wraps() {
    let mut theme = Theme::default();
    let mut names = vec![theme.name];
    for _ in 0..THEMES.len() {
      theme = theme.next();
      names.push(theme.name);
    }

    assert_eq!(names, ["default", "nord", "dracula", "gruvbox", "tokyo-night", "mono", "default"]);
  }

  #[test]
  fn theme_cycle_keeps_color_support() {
    let theme = Theme::new("default", false).next();
    assert_eq!(theme.name, "nord");
    assert!(matches!(theme.border, Color::Indexed(_)));
  }

  #[test]
  fn unknown_theme_falls_back_to_default() {
    assert_eq!(by_name("nope").name, "default");
    assert_eq!(by_name("").name, "default");
    assert_eq!(by_name("nope").next().name, "nord");
  }

  #[test]
  fn builtin_themes_resolve_by_name() {
    for theme in THEMES {
      assert_eq!(by_name(theme.name), theme);
    }
  }
}
