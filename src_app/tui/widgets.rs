//! Custom widgets: history graphs and horizontal meters.

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{RenderDirection, Sparkline, SparklineBar, Widget};

use super::theme::Theme;
use crate::config::ViewType;

/// Braille dot bits of the left column, filled bottom-up (index = number of dots).
const LEFT_DOTS: [u32; 5] = [0x00, 0x40, 0x44, 0x46, 0x47];
/// Braille dot bits of the right column, filled bottom-up (index = number of dots).
const RIGHT_DOTS: [u32; 5] = [0x00, 0x80, 0xa0, 0xb0, 0xb8];
const BRAILLE_BLANK: u32 = 0x2800;
const DOTS_PER_ROW: u64 = 4;

fn bar_set() -> symbols::bar::Set<'static> {
  match std::env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => symbols::bar::THREE_LEVELS,
    _ => symbols::bar::NINE_LEVELS,
  }
}

fn clamp_ratio(ratio: f64) -> f64 {
  if ratio.is_nan() { 0.0 } else { ratio.clamp(0.0, 1.0) }
}

/// History graph for newest-first samples, right-aligned (newest sample on the right).
///
/// `ViewType::Braille` draws a filled braille area graph: 2 samples per cell, 4 dot levels per
/// row, each row colored by its height on the theme gradient (capped by the cell's own value so
/// one-row graphs still reflect the load). `ViewType::Block` falls back to ratatui's block
/// `Sparkline` with every bar colored by its value.
pub struct Graph<'a> {
  view: ViewType,
  data: &'a [u64],
  max: Option<u64>,
  theme: &'a Theme,
  label: Option<Line<'a>>,
}

/// Creates a history graph in the style selected by `view` (`v` key).
pub fn graph<'a>(view: ViewType, data: &'a [u64], theme: &'a Theme) -> Graph<'a> {
  Graph { view, data, max: None, theme, label: None }
}

impl<'a> Graph<'a> {
  /// Value drawn at full height. Defaults to the largest visible sample.
  pub fn max(mut self, max: u64) -> Self {
    self.max = Some(max);
    self
  }

  /// Text drawn over the top-left corner of the graph.
  pub fn label(mut self, label: impl Into<Line<'a>>) -> Self {
    self.label = Some(label.into());
    self
  }

  fn scale_max(&self, visible: usize) -> u64 {
    let max =
      self.max.unwrap_or_else(|| self.data.iter().take(visible).copied().max().unwrap_or(0));
    max.max(1)
  }

  fn render_braille(&self, area: Rect, buf: &mut Buffer) {
    let width = area.width as usize;
    let max = self.scale_max(width * 2);
    let rows = area.height as u64;
    let level = |value: Option<u64>| value.map_or(0, |v| dot_level(v, max, rows * DOTS_PER_ROW));

    for cell in 0..width {
      // right dot column holds the newer sample of the pair
      let newer = 2 * (width - 1 - cell);
      let (left, right) = (self.data.get(newer + 1).copied(), self.data.get(newer).copied());
      if left.is_none() && right.is_none() {
        continue;
      }

      let (left_dots, right_dots) = (level(left), level(right));
      let peak = left.unwrap_or(0).max(right.unwrap_or(0)) as f64 / max as f64;
      let x = area.x + cell as u16;

      for row in 0..rows {
        let base = row * DOTS_PER_ROW;
        let fill = |dots: u64| dots.saturating_sub(base).min(DOTS_PER_ROW) as usize;
        let bits = LEFT_DOTS[fill(left_dots)] | RIGHT_DOTS[fill(right_dots)];
        if bits == 0 {
          break;
        }

        let row_top = (row + 1) as f64 / rows as f64;
        let symbol = char::from_u32(BRAILLE_BLANK + bits).unwrap_or(' ');
        let y = area.bottom() - 1 - row as u16;
        buf[(x, y)].set_char(symbol).set_fg(self.theme.gradient(row_top.min(peak)));
      }
    }
  }

  fn render_blocks(&self, area: Rect, buf: &mut Buffer) {
    let max = self.scale_max(area.width as usize);
    let bars = self.data.iter().take(area.width as usize).map(|&value| {
      let color = self.theme.gradient(value as f64 / max as f64);
      SparklineBar::from(value).style(Style::new().fg(color))
    });

    Sparkline::default()
      .direction(RenderDirection::RightToLeft)
      .data(bars)
      .max(max)
      .bar_set(bar_set())
      .render(area, buf);
  }
}

