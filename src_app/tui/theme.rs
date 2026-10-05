//! Colors from the terminal's own palette: default foreground / background and the 16 ANSI
//! colors, so macmon follows the terminal theme.

use ratatui::style::{Color, Modifier, Style};

use super::palette::{Palette, Rgb};

/// Steps of the gradient without a smooth palette: green up to `GREEN_MAX`, yellow up to
/// `YELLOW_MAX`, red above.
const GREEN_MAX: f64 = 1.0 / 3.0;
const YELLOW_MAX: f64 = 2.0 / 3.0;

/// UI colors. Everything is a terminal color (`Color::Reset` or an ANSI index), except the load
/// gradient on truecolor terminals with a known palette: it blends the terminal's own green,
/// yellow and red in RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
  pub border: Color,
  pub title: Color,
  pub text: Color,
  pub dim: Color,
  /// Selected process row: reverse video over the default colors.
  pub selected: Style,
  /// RGB gradient stops (green, yellow, red); `None` steps through the ANSI colors instead.
  smooth: Option<[Rgb; 3]>,
}

impl Default for Theme {
  fn default() -> Self {
    Self::new(None, false)
  }
}

impl Theme {
  /// Theme for a terminal with the given palette (`None` when the query went unanswered) and
  /// color support. The gradient is smooth only with both.
  pub fn new(palette: Option<Palette>, truecolor: bool) -> Self {
    Self {
      border: Color::DarkGray,
      title: Color::Reset,
      text: Color::Reset,
      dim: Color::DarkGray,
      selected: Style::new().fg(Color::Reset).add_modifier(Modifier::REVERSED),
      smooth: palette.filter(|_| truecolor).map(|p| [p.green, p.yellow, p.red]),
    }
  }

  /// Load color for `t` in `0.0..=1.0` (green → yellow → red), clamped outside the range.
  pub fn gradient(&self, t: f64) -> Color {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    match self.smooth {
      Some([low, mid, _]) if t <= 0.5 => lerp(low, mid, t * 2.0),
      Some([_, mid, high]) => lerp(mid, high, (t - 0.5) * 2.0),
      None if t <= GREEN_MAX => Color::Green,
      None if t <= YELLOW_MAX => Color::Yellow,
      None => Color::Red,
    }
  }
}

fn lerp((r1, g1, b1): Rgb, (r2, g2, b2): Rgb, t: f64) -> Color {
  let mix = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
  Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// True when `COLORTERM` advertises 24-bit color support.
pub fn detect_truecolor() -> bool {
  supports_truecolor(std::env::var("COLORTERM").ok().as_deref())
}

fn supports_truecolor(colorterm: Option<&str>) -> bool {
  matches!(colorterm, Some("truecolor" | "24bit"))
}

#[cfg(test)]
mod tests {
  use ratatui::style::{Color, Modifier};

  use super::{Theme, supports_truecolor};
  use crate::tui::palette::Palette;

  /// Solarized-like terminal colors.
  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  #[test]
  fn ui_colors_come_from_the_terminal() {
    for theme in [Theme::default(), Theme::new(Some(PALETTE), true)] {
      assert_eq!((theme.border, theme.dim), (Color::DarkGray, Color::DarkGray));
      assert_eq!((theme.title, theme.text), (Color::Reset, Color::Reset));
      assert_eq!(theme.selected.fg, Some(Color::Reset));
      assert!(theme.selected.add_modifier.contains(Modifier::REVERSED));
      assert_eq!(theme.selected.bg, None);
    }
  }

  #[test]
  fn smooth_gradient_blends_terminal_colors() {
    let theme = Theme::new(Some(PALETTE), true);
    assert_eq!(theme.gradient(0.0), Color::Rgb(0x85, 0x99, 0x00));
    assert_eq!(theme.gradient(0.5), Color::Rgb(0xb5, 0x89, 0x00));
    assert_eq!(theme.gradient(1.0), Color::Rgb(0xdc, 0x32, 0x2f));
    // halfway between green and yellow, between yellow and red
    assert_eq!(theme.gradient(0.25), Color::Rgb(0x9d, 0x91, 0x00));
    assert_eq!(theme.gradient(0.75), Color::Rgb(0xc9, 0x5e, 0x18));
  }

  #[test]
  fn discrete_gradient_without_palette_or_truecolor() {
    // a known palette needs truecolor, truecolor needs a known palette
    for theme in [Theme::new(None, true), Theme::new(Some(PALETTE), false), Theme::default()] {
      let steps = [
        (0.0, Color::Green),
        (1.0 / 3.0, Color::Green),
        (0.34, Color::Yellow),
        (0.5, Color::Yellow),
        (2.0 / 3.0, Color::Yellow),
        (0.67, Color::Red),
        (1.0, Color::Red),
      ];
      for (t, color) in steps {
        assert_eq!(theme.gradient(t), color, "{t}");
      }
    }
  }

  #[test]
  fn gradient_clamps_out_of_range() {
    for theme in [Theme::default(), Theme::new(Some(PALETTE), true)] {
      assert_eq!(theme.gradient(-1.0), theme.gradient(0.0));
      assert_eq!(theme.gradient(2.0), theme.gradient(1.0));
      assert_eq!(theme.gradient(f64::NAN), theme.gradient(0.0));
    }
  }

  #[test]
  fn detects_truecolor_from_colorterm() {
    assert!(supports_truecolor(Some("truecolor")));
    assert!(supports_truecolor(Some("24bit")));
    assert!(!supports_truecolor(Some("256color")));
    assert!(!supports_truecolor(Some("")));
    assert!(!supports_truecolor(None));
  }
}
