//! History graph and gauge widgets.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use super::theme::Theme;

/// Bar heights in one row: `▁` (1/8) up to `█` (8/8).
const BAR_LEVELS: u64 = 8;
/// Code point before `▁`: the bar of level `n` is `BAR_BASE + n`.
const BAR_BASE: u32 = 0x2580;
/// Filled cell of a gauge.
const FULL: char = '█';

/// Gauge view of the original macmon: a bar across its whole area, filled from the left to `ratio`
/// (rounded to whole cells) in the load color of `ratio`; the rest stays blank.
pub struct Gauge<'a> {
  ratio: f64,
  theme: &'a Theme,
}

impl<'a> Gauge<'a> {
  /// Gauge for `ratio` in `0.0..=1.0`, clamped outside the range.
  pub fn new(ratio: f64, theme: &'a Theme) -> Self {
    let ratio = if ratio.is_nan() { 0.0 } else { ratio.clamp(0.0, 1.0) };
    Self { ratio, theme }
  }
}

impl Widget for Gauge<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    // at most `area.width`, as the ratio is at most 1
    let filled = (f64::from(area.width) * self.ratio).round() as u16;
    let color = self.theme.gradient(self.ratio);
    for y in area.top()..area.bottom() {
      for x in area.left()..area.left() + filled {
        buf[(x, y)].set_char(FULL).set_fg(color);
      }
    }
  }
}

/// History graph for newest-first samples over its whole area, right-aligned (newest sample on
/// the right): one solid bar per column, growing from the bottom row up in eighths of a row
/// (`▁`…`█`), each colored by its own value on the load gradient, or all in one color. Zero
/// values leave the column blank.
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

  /// Value drawn at full height; `None` (the default) scales to the largest visible sample.
  pub fn max(mut self, max: Option<u64>) -> Self {
    self.max = max;
    self
  }

  /// Draws every bar in `color` instead of its load color (`None`, the default).
  pub fn color(mut self, color: Option<Color>) -> Self {
    self.color = color;
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
    let levels = u64::from(area.height) * BAR_LEVELS;
    for (x, &value) in (area.left()..area.right()).rev().zip(visible) {
      let mut level = bar_level(value, max, levels);
      let color = self.color.unwrap_or_else(|| self.theme.gradient(value as f64 / max as f64));
      for y in (area.top()..area.bottom()).rev() {
        if level == 0 {
          break;
        }

        let eighths = level.min(BAR_LEVELS);
        let symbol = char::from_u32(BAR_BASE + eighths as u32).unwrap_or(' ');
        buf[(x, y)].set_char(symbol).set_fg(color);
        level -= eighths;
      }
    }
  }
}

