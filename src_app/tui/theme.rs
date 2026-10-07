//! Colors from the terminal's own palette (the default foreground and the 16 ANSI colors), so
//! macmon follows the terminal theme, and the bar glyphs the terminal draws without gaps.

use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Borders: bright black (ANSI 8).
pub(super) const BORDER: Color = Color::DarkGray;
/// Secondary text (separators, units, zeros): bright black (ANSI 8).
pub(super) const DIM: Color = Color::DarkGray;
/// Titles and text: the terminal's default foreground.
pub(super) const TEXT: Color = Color::Reset;
/// Selected process row: reverse video over the default colors, so the row reads as one bar
/// instead of reversing each load color in it.
pub(super) const SELECTED: Style = Style::new().fg(TEXT).add_modifier(Modifier::REVERSED);

/// Steps of the load gradient: green up to `GREEN_MAX`, yellow up to `YELLOW_MAX`, red above.
const GREEN_MAX: f64 = 1.0 / 3.0;
const YELLOW_MAX: f64 = 2.0 / 3.0;

/// Box or metric name: bold, in the default color.
pub(super) fn heading<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, Style::new().fg(TEXT).add_modifier(Modifier::BOLD))
}

/// Text in the default color. The color is set, not left out, so text drawn over a border doesn't
/// take the border's color.
pub(super) fn text<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, TEXT)
}

pub(super) fn dim<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, DIM)
}

/// Load color for `t` in `0.0..=1.0`: the terminal's green, yellow or red, in steps of a third.
/// Values below the range (and NaN) are green, above it red.
pub(super) fn gradient(t: f64) -> Color {
  if t.is_nan() || t <= GREEN_MAX {
    Color::Green
  } else if t <= YELLOW_MAX {
    Color::Yellow
  } else {
    Color::Red
  }
}

/// True in Apple Terminal (`TERM_PROGRAM`), which draws gaps between the eighth blocks, so graph
/// bars come in three levels there (blank, `▄`, `█`).
pub(super) fn detect_three_level_bars() -> bool {
  is_apple_terminal(std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn is_apple_terminal(term_program: Option<&str>) -> bool {
  term_program == Some("Apple_Terminal")
}

#[cfg(test)]
mod tests {
  use ratatui::style::Color;

  use super::{gradient, is_apple_terminal};

  #[test]
  fn gradient_steps_through_terminal_colors() {
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
      assert_eq!(gradient(t), color, "{t}");
    }
  }

  #[test]
  fn gradient_clamps_out_of_range() {
    assert_eq!(gradient(-1.0), gradient(0.0));
    assert_eq!(gradient(2.0), gradient(1.0));
    assert_eq!(gradient(f64::NAN), gradient(0.0));
  }

  #[test]
  fn detects_apple_terminal_from_term_program() {
    assert!(is_apple_terminal(Some("Apple_Terminal")));
    for other in [Some("iTerm.app"), Some("ghostty"), Some("tmux"), Some(""), None] {
      assert!(!is_apple_terminal(other), "{other:?}");
    }
  }
}
