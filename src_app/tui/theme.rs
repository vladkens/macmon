//! Colors from the terminal's own palette (the default foreground and the 16 ANSI colors), so
//! macmon follows the terminal theme, and the bar glyphs the terminal draws without gaps.

use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::palette::{Palette, Rgb};

/// Borders: bright black (ANSI 8).
pub const BORDER: Color = Color::DarkGray;
/// Secondary text (separators, units, zeros): bright black (ANSI 8).
pub const DIM: Color = Color::DarkGray;
/// Titles and text: the terminal's default foreground.
pub const TEXT: Color = Color::Reset;
/// Selected process row: reverse video over the default colors, so the row reads as one bar
/// instead of reversing each load color in it.
pub const SELECTED: Style = Style::new().fg(TEXT).add_modifier(Modifier::REVERSED);

/// Steps of the gradient without a smooth palette: green up to `GREEN_MAX`, yellow up to
/// `YELLOW_MAX`, red above.
const GREEN_MAX: f64 = 1.0 / 3.0;
const YELLOW_MAX: f64 = 2.0 / 3.0;

/// Box or metric name: bold, in the default color.
pub fn heading<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, Style::new().fg(TEXT).add_modifier(Modifier::BOLD))
}

/// Text in the default color. The color is set, not left out, so text drawn over a border doesn't
/// take the border's color.
pub fn text<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, TEXT)
}

pub fn dim<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, DIM)
}

/// What depends on the terminal: the load gradient and the bar glyphs. The UI colors above are
/// the same everywhere.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Theme {
  /// RGB gradient stops (green, yellow, red) from the palette query; `None` steps through the
  /// ANSI colors instead.
  smooth: Option<[Rgb; 3]>,
  /// Graph bars in three levels (blank, `▄`, `█`) instead of eighths: Apple Terminal draws gaps
  /// between the eighth blocks.
  pub three_level_bars: bool,
}

impl Theme {
  /// Theme for a terminal that answered the palette query with `palette` (asked only on truecolor
  /// terminals); without one the gradient steps through the ANSI colors.
  pub fn new(palette: Option<Palette>) -> Self {
    Self { smooth: palette.map(|p| [p.green, p.yellow, p.red]), three_level_bars: false }
  }

  pub fn with_three_level_bars(self, three_level_bars: bool) -> Self {
    Self { three_level_bars, ..self }
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

/// True in Apple Terminal, which needs three-level bars (`TERM_PROGRAM`).
pub fn detect_three_level_bars() -> bool {
  is_apple_terminal(std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn is_apple_terminal(term_program: Option<&str>) -> bool {
  term_program == Some("Apple_Terminal")
}

#[cfg(test)]
mod tests {
  use ratatui::style::Color;

  use super::{Theme, is_apple_terminal, supports_truecolor};
  use crate::tui::palette::Palette;

  /// Solarized-like terminal colors.
  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  #[test]
  fn smooth_gradient_blends_terminal_colors() {
    let theme = Theme::new(Some(PALETTE));
    assert_eq!(theme.gradient(0.0), Color::Rgb(0x85, 0x99, 0x00));
    assert_eq!(theme.gradient(0.5), Color::Rgb(0xb5, 0x89, 0x00));
    assert_eq!(theme.gradient(1.0), Color::Rgb(0xdc, 0x32, 0x2f));
    // halfway between green and yellow, between yellow and red
    assert_eq!(theme.gradient(0.25), Color::Rgb(0x9d, 0x91, 0x00));
    assert_eq!(theme.gradient(0.75), Color::Rgb(0xc9, 0x5e, 0x18));
  }

  #[test]
  fn discrete_gradient_without_palette() {
    for theme in [Theme::new(None), Theme::default()] {
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
    for theme in [Theme::default(), Theme::new(Some(PALETTE))] {
      assert_eq!(theme.gradient(-1.0), theme.gradient(0.0));
      assert_eq!(theme.gradient(2.0), theme.gradient(1.0));
      assert_eq!(theme.gradient(f64::NAN), theme.gradient(0.0));
    }
  }

  #[test]
  fn three_level_bars_keep_the_gradient() {
    let theme = Theme::new(Some(PALETTE)).with_three_level_bars(true);
    assert!(theme.three_level_bars && !Theme::new(Some(PALETTE)).three_level_bars);
    assert_eq!(theme.gradient(0.25), Theme::new(Some(PALETTE)).gradient(0.25));
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
  fn detects_apple_terminal_from_term_program() {
    assert!(is_apple_terminal(Some("Apple_Terminal")));
    for other in [Some("iTerm.app"), Some("ghostty"), Some("tmux"), Some(""), None] {
      assert!(!is_apple_terminal(other), "{other:?}");
    }
  }
}