/// Bar height of `value` in eighths of a row out of `levels`, rounded up: non-zero values get at
/// least `▁`.
fn bar_level(value: u64, max: u64, levels: u64) -> u64 {
  if value == 0 {
    return 0;
  }

  // in u128, so large values don't overflow
  let level = (u128::from(value) * u128::from(levels)).div_ceil(u128::from(max.max(1)));
  level.min(u128::from(levels)) as u64
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Color;
  use ratatui::widgets::Widget;

  use super::{Gauge, Graph, bar_level};
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

  fn rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height).map(|y| row(buf, y)).collect()
  }

  /// Graph of `data` (newest first) scaled to `max`, `height` rows tall.
  fn graph(data: &[u64], max: u64, width: u16, height: u16) -> Vec<String> {
    let theme = Theme::default();
    rows(&draw(Graph::new(data, &theme).max(Some(max)), width, height))
  }

  /// One-row graph of `data` scaled to `max`.
  fn bars(data: &[u64], max: u64, width: u16) -> String {
    graph(data, max, width, 1).remove(0)
  }

  #[test]
  fn graph_empty_data_renders_nothing() {
    assert_eq!(bars(&[], 100, 4), "    ");
    // zero samples are blank too
    assert_eq!(graph(&[0; 8], 100, 4, 3), ["    "; 3]);
  }

  #[test]
  fn graph_bar_levels() {
    // zero blank, a tiny value one eighth, half, full
    assert_eq!(bars(&[100, 50, 1, 0], 100, 4), " ▁▄█");
    assert_eq!(bars(&[800, 700, 600, 500, 400, 300, 200, 100], 800, 8), "▁▂▃▄▅▆▇█");

    // rounded up to the next eighth, clamped to the area
    assert_eq!(bar_level(0, 100, 8), 0);
    assert_eq!(bar_level(1, 100, 8), 1);
    assert_eq!(bar_level(12, 100, 8), 1);
    assert_eq!(bar_level(13, 100, 8), 2);
    assert_eq!(bar_level(50, 100, 8), 4);
    assert_eq!(bar_level(51, 100, 8), 5);
    assert_eq!(bar_level(100, 100, 8), 8);
    assert_eq!(bar_level(500, 100, 8), 8);
    assert_eq!(bar_level(5, 0, 8), 8); // zero max doesn't divide by zero
    assert_eq!(bar_level(u64::MAX, u64::MAX, u64::MAX), u64::MAX);

    // three rows: 24 eighths
    assert_eq!(bar_level(50, 100, 24), 12);
    assert_eq!(bar_level(1, 100, 24), 1);
    assert_eq!(bar_level(100, 100, 24), 24);
  }

  #[test]
  fn graph_bars_grow_across_rows() {
    // three rows, newest on the right: 100 % full, 50 % one and a half rows, 30 % (7.2 eighths
    // rounded up to 8) one row, a tiny value one eighth at the bottom, zero blank
    let rows = graph(&[100, 50, 30, 1, 0], 100, 5, 3);
    assert_eq!(rows, ["█", "▄█", "▁███"].map(|r| format!("{r:>5}")));

    // every level of a two-row bar, bottom row full before the top row starts
    let levels: Vec<u64> = (1..=16).rev().collect();
    let rows = graph(&levels, 16, 16, 2);
    assert_eq!(rows, ["        ▁▂▃▄▅▆▇█", "▁▂▃▄▅▆▇█████████"]);
  }

  #[test]
  fn graph_is_right_aligned_one_sample_per_column() {
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
    let buf = draw(Graph::new(&[10, 20, 100], &theme), 2, 2);
    assert_eq!(rows(&buf), ["█ ", "██"]);
    // `None` is the same as no maximum
    let buf = draw(Graph::new(&[10, 20, 100], &theme).max(None), 2, 2);
    assert_eq!(rows(&buf), ["█ ", "██"]);
  }

  #[test]
  fn graph_colors_each_column_by_its_value() {
    for theme in [smooth(), Theme::default()] {
      let buf = draw(Graph::new(&[90, 50, 10], &theme).max(Some(100)), 3, 4);
      // every cell of a bar in the bar's color
      for (x, t) in [(0, 0.1), (1, 0.5), (2, 0.9)] {
        for y in 0..4 {
          if buf[(x, y)].symbol() != " " {
            assert_eq!(buf[(x, y)].fg, theme.gradient(t), "{x}, {y}");
          }
        }
      }
      assert_eq!(rows(&buf), ["  ▅", "  █", " ██", "▄██"]);
    }

    // the terminal's green / yellow / red without a palette, no RGB
    let theme = Theme::default();
    let buf = draw(Graph::new(&[90, 50, 10], &theme).max(Some(100)), 3, 1);
    let colors: Vec<Color> = (0..3).map(|x| buf[(x, 0)].fg).collect();
    assert_eq!(colors, [Color::Green, Color::Yellow, Color::Red]);
  }

  #[test]
  fn graph_in_one_color() {
    let theme = smooth();
    let low = theme.gradient(0.0);
    let buf = draw(Graph::new(&[4000, 2000, 0, 200], &theme).color(Some(low)), 4, 2);
    assert_eq!(rows(&buf), ["   █", "▁ ██"]);
    for (x, y) in [(0, 1), (2, 1), (3, 1), (3, 0)] {
      assert_eq!(buf[(x, y)].fg, low, "{x}, {y}");
    }
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
  fn graph_stays_inside_its_area() {
    let theme = Theme::default();
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    Graph::new(&[100; 10], &theme).max(Some(100)).render(Rect::new(2, 1, 3, 2), &mut buf);
    assert_eq!(rows(&buf), ["      ", "  ███ ", "  ███ ", "      "]);
  }

  #[test]
  fn gauge_fills_its_ratio_of_every_row() {
    for theme in [smooth(), Theme::default()] {
      // (ratio, width, height, filled cells per row)
      let cases = [
        (0.0, 10, 3, 0),
        (0.5, 10, 3, 5),
        (1.0, 10, 3, 10),
        (0.0, 48, 7, 0),
        (0.5, 48, 7, 24),
        (1.0, 48, 7, 48),
        (0.5, 1, 1, 1), // half a cell rounds up
        (0.556, 48, 1, 27),
        (0.44, 25, 2, 11),
        (0.04, 10, 1, 0), // less than half a cell stays blank
      ];
      for (ratio, width, height, filled) in cases {
        let buf = draw(Gauge::new(ratio, &theme), width, height);
        let ctx = format!("{ratio} in {width}x{height}");
        let row = format!("{}{}", "█".repeat(filled), " ".repeat(usize::from(width) - filled));
        assert_eq!(rows(&buf), vec![row; usize::from(height)], "{ctx}");

        // filled cells in the load color of the ratio
        for (x, y) in (0..filled as u16).flat_map(|x| (0..height).map(move |y| (x, y))) {
          assert_eq!(buf[(x, y)].fg, theme.gradient(ratio), "{ctx}: {x}, {y}");
        }
      }
    }

    // the terminal's green / yellow / red without a palette
    let theme = Theme::default();
    let colors = [0.0, 0.5, 1.0].map(|ratio| draw(Gauge::new(ratio, &theme), 4, 1)[(0, 0)].fg);
    assert_eq!(colors, [Color::Reset, Color::Yellow, Color::Red], "nothing to color at 0");
    assert_eq!(draw(Gauge::new(0.2, &theme), 4, 1)[(0, 0)].fg, Color::Green);
  }

  #[test]
  fn gauge_clamps_ratio_and_stays_inside_its_area() {
    let theme = Theme::default();
    assert_eq!(rows(&draw(Gauge::new(1.5, &theme), 4, 1)), ["████"]);
    assert_eq!(rows(&draw(Gauge::new(-1.0, &theme), 4, 1)), ["    "]);
    assert_eq!(rows(&draw(Gauge::new(f64::NAN, &theme), 4, 1)), ["    "]);

    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    Gauge::new(1.0, &theme).render(Rect::new(2, 1, 3, 2), &mut buf);
    assert_eq!(rows(&buf), ["      ", "  ███ ", "  ███ ", "      "]);
    // an area reaching past the buffer is clipped to it
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
    Gauge::new(1.0, &theme).render(Rect::new(2, 1, 10, 5), &mut buf);
    assert_eq!(rows(&buf), ["    ", "  ██"]);

    for (w, h) in [(0, 0), (0, 3), (3, 0)] {
      assert!(draw(Gauge::new(0.5, &theme), w, h).content.is_empty());
    }
  }
}