impl Widget for Graph<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
      return;
    }

    match self.view {
      ViewType::Braille => self.render_braille(area, buf),
      ViewType::Block => self.render_blocks(area, buf),
    }

    if let Some(mut label) = self.label {
      label.style = Style::new().fg(self.theme.text).patch(label.style);
      buf.set_line(area.x, area.y, &label, area.width);
    }
  }
}

/// Number of filled dots (of `dots`) for `value`. Non-zero values get at least one dot.
fn dot_level(value: u64, max: u64, dots: u64) -> u64 {
  if value == 0 || dots == 0 {
    return 0;
  }

  let level = (value as f64 / max.max(1) as f64 * dots as f64).round() as u64;
  level.clamp(1, dots)
}

/// Horizontal meter: `label ▰▰▰▱▱ 42%` with the fill colored by the load gradient.
pub struct Meter<'a> {
  label: Span<'a>,
  ratio: f64,
  theme: &'a Theme,
  symbols: (&'static str, &'static str),
}

impl<'a> Meter<'a> {
  pub fn new(label: impl Into<Cow<'a, str>>, ratio: f64, theme: &'a Theme) -> Self {
    Self { label: Span::raw(label), ratio, theme, symbols: ("▰", "▱") }
  }

  /// Uses block characters (`█` / `░`) instead of `▰` / `▱`.
  pub fn block_chars(mut self, on: bool) -> Self {
    self.symbols = if on { ("█", "░") } else { ("▰", "▱") };
    self
  }
}

/// Widths of the meter label and bar for `width` cells. The value text is right-aligned and
/// keeps priority: the bar is dropped first, then the label.
fn meter_layout(width: u16, label: u16, value: u16) -> (u16, u16) {
  let lead = if label > 0 { label.saturating_add(1) } else { 0 };
  let text = lead.saturating_add(value);
  if width > text.saturating_add(1) {
    (label, width - text - 1)
  } else if width >= text {
    (label, 0)
  } else {
    (0, 0)
  }
}

