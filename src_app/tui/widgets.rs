//! Custom widgets: history graphs and horizontal meters.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use super::theme::Theme;

/// Bar heights in one row: `▁` (1/8) up to `█` (8/8).
const BAR_LEVELS: u64 = 8;
/// Code point before `▁`: the bar of level `n` is `BAR_BASE + n`.
const BAR_BASE: u32 = 0x2580;

fn clamp_ratio(ratio: f64) -> f64 {
  if ratio.is_nan() { 0.0 } else { ratio.clamp(0.0, 1.0) }
}

/// History graph for newest-first samples over the first row of its area, right-aligned (newest
/// sample on the right): one solid bar `▁`…`█` per cell, each colored by its own value on the load
/// gradient, or all in one color. Zero values leave the cell blank.
pub struct Graph<'a> {
  data: &'a [u64],
  max: Option<u64>,
  color: Option<Color>,
  theme: &'a Theme,
}

impl<'a> Graph<'a> {
  pub fn new(data: &'a [u64], theme: &'a Theme) -> Self {
    Self { data, max: None, color: None, theme }
  }

  /// Value drawn at full height. Defaults to the largest visible sample.
  pub fn max(mut self, max: u64) -> Self {
    self.max = Some(max);
    self
  }

  /// Draws every bar in `color` instead of its load color.
  pub fn color(mut self, color: Color) -> Self {
    self.color = Some(color);
    self
  }
}

impl Widget for Graph<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
      return;
    }

    let visible = &self.data[..self.data.len().min(usize::from(area.width))];
    let max = self.max.unwrap_or_else(|| visible.iter().copied().max().unwrap_or(0)).max(1);
    for (x, &value) in (area.left()..area.right()).rev().zip(visible) {
      let level = bar_level(value, max);
      if level == 0 {
        continue;
      }

      let symbol = char::from_u32(BAR_BASE + level as u32).unwrap_or(' ');
      let color = self.color.unwrap_or_else(|| self.theme.gradient(value as f64 / max as f64));
      buf[(x, area.y)].set_char(symbol).set_fg(color);
    }
  }
}

/// Bar height of `value` in eighths of a row, rounded up: non-zero values get at least `▁`.
fn bar_level(value: u64, max: u64) -> u64 {
  if value == 0 {
    return 0;
  }

  // in u128, so large values don't overflow
  let level = (u128::from(value) * u128::from(BAR_LEVELS)).div_ceil(u128::from(max.max(1)));
  level.min(u128::from(BAR_LEVELS)) as u64
}

/// Horizontal meter bar `▰▰▰▱▱` over the first row of its area, the fill colored by the load
/// gradient.
pub struct Meter<'a> {
  ratio: f64,
  theme: &'a Theme,
}

impl<'a> Meter<'a> {
  pub fn new(ratio: f64, theme: &'a Theme) -> Self {
    Self { ratio, theme }
  }
}

