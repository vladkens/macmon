//! Custom widgets: history graphs, horizontal meters and per-core bars.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols;
use ratatui::widgets::{RenderDirection, Sparkline, SparklineBar, Widget};

use super::theme::Theme;
use crate::config::ViewType;

/// Braille dot bits of the left column, filled bottom-up (index = number of dots).
const LEFT_DOTS: [u32; 5] = [0x00, 0x40, 0x44, 0x46, 0x47];
/// Braille dot bits of the right column, filled bottom-up (index = number of dots).
const RIGHT_DOTS: [u32; 5] = [0x00, 0x80, 0xa0, 0xb0, 0xb8];
const BRAILLE_BLANK: u32 = 0x2800;
const DOTS_PER_ROW: u64 = 4;
/// Vertical bars of the per-core row, lowest first; an idle core still shows the lowest one.
pub const CORE_BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

fn bar_set() -> symbols::bar::Set<'static> {
  match std::env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => symbols::bar::THREE_LEVELS,
    _ => symbols::bar::NINE_LEVELS,
  }
}

fn clamp_ratio(ratio: f64) -> f64 {
  if ratio.is_nan() { 0.0 } else { ratio.clamp(0.0, 1.0) }
}

/// One-cell vertical bar for a core load `ratio` (`▁` idle … `█` busy).
pub fn core_bar(ratio: f64) -> &'static str {
  let level = (clamp_ratio(ratio) * (CORE_BARS.len() - 1) as f64).round() as usize;
  CORE_BARS[level.min(CORE_BARS.len() - 1)]
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
}

/// Creates a history graph in the style selected by `view` (`v` key).
pub fn graph<'a>(view: ViewType, data: &'a [u64], theme: &'a Theme) -> Graph<'a> {
  Graph { view, data, max: None, theme }
}

impl Graph<'_> {
  /// Value drawn at full height. Defaults to the largest visible sample.
  pub fn max(mut self, max: u64) -> Self {
    self.max = Some(max);
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

/// Horizontal meter bar `▰▰▰▱▱` over the first row of its area, the fill colored by the load
/// gradient.
pub struct Meter<'a> {
  ratio: f64,
  theme: &'a Theme,
  symbols: (&'static str, &'static str),
}

impl<'a> Meter<'a> {
  pub fn new(ratio: f64, theme: &'a Theme) -> Self {
    Self { ratio, theme, symbols: ("▰", "▱") }
  }

  /// Uses block characters (`█` / `░`) instead of `▰` / `▱`.
  pub fn block_chars(mut self, on: bool) -> Self {
    self.symbols = if on { ("█", "░") } else { ("▰", "▱") };
    self
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
    let (on, off) = self.symbols;
    for i in 0..area.width {
      let (symbol, color) =
        if i < filled { (on, self.theme.gradient(ratio)) } else { (off, self.theme.dim) };
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

  use super::{Meter, core_bar, dot_level, graph};
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

  fn meter_rows(ratio: f64, width: u16, height: u16) -> Buffer {
    let theme = Theme::default();
    draw(Meter::new(ratio, &theme), width, height)
  }

  fn meter(ratio: f64, width: u16) -> Buffer {
    meter_rows(ratio, width, 1)
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
        let buf = draw(graph(view, &[100; 8], &theme), w, h);
        assert!(buf.content.is_empty());
      }
    }
  }

  #[test]
  fn block_view_renders_sparkline() {
    let theme = Theme::default();
    let buf = draw(graph(ViewType::Block, &[100, 0, 100], &theme).max(100), 4, 1);
    assert_eq!(row(&buf, 0), " █ █");
    assert_eq!(buf[(3, 0)].fg, theme.gradient(1.0));

    let buf = draw(graph(ViewType::Block, &[50, 100], &theme).max(100), 3, 2);
    assert_eq!(buf[(2, 1)].symbol(), "█");
    assert_eq!(buf[(2, 1)].fg, theme.gradient(0.5));
    assert_eq!(buf[(2, 0)].symbol(), " ");
  }

  #[test]
  fn core_bars_by_load() {
    assert_eq!(core_bar(0.0), "▁", "idle cores stay visible");
    assert_eq!(core_bar(0.5), "▅");
    assert_eq!(core_bar(1.0), "█");
    assert_eq!(core_bar(0.07), "▁");
    assert_eq!(core_bar(0.08), "▂");
    // out of range values are clamped
    assert_eq!(core_bar(-1.0), "▁");
    assert_eq!(core_bar(f64::NAN), "▁");
    assert_eq!(core_bar(2.0), "█");
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
    let theme = Theme::default();
    let buf = draw(Meter::new(0.5, &theme), 10, 1);
    assert_eq!(buf[(0, 0)].fg, theme.gradient(0.5));
    assert_eq!(buf[(4, 0)].fg, theme.gradient(0.5));
    assert_eq!(buf[(5, 0)].fg, theme.dim);
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

  #[test]
  fn meter_block_chars() {
    let theme = Theme::default();
    let buf = draw(Meter::new(0.5, &theme).block_chars(true), 10, 1);
    assert_eq!(row(&buf, 0), "█████░░░░░");
    assert_eq!(count(&buf, "▰") + count(&buf, "▱"), 0);

    let buf = draw(Meter::new(0.5, &theme).block_chars(false), 10, 1);
    assert_eq!(count(&buf, "▰"), 5);
  }

  #[test]
  fn meter_mono_theme_has_no_rgb() {
    let theme = Theme::new("mono", false);
    let buf = draw(Meter::new(0.7, &theme), 10, 1);
    assert!(buf.content.iter().all(|cell| !matches!(cell.fg, Color::Rgb(..))));
  }
}