impl Widget for Meter<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
      return;
    }

    let ratio = clamp_ratio(self.ratio);
    let value = format!("{:>3.0}%", ratio * 100.0);
    let label_width = self.label.width().min(u16::MAX as usize) as u16;
    let (label_width, bar_width) = meter_layout(area.width, label_width, value.len() as u16);
    let (x, y) = (area.x, area.y);

    if label_width > 0 {
      buf.set_stringn(x, y, &self.label.content, label_width as usize, self.theme.text);
    }

    if bar_width > 0 {
      // the bar ends one cell before the right-aligned value
      let bar_x = area.right() - value.len() as u16 - 1 - bar_width;
      let filled = (ratio * bar_width as f64).round() as u16;
      let (on, off) = self.symbols;
      for i in 0..bar_width {
        let (symbol, color) =
          if i < filled { (on, self.theme.gradient(ratio)) } else { (off, self.theme.dim) };
        buf[(bar_x + i, y)].set_symbol(symbol).set_fg(color);
      }
    }

    // narrow areas drop the padding of the value text
    let value = if value.len() > area.width as usize { value.trim_start() } else { value.as_str() };
    if value.len() <= area.width as usize {
      buf.set_string(area.right() - value.len() as u16, y, value, self.theme.text);
    }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Color;
  use ratatui::text::Span;
  use ratatui::widgets::Widget;

  use super::{Meter, dot_level, graph, meter_layout};
  use crate::config::ViewType;
  use crate::tui::theme::Theme;

  fn draw(widget: impl Widget, width: u16, height: u16) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    widget.render(buf.area, &mut buf);
    buf
  }

  fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  fn braille(data: &[u64], max: u64, width: u16, height: u16) -> Buffer {
    let theme = Theme::default();
    draw(graph(ViewType::Braille, data, &theme).max(max), width, height)
  }

  fn meter(label: &str, ratio: f64, width: u16) -> Buffer {
    let theme = Theme::default();
    draw(Meter::new(label, ratio, &theme), width, 1)
  }

  fn count(buf: &Buffer, symbol: &str) -> usize {
    buf.content.iter().filter(|cell| cell.symbol() == symbol).count()
  }

  #[test]
  fn braille_empty_data_renders_nothing() {
    let buf = braille(&[], 100, 4, 2);
    assert_eq!(row(&buf, 0), "    ");
    assert_eq!(row(&buf, 1), "    ");

    // all-zero samples draw no dots either
    let buf = braille(&[0; 8], 100, 4, 2);
    assert_eq!(row(&buf, 1), "    ");
  }

  #[test]
  fn braille_full_data_fills_every_cell() {
    let buf = braille(&[100; 8], 100, 4, 2);
    assert_eq!(row(&buf, 0), "⣿⣿⣿⣿");
    assert_eq!(row(&buf, 1), "⣿⣿⣿⣿");
  }

  #[test]
  fn braille_rows_follow_vertical_gradient() {
    let theme = Theme::default();
    let buf = draw(graph(ViewType::Braille, &[100; 8], &theme).max(100), 4, 4);
    for (y, t) in [(0, 1.0), (1, 0.75), (2, 0.5), (3, 0.25)] {
      assert_eq!(buf[(0, y)].fg, theme.gradient(t), "row {y}");
    }

    // a one-row graph is colored by the value, not by the (full) row height
    let buf = draw(graph(ViewType::Braille, &[20, 20], &theme).max(100), 1, 1);
    assert_eq!(buf[(0, 0)].fg, theme.gradient(0.2));
  }

  #[test]
  fn braille_half_height() {
    let buf = braille(&[50; 4], 100, 2, 2);
    assert_eq!(row(&buf, 0), "  ");
    assert_eq!(row(&buf, 1), "⣿⣿");

    // single row: 2 of 4 dots in both columns
    let buf = braille(&[50; 2], 100, 1, 1);
    assert_eq!(row(&buf, 0), "⣤");
  }

  #[test]
  fn braille_odd_sample_count_is_right_aligned() {
    let buf = braille(&[100; 3], 100, 3, 1);
    assert_eq!(row(&buf, 0), " ⢸⣿");
  }

  #[test]
  fn braille_newest_sample_is_rightmost() {
    assert_eq!(row(&braille(&[100, 0], 100, 1, 1), 0), "⢸");
    assert_eq!(row(&braille(&[0, 100], 100, 1, 1), 0), "⡇");
  }

  #[test]
  fn braille_small_values_show_one_dot() {
    assert_eq!(row(&braille(&[1, 1], 100, 1, 1), 0), "⣀");
    assert_eq!(dot_level(0, 100, 8), 0);
    assert_eq!(dot_level(1, 100, 8), 1);
    assert_eq!(dot_level(50, 100, 8), 4);
    assert_eq!(dot_level(500, 100, 8), 8); // clamped to the area
    assert_eq!(dot_level(5, 0, 8), 8); // zero max doesn't divide by zero
  }

  #[test]
  fn braille_scales_to_visible_samples() {
    let theme = Theme::default();
    // only the 2 newest samples fit in one cell, older peaks don't affect the scale
    let buf = draw(graph(ViewType::Braille, &[10, 10, 100, 100], &theme), 1, 1);
    assert_eq!(row(&buf, 0), "⣿");

    let buf = draw(graph(ViewType::Braille, &[10, 20], &theme), 1, 2);
    assert_eq!(row(&buf, 0), "⡇"); // older 20 is full height, newer 10 is half
    assert_eq!(row(&buf, 1), "⣿");
  }

  #[test]
  fn graph_zero_size_area_does_not_panic() {
    let theme = Theme::default();
    for view in [ViewType::Braille, ViewType::Block] {
      for (w, h) in [(0, 0), (0, 3), (3, 0)] {
        let buf = draw(graph(view, &[100; 8], &theme).label("CPU"), w, h);
        assert!(buf.content.is_empty());
      }
    }
  }

  #[test]
  fn graph_label_overlays_top_left() {
    let theme = Theme::new("nord", true);
    let buf = draw(graph(ViewType::Braille, &[100; 12], &theme).max(100).label("CPU 42%"), 6, 2);
    assert_eq!(row(&buf, 0), "CPU 42");
    assert_eq!(row(&buf, 1), "⣿⣿⣿⣿⣿⣿");
    assert_eq!(buf[(0, 0)].fg, theme.text);

    let label = Span::styled("GPU", theme.dim);
    let buf = draw(graph(ViewType::Braille, &[], &theme).label(label), 6, 1);
    assert_eq!(row(&buf, 0), "GPU   ");
    assert_eq!(buf[(0, 0)].fg, theme.dim);
  }

  #[test]
  fn block_view_renders_sparkline() {
    let theme = Theme::default();
    let buf = draw(graph(ViewType::Block, &[100, 0, 100], &theme).max(100), 4, 1);
    assert_eq!(row(&buf, 0), " █ █");
    assert_eq!(buf[(3, 0)].fg, theme.gradient(1.0));

    let buf = draw(graph(ViewType::Block, &[50, 100], &theme).max(100).label("x"), 3, 2);
    assert_eq!(buf[(2, 1)].symbol(), "█");
    assert_eq!(buf[(2, 1)].fg, theme.gradient(0.5));
    assert_eq!(buf[(0, 0)].symbol(), "x");
  }

  #[test]
  fn meter_fill_at_0_50_100_percent() {
    // "E0 " + 10 cells + " " + value
    let buf = meter("E0", 0.0, 18);
    assert_eq!(row(&buf, 0), "E0 ▱▱▱▱▱▱▱▱▱▱   0%");

    let buf = meter("E0", 0.5, 18);
    assert_eq!(row(&buf, 0), "E0 ▰▰▰▰▰▱▱▱▱▱  50%");

    let buf = meter("E0", 1.0, 18);
    assert_eq!(row(&buf, 0), "E0 ▰▰▰▰▰▰▰▰▰▰ 100%");
  }

  #[test]
  fn meter_colors_fill_by_ratio() {
    let theme = Theme::default();
    let buf = draw(Meter::new("P1", 0.5, &theme), 18, 1);
    assert_eq!(buf[(3, 0)].fg, theme.gradient(0.5));
    assert_eq!(buf[(12, 0)].fg, theme.dim);
    assert_eq!(buf[(0, 0)].fg, theme.text);
    assert_eq!(buf[(17, 0)].fg, theme.text);
  }

  #[test]
  fn meter_narrow_widths() {
    assert_eq!(row(&meter("E0", 0.42, 9), 0), "E0 ▱  42%"); // one bar cell left
    assert_eq!(row(&meter("E0", 0.42, 8), 0), "E0   42%"); // no room for the bar
    assert_eq!(row(&meter("E0", 0.42, 5), 0), "  42%"); // no room for the label
    assert_eq!(row(&meter("E0", 0.42, 3), 0), "42%"); // value without padding
    assert_eq!(row(&meter("E0", 1.0, 3), 0), "   "); // "100%" doesn't fit
    assert!(meter("E0", 0.42, 0).content.is_empty());
  }

  #[test]
  fn meter_layout_priorities() {
    assert_eq!(meter_layout(18, 2, 4), (2, 10));
    assert_eq!(meter_layout(9, 2, 4), (2, 1));
    assert_eq!(meter_layout(8, 2, 4), (2, 0));
    assert_eq!(meter_layout(7, 2, 4), (2, 0));
    assert_eq!(meter_layout(6, 2, 4), (0, 0));
    assert_eq!(meter_layout(10, 0, 4), (0, 5));
    assert_eq!(meter_layout(0, 2, 4), (0, 0));
  }

  #[test]
  fn meter_without_label_and_clamped_ratio() {
    assert_eq!(row(&meter("", 1.5, 10), 0), "▰▰▰▰▰ 100%");
    assert_eq!(row(&meter("", -1.0, 10), 0), "▱▱▱▱▱   0%");
    assert_eq!(row(&meter("", f64::NAN, 10), 0), "▱▱▱▱▱   0%");
  }

  #[test]
  fn meter_block_chars() {
    let theme = Theme::default();
    let buf = draw(Meter::new("E0", 0.5, &theme).block_chars(true), 18, 1);
    assert_eq!(row(&buf, 0), "E0 █████░░░░░  50%");
    assert_eq!(count(&buf, "▰") + count(&buf, "▱"), 0);

    let buf = draw(Meter::new("E0", 0.5, &theme).block_chars(false), 18, 1);
    assert_eq!(count(&buf, "▰"), 5);
  }

  #[test]
  fn meter_mono_theme_has_no_rgb() {
    let theme = Theme::new("mono", false);
    let buf = draw(Meter::new("E0", 0.7, &theme), 18, 1);
    assert!(buf.content.iter().all(|cell| !matches!(cell.fg, Color::Rgb(..))));
  }
}