impl Widget for Meter<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
      return;
    }

    let ratio = clamp_ratio(self.ratio);
    let filled = (ratio * f64::from(area.width)).round() as u16;
    for i in 0..area.width {
      let (symbol, color) =
        if i < filled { ("▰", self.theme.gradient(ratio)) } else { ("▱", self.theme.dim) };
      buf[(area.x + i, area.y)].set_symbol(symbol).set_fg(color);
    }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Color;
  use ratatui::widgets::Widget;

  use super::{Graph, Meter, bar_level};
  use crate::tui::palette::Palette;
  use crate::tui::theme::Theme;

  /// Theme with a smooth gradient, so every load level has its own color.
  fn smooth() -> Theme {
    let palette = Palette { green: (0, 255, 0), yellow: (255, 255, 0), red: (255, 0, 0) };
    Theme::new(Some(palette), true)
  }

  fn draw(widget: impl Widget, width: u16, height: u16) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    widget.render(buf.area, &mut buf);
    buf
  }

  fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// One-row graph of `data` (newest first) scaled to `max`.
  fn bars(data: &[u64], max: u64, width: u16) -> String {
    let theme = Theme::default();
    row(&draw(Graph::new(data, &theme).max(max), width, 1), 0)
  }

  fn meter_rows(ratio: f64, width: u16, height: u16) -> Buffer {
    let theme = Theme::default();
    draw(Meter::new(ratio, &theme), width, height)
  }

  fn meter(ratio: f64, width: u16) -> Buffer {
    meter_rows(ratio, width, 1)
  }

  #[test]
  fn graph_empty_data_renders_nothing() {
    assert_eq!(bars(&[], 100, 4), "    ");
    // zero samples are blank too
    assert_eq!(bars(&[0; 8], 100, 4), "    ");
  }

  #[test]
  fn graph_bar_levels() {
    // zero blank, a tiny value one eighth, half, full
    assert_eq!(bars(&[100, 50, 1, 0], 100, 4), " ▁▄█");
    assert_eq!(bars(&[800, 700, 600, 500, 400, 300, 200, 100], 800, 8), "▁▂▃▄▅▆▇█");

    // rounded up to the next eighth, clamped to the row
    assert_eq!(bar_level(0, 100), 0);
    assert_eq!(bar_level(1, 100), 1);
    assert_eq!(bar_level(12, 100), 1);
    assert_eq!(bar_level(13, 100), 2);
    assert_eq!(bar_level(50, 100), 4);
    assert_eq!(bar_level(51, 100), 5);
    assert_eq!(bar_level(100, 100), 8);
    assert_eq!(bar_level(500, 100), 8);
    assert_eq!(bar_level(5, 0), 8); // zero max doesn't divide by zero
    assert_eq!(bar_level(u64::MAX, u64::MAX), 8);
  }

  #[test]
  fn graph_is_right_aligned_one_sample_per_cell() {
    assert_eq!(bars(&[100; 3], 100, 5), "  ███");
    // newest sample is rightmost
    assert_eq!(bars(&[100, 50], 100, 2), "▄█");
    assert_eq!(bars(&[50, 100], 100, 2), "█▄");
    // only the newest samples that fit
    assert_eq!(bars(&[100, 50, 0, 100, 100], 100, 2), "▄█");
  }

  #[test]
  fn graph_scales_to_visible_samples() {
    let theme = Theme::default();
    // the older 100 doesn't fit, so 20 is the full height
    let buf = draw(Graph::new(&[10, 20, 100], &theme), 2, 1);
    assert_eq!(row(&buf, 0), "█▄");
  }

  #[test]
  fn graph_colors_each_bar_by_its_value() {
    for theme in [smooth(), Theme::default()] {
      let buf = draw(Graph::new(&[90, 50, 10], &theme).max(100), 3, 1);
      let colors: Vec<Color> = (0..3).map(|x| buf[(x, 0)].fg).collect();
      assert_eq!(colors, [0.1, 0.5, 0.9].map(|t| theme.gradient(t)));
    }

    // the terminal's green / yellow / red without a palette, no RGB
    let theme = Theme::default();
    let buf = draw(Graph::new(&[90, 50, 10], &theme).max(100), 3, 1);
    let colors: Vec<Color> = (0..3).map(|x| buf[(x, 0)].fg).collect();
    assert_eq!(colors, [Color::Green, Color::Yellow, Color::Red]);
  }

  #[test]
  fn graph_in_one_color() {
    let theme = smooth();
    let low = theme.gradient(0.0);
    let buf = draw(Graph::new(&[4000, 2000, 0, 300], &theme).color(low), 4, 1);
    assert_eq!(row(&buf, 0), "▁ ▄█");
    for x in [0, 2, 3] {
      assert_eq!(buf[(x, 0)].fg, low, "x {x}");
    }
  }

  #[test]
  fn graph_draws_first_row_only() {
    let theme = Theme::default();
    let buf = draw(Graph::new(&[100, 50], &theme).max(100), 2, 2);
    assert_eq!((row(&buf, 0).as_str(), row(&buf, 1).as_str()), ("▄█", "  "));
  }

  #[test]
  fn graph_zero_size_area_does_not_panic() {
    let theme = Theme::default();
    for (w, h) in [(0, 0), (0, 3), (3, 0)] {
      let buf = draw(Graph::new(&[100; 8], &theme), w, h);
      assert!(buf.content.is_empty());
    }
  }

  #[test]
  fn meter_fill_at_0_50_100_percent() {
    assert_eq!(row(&meter(0.0, 10), 0), "▱▱▱▱▱▱▱▱▱▱");
    assert_eq!(row(&meter(0.5, 10), 0), "▰▰▰▰▰▱▱▱▱▱");
    assert_eq!(row(&meter(1.0, 10), 0), "▰▰▰▰▰▰▰▰▰▰");
    // rounded to the nearest cell
    assert_eq!(row(&meter(0.42, 10), 0), "▰▰▰▰▱▱▱▱▱▱");
  }

  #[test]
  fn meter_colors_fill_by_ratio() {
    for theme in [smooth(), Theme::default()] {
      let buf = draw(Meter::new(0.5, &theme), 10, 1);
      assert_eq!(buf[(0, 0)].fg, theme.gradient(0.5));
      assert_eq!(buf[(4, 0)].fg, theme.gradient(0.5));
      assert_eq!(buf[(5, 0)].fg, theme.dim);
    }

    let theme = Theme::default();
    let buf = draw(Meter::new(0.7, &theme), 10, 1);
    assert_eq!((buf[(0, 0)].fg, buf[(9, 0)].fg), (Color::Red, Color::DarkGray));
  }

  #[test]
  fn meter_narrow_widths() {
    assert_eq!(row(&meter(0.42, 2), 0), "▰▱");
    assert_eq!(row(&meter(0.2, 1), 0), "▱");
    assert_eq!(row(&meter(0.6, 1), 0), "▰");
    assert!(meter(0.42, 0).content.is_empty());

    // only the first row is drawn
    let buf = meter_rows(0.5, 4, 2);
    assert_eq!((row(&buf, 0).as_str(), row(&buf, 1).as_str()), ("▰▰▱▱", "    "));
  }

  #[test]
  fn meter_clamps_ratio() {
    assert_eq!(row(&meter(1.5, 5), 0), "▰▰▰▰▰");
    assert_eq!(row(&meter(-1.0, 5), 0), "▱▱▱▱▱");
    assert_eq!(row(&meter(f64::NAN, 5), 0), "▱▱▱▱▱");
  }
}
