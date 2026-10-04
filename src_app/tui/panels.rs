//! Metric panels (CPU, GPU, MEM, POWER) and the box frame they share: rounded borders, titles
//! fitted on the top border and key hints on the bottom border.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::App;
use super::layout::LayoutPlan;
use super::store::{CpuFreqStore, PowerStore};
use super::widgets::{Meter, graph};
use crate::config::ViewType;

const GB: f64 = (1u64 << 30) as f64;
/// Narrowest history graph next to a POWER row; avg / max columns are dropped to keep it.
const POWER_GRAPH_MIN_WIDTH: u16 = 8;
/// Blank cells between key hints on the bottom border.
const HINT_GAP: u16 = 2;

pub(super) fn ratio(value: f64, total: f64) -> f64 {
  if total == 0.0 { 0.0 } else { value / total }
}

/// Temperature position on the load gradient: 30 °C is cool, 100 °C is hot.
fn temp_ratio(celsius: f32) -> f64 {
  (f64::from(celsius) - 30.0) / 70.0
}

fn width_u16(line: &Line) -> u16 {
  line.width().min(u16::MAX as usize) as u16
}

/// Pads `labels` with spaces to the widest one so bars next to them line up.
fn pad_labels(labels: &[String]) -> Vec<String> {
  let width = labels.iter().map(|label| label.chars().count()).max().unwrap_or(0);
  labels.iter().map(|label| format!("{label:<width$}")).collect()
}

/// Titles on the top border of a box. When the border is too short, the first left title is
/// truncated, while later left titles, the right title and the center title (in this order of
/// priority) are dropped, so titles never overlap.
#[derive(Default)]
pub(super) struct Titles<'a> {
  left: Vec<Line<'a>>,
  center: Option<Line<'a>>,
  right: Option<Line<'a>>,
}

impl<'a> Titles<'a> {
  pub(super) fn new(title: impl Into<Line<'a>>) -> Self {
    Self { left: vec![title.into()], ..Default::default() }
  }

  /// Adds a title after the existing left titles.
  pub(super) fn left(mut self, title: impl Into<Line<'a>>) -> Self {
    self.left.push(title.into());
    self
  }

  pub(super) fn center(mut self, title: impl Into<Line<'a>>) -> Self {
    self.center = Some(title.into());
    self
  }

  pub(super) fn right(mut self, title: impl Into<Line<'a>>) -> Self {
    self.right = Some(title.into());
    self
  }

  /// Draws the titles over the top border of `area`; unstyled text gets `style`.
  fn render(self, area: Rect, buf: &mut Buffer, style: Style) {
    let pad = |line: Line<'a>| {
      let mut spans = vec![Span::raw(" ")];
      spans.extend(line.spans);
      spans.push(Span::raw(" "));
      Line::from(spans).style(style.patch(line.style))
    };

    let left: Vec<Line> = self.left.into_iter().map(pad).collect();
    let center = self.center.map(pad);
    let right = self.right.map(pad);

    let widths: Vec<u16> = left.iter().map(width_u16).collect();
    let slots = place_titles(
      area.width,
      &widths,
      center.as_ref().map(width_u16),
      right.as_ref().map(width_u16),
    );

    for (line, (x, width)) in left.iter().zip(slots.left) {
      buf.set_line(area.x + x, area.y, line, width);
    }

    for (line, x) in [(center, slots.center), (right, slots.right)] {
      if let (Some(line), Some(x)) = (line, x) {
        buf.set_line(area.x + x, area.y, &line, width_u16(&line));
      }
    }
  }
}

/// Positions of titles on a border `width` cells wide, as offsets from the box's left edge.
#[derive(Debug, Default, PartialEq, Eq)]
struct TitleSlots {
  /// `(x, visible width)` of the left titles that fit.
  left: Vec<(u16, u16)>,
  center: Option<u16>,
  right: Option<u16>,
}

/// Fits titles of the given widths on a border `width` cells wide. Titles keep at least one border
/// cell between each other and next to the corners. The first left title is truncated to fit; the
/// other titles are placed in full or dropped: left ones first, then right, then center.
fn place_titles(width: u16, left: &[u16], center: Option<u16>, right: Option<u16>) -> TitleSlots {
  let mut slots = TitleSlots::default();
  // free cells for titles: [start, end)
  let mut start = 2;
  let mut end = width.saturating_sub(2);

  for (i, &title) in left.iter().enumerate() {
    let room = end.saturating_sub(start);
    if room == 0 || (i > 0 && title > room) {
      break;
    }

    let title = title.min(room);
    slots.left.push((start, title));
    start = start.saturating_add(title).saturating_add(1);
  }

  if let Some(title) = right
    && title > 0
    && start.saturating_add(title) <= end
  {
    slots.right = Some(end - title);
    end = (end - title).saturating_sub(1);
  }

  if let Some(title) = center
    && title > 0
    && start.saturating_add(title) <= end
  {
    // centered on the whole border when possible, in the gap between the other titles otherwise
    let centered = (width - title) / 2;
    let gap_center = start + (end - start - title) / 2;
    let fits = centered >= start && centered + title <= end;
    slots.center = Some(if fits { centered } else { gap_center });
  }

  slots
}

/// Number of leading `items` (by width) that fit in `room` cells with `gap` cells between them.
fn fit_count(room: u16, items: &[u16], gap: u16) -> usize {
  let mut used = 0u32;
  for (i, &item) in items.iter().enumerate() {
    let need = u32::from(item) + if i == 0 { 0 } else { u32::from(gap) };
    if used + need > u32::from(room) {
      return i;
    }
    used += need;
  }

  items.len()
}

/// One-row cells of a column-major grid for `count` items: as many rows as `area` has, as few
/// columns as needed (balanced), one blank column between columns. Empty when the columns don't
/// fit.
fn grid_cells(area: Rect, count: usize) -> Vec<Rect> {
  if area.is_empty() || count == 0 {
    return vec![];
  }

  let cols = count.div_ceil(area.height as usize);
  let rows = count.div_ceil(cols);
  let Ok(cols) = u16::try_from(cols) else { return vec![] };
  let col_width = area.width.saturating_sub(cols - 1) / cols;
  if col_width == 0 {
    return vec![];
  }

  (0..count)
    .map(|i| {
      let (col, row) = ((i / rows) as u16, (i % rows) as u16);
      Rect::new(area.x + col * (col_width + 1), area.y + row, col_width, 1)
    })
    .collect()
}

impl App {
  /// Box or metric name: title color, bold.
  pub(super) fn heading<'a>(&self, text: impl Into<Cow<'a, str>>) -> Span<'a> {
    Span::styled(text, Style::new().fg(self.theme.title).add_modifier(Modifier::BOLD))
  }

  pub(super) fn text<'a>(&self, text: impl Into<Cow<'a, str>>) -> Span<'a> {
    Span::styled(text, self.theme.text)
  }

  pub(super) fn dim<'a>(&self, text: impl Into<Cow<'a, str>>) -> Span<'a> {
    Span::styled(text, self.theme.dim)
  }

  /// Temperature colored by the load gradient.
  fn temp<'a>(&self, celsius: f32) -> Span<'a> {
    Span::styled(format!("{celsius:.0}°C"), self.theme.gradient(temp_ratio(celsius)))
  }

  fn block_meters(&self) -> bool {
    self.cfg.view_type == ViewType::Block
  }

  /// Draws a rounded box with `titles` on the top border. Returns the area inside the borders.
  pub(super) fn draw_box(&self, f: &mut Frame, area: Rect, titles: Titles) -> Rect {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(self.theme.border);
    let inner = block.inner(area);
    f.render_widget(block, area);
    titles.render(area, f.buffer_mut(), Style::new().fg(self.theme.text));
    inner
  }

  /// Draws the global key hints over the bottom border of box `area`. Hints that don't fit are
  /// dropped from the end, so `q quit` stays visible as long as possible. Returns where the hints
  /// end, as for `draw_hints`.
  pub(super) fn render_key_hints(&self, f: &mut Frame, area: Rect) -> Option<u16> {
    let view = match self.cfg.view_type {
      ViewType::Braille => "braille",
      ViewType::Block => "block",
    };
    let hints = [
      ("q", "quit".to_string()),
      ("c", self.theme.name.to_string()),
      ("v", view.to_string()),
      ("d", "cores".to_string()),
      ("r", self.cfg.ratio_mode.label().to_string()),
      ("-/+", format!("{}ms", self.cfg.interval)),
      ("1-5", "panels".to_string()),
    ];

    self.draw_hints(f, area, 2, &hints)
  }

  /// Draws `(key, label)` hints over the bottom border of box `area`, starting `start` cells from
  /// its left edge. Hints that don't fit are dropped from the end. Returns the offset from the left
  /// edge where the drawn hints end, `None` when none fit.
  pub(super) fn draw_hints(
    &self,
    f: &mut Frame,
    area: Rect,
    start: u16,
    hints: &[(&str, String)],
  ) -> Option<u16> {
    let items: Vec<[Span; 2]> = hints
      .iter()
      .map(|(key, label)| [self.heading(*key), self.text(format!(" {label}"))])
      .collect();
    let widths: Vec<u16> =
      items.iter().map(|[key, label]| (key.width() + label.width()) as u16).collect();

    // the line keeps a border cell and the corner on the right, hints get a padding space on
    // both sides
    let room = area.width.saturating_sub(start).saturating_sub(2);
    let count = fit_count(room.saturating_sub(2), &widths, HINT_GAP);
    if count == 0 {
      return None;
    }

    let gap = " ".repeat(HINT_GAP as usize);
    let mut spans = vec![Span::raw(" ")];
    for (i, item) in items.into_iter().take(count).enumerate() {
      if i > 0 {
        spans.push(Span::raw(gap.clone()));
      }
      spans.extend(item);
    }
    spans.push(Span::raw(" "));

    let line = Line::from(spans).style(self.theme.text);
    f.buffer_mut().set_line(area.x + start, area.bottom() - 1, &line, room);
    Some(start + width_u16(&line))
  }

  /// CPU box: chip info, clock and version in the title, E-CPU / P-CPU graphs and the optional
  /// per-core meter grid.
  pub(super) fn render_cpu_box(&self, f: &mut Frame, plan: &LayoutPlan) {
    let Some(area) = plan.cpu else { return };
    let soc = &self.soc;

    let mut name = vec![self.heading("cpu")];
    let temp = self.cpu_temp.last();
    if temp > 0.0 {
      name.extend([Span::raw(" "), self.temp(temp)]);
    }

    let chip = format!(
      "{} · {}{}+{}{} · {}GPU · {}GB",
      soc.chip_name,
      soc.ecpu_cores,
      soc.ecpu_label,
      soc.pcpu_cores,
      soc.pcpu_label,
      soc.gpu_cores,
      soc.memory_gb,
    );
    let clock = chrono::Local::now().format("%H:%M:%S").to_string();
    let brand = format!(
      "{} v{} · {}ms",
      env!("CARGO_PKG_NAME"),
      env!("CARGO_PKG_VERSION"),
      self.cfg.interval
    );

    let titles = Titles::new(name).left(chip).center(clock).right(brand);
    self.draw_box(f, area, titles);

    if let Some(graphs) = plan.cpu_graphs {
      self.render_cpu_graphs(f, graphs);
    }

    if let Some(cores) = plan.cores {
      self.render_core_grid(f, cores);
    }
  }

  /// E-CPU and P-CPU history graphs, stacked (side by side when there is only one row).
  fn render_cpu_graphs(&self, f: &mut Frame, area: Rect) {
    let constraints = [Constraint::Fill(1); 2];
    let [ecpu, pcpu] = if area.height >= 2 {
      Layout::vertical(constraints).areas(area)
    } else {
      Layout::horizontal(constraints).spacing(1).areas(area)
    };

    self.render_cluster_graph(f, ecpu, &self.soc.ecpu_label, &self.ecpu_freq);
    self.render_cluster_graph(f, pcpu, &self.soc.pcpu_label, &self.pcpu_freq);
  }

  fn render_cluster_graph(&self, f: &mut Frame, area: Rect, cluster: &str, store: &CpuFreqStore) {
    let freq = &store.aggregate;
    let series = freq.ratio(self.cfg.ratio_mode);
    let label = Line::from(vec![
      self.heading(format!("{cluster}-CPU")),
      self.text(format!(" {:.0}% @ {} MHz", series.ratio * 100.0, freq.freq_mhz)),
    ]);

    let w = graph(self.cfg.view_type, &series.items, &self.theme).max(100).label(label);
    f.render_widget(w, area);
  }

  /// Per-core meters, E cores then P cores, in as many columns as needed.
  fn render_core_grid(&self, f: &mut Frame, area: Rect) {
    // one blank column between the graphs and the grid
    let area = Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area };

    let mode = self.cfg.ratio_mode;
    let with_die = self.ecpu_freq.has_multiple_dies() || self.pcpu_freq.has_multiple_dies();
    let mut cores = self.ecpu_freq.core_ratios(&self.soc.ecpu_label, mode, with_die);
    cores.extend(self.pcpu_freq.core_ratios(&self.soc.pcpu_label, mode, with_die));

    let (labels, ratios): (Vec<String>, Vec<f64>) = cores.into_iter().unzip();
    let cells = grid_cells(area, labels.len());
    for ((cell, label), ratio) in cells.into_iter().zip(pad_labels(&labels)).zip(ratios) {
      let meter = Meter::new(label, ratio, &self.theme).block_chars(self.block_meters());
      f.render_widget(meter, cell);
    }
  }

  /// GPU box: usage, frequency and temperature in the title, usage history inside.
  pub(super) fn render_gpu_box(&self, f: &mut Frame, area: Rect) {
    let freq = &self.igpu_freq;
    let series = freq.ratio(self.cfg.ratio_mode);

    let mut title = vec![
      self.heading("gpu"),
      self.text(format!(" {:.0}% @ {} MHz", series.ratio * 100.0, freq.freq_mhz)),
    ];
    let temp = self.gpu_temp.last();
    if temp > 0.0 {
      title.extend([self.dim(" · "), self.temp(temp)]);
    }

    let inner = self.draw_box(f, area, Titles::new(title));
    f.render_widget(graph(self.cfg.view_type, &series.items, &self.theme).max(100), inner);
  }

  /// MEM box: RAM and SWAP (when present) meters, RAM history below them.
  pub(super) fn render_mem_box(&self, f: &mut Frame, area: Rect) {
    let inner = self.draw_box(f, area, Titles::new(self.heading("mem")));
    let mem = &self.mem;

    let mut meters = vec![("RAM", mem.ram_usage, mem.ram_total)];
    if mem.swap_total > 0 {
      meters.push(("SWAP", mem.swap_usage, mem.swap_total));
    }

    let labels: Vec<String> = meters
      .iter()
      .map(|(name, used, total)| {
        format!("{name:<4} {:>5.2}/{:.1} GB", *used as f64 / GB, *total as f64 / GB)
      })
      .collect();

    let mut rest = inner;
    for ((_, used, total), label) in meters.iter().zip(pad_labels(&labels)) {
      if rest.is_empty() {
        return;
      }

      let value = ratio(*used as f64, *total as f64);
      let meter = Meter::new(label, value, &self.theme).block_chars(self.block_meters());
      f.render_widget(meter, Rect { height: 1, ..rest });
      rest = Rect { y: rest.y + 1, height: rest.height - 1, ..rest };
    }

    if !rest.is_empty() {
      f.render_widget(graph(self.cfg.view_type, &mem.items, &self.theme).max(mem.ram_total), rest);
    }
  }

  /// POWER box: total power with avg / max in the title, CPU / GPU / ANE rows with history graphs
  /// and a SYS / fans footer when those sensors are available.
  pub(super) fn render_power_box(&self, f: &mut Frame, area: Rect) {
    let all = &self.all_power;
    let title = vec![
      self.heading("power"),
      self.text(format!(" {:.2}W", all.top_value)),
      self.dim(" · avg "),
      self.text(format!("{:.2}W", all.avg_value)),
      self.dim(" · max "),
      self.text(format!("{:.2}W", all.max_value)),
    ];

    let inner = self.draw_box(f, area, Titles::new(title));
    if inner.is_empty() {
      return;
    }

    let units = [
      ("CPU", &self.cpu_power, self.cpu_temp.last()),
      ("GPU", &self.gpu_power, self.gpu_temp.last()),
      ("ANE", &self.ane_power, 0.0),
    ];

    // avg / max columns only when a short graph still fits next to them
    let full = width_u16(&Line::from(self.power_row(units[0].0, units[0].1, units[0].2, true)));
    let stats = inner.width >= full + 1 + POWER_GRAPH_MIN_WIDTH;
    let footer = self.power_footer(stats);
    let buf = f.buffer_mut();

    // too low for a row per unit: all of them on the first row, footer below when it fits
    if inner.height < units.len() as u16 {
      let mut spans = vec![];
      for (i, (label, store, temp)) in units.into_iter().enumerate() {
        if i > 0 {
          spans.push(self.dim(" · "));
        }
        spans.extend([self.heading(label), self.text(format!(" {:.2}W", store.top_value))]);
        if temp > 0.0 {
          spans.extend([Span::raw(" "), self.temp(temp)]);
        }
      }

      buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);
      if let Some(footer) = footer
        && inner.height > 1
      {
        buf.set_line(inner.x, inner.y + 1, &footer, inner.width);
      }
      return;
    }

    let footer_height = u16::from(footer.is_some() && inner.height > units.len() as u16);
    let body = Rect { height: inner.height - footer_height, ..inner };
    let rows: [Rect; 3] = Layout::vertical([Constraint::Fill(1); 3]).areas(body);

    for (row, (label, store, temp)) in rows.into_iter().zip(units) {
      let line = Line::from(self.power_row(label, store, temp, stats));
      let text_width = width_u16(&line);
      buf.set_line(row.x, row.y, &line, row.width);

      // history graph right of the text, over the full row height
      if row.width > text_width + 1 {
        let x = row.x + text_width + 1;
        let graph_area = Rect { x, width: row.right() - x, ..row };
        graph(self.cfg.view_type, &store.items, &self.theme).render(graph_area, buf);
      }
    }

    if let Some(footer) = footer
      && footer_height == 1
    {
      buf.set_line(inner.x, inner.bottom() - 1, &footer, inner.width);
    }
  }

  /// Fixed-width POWER row text: name, watts, optional avg / max and temperature (blank when the
  /// sensor is missing, so rows stay aligned).
  fn power_row(
    &self,
    label: &str,
    store: &PowerStore,
    temp: f32,
    stats: bool,
  ) -> Vec<Span<'static>> {
    let mut spans =
      vec![self.heading(format!("{label:<4}")), self.text(format!("{:>6.2}W", store.top_value))];
    if stats {
      spans.extend(self.power_stats(store));
    }

    if temp > 0.0 {
      spans.extend([Span::raw("  "), self.temp(temp)]);
    } else {
      spans.push(Span::raw("      "));
    }
    spans
  }

  fn power_stats<'a>(&self, store: &PowerStore) -> [Span<'a>; 4] {
    [
      self.dim(" avg "),
      self.text(format!("{:>5.2}", store.avg_value)),
      self.dim(" max "),
      self.text(format!("{:>5.2}", store.max_value)),
    ]
  }

  /// SYS power and fan speeds, `None` when neither sensor is available.
  fn power_footer(&self, stats: bool) -> Option<Line<'static>> {
    let mut spans = vec![];

    let sys = &self.sys_power;
    if sys.top_value > 0.0 {
      spans.extend([self.heading("SYS "), self.text(format!("{:>6.2}W", sys.top_value))]);
      if stats {
        spans.extend(self.power_stats(sys));
      }
    }

    let fans = self.fans.label();
    if !fans.is_empty() {
      if !spans.is_empty() {
        spans.push(Span::raw("  "));
      }
      spans.push(self.text(fans));
    }

    if spans.is_empty() { None } else { Some(Line::from(spans)) }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::layout::Rect;

  use super::{TitleSlots, fit_count, grid_cells, pad_labels, place_titles, temp_ratio};

  fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
  }

  #[test]
  fn titles_fit_on_wide_border() {
    // left at 2, right ends 2 cells before the box edge, center in the middle
    let slots = place_titles(60, &[10, 8], Some(6), Some(12));
    assert_eq!(slots.left, vec![(2, 10), (13, 8)]);
    assert_eq!(slots.right, Some(46));
    assert_eq!(slots.center, Some(27));
  }

  #[test]
  fn right_title_dropped_instead_of_overlapping_left() {
    // the 40%-wide POWER box case: left and right don't fit together
    let slots = place_titles(40, &[30], None, Some(20));
    assert_eq!(slots, TitleSlots { left: vec![(2, 30)], center: None, right: None });

    // exactly one border cell between them still fits
    let slots = place_titles(40, &[15], None, Some(20));
    assert_eq!(slots.right, Some(18));
    assert_eq!(2 + 15 + 1, 18);
  }

  #[test]
  fn first_left_title_is_truncated_others_dropped() {
    let slots = place_titles(20, &[30, 4], Some(4), Some(4));
    assert_eq!(slots, TitleSlots { left: vec![(2, 16)], center: None, right: None });

    // a later left title is dropped when it doesn't fit in full
    let slots = place_titles(20, &[10, 8], None, None);
    assert_eq!(slots.left, vec![(2, 10)]);
  }

  #[test]
  fn center_title_moves_aside_or_drops() {
    // a long left title moves the center title to the middle of the free cells [23, 38)
    let slots = place_titles(40, &[20], Some(6), None);
    assert_eq!(slots.center, Some(27));

    // no room left between left and right titles
    let slots = place_titles(40, &[20], Some(6), Some(10));
    assert_eq!(slots.right, Some(28));
    assert_eq!(slots.center, None);
  }

  #[test]
  fn tiny_borders_place_nothing() {
    for width in 0..=4 {
      let slots = place_titles(width, &[5], Some(3), Some(3));
      assert!(slots.center.is_none() && slots.right.is_none(), "width {width}");
      assert!(slots.left.iter().all(|(x, w)| x + w <= width.saturating_sub(2)), "width {width}");
    }
    assert_eq!(place_titles(5, &[5], None, None).left, vec![(2, 1)]);
  }

  #[test]
  fn titles_never_overlap() {
    let sizes = [0, 1, 3, 6, 9, 14, 20, 33];
    for width in 0..=120 {
      for (a, b) in sizes.iter().flat_map(|a| sizes.map(|b| (*a, b))) {
        for (center, right) in [(None, None), (Some(a), Some(b)), (Some(b), Some(a))] {
          let slots = place_titles(width, &[a, b], center, right);
          let mut spans: Vec<(u16, u16)> = slots.left.clone();
          spans.extend(slots.center.zip(center));
          spans.extend(slots.right.zip(right));

          let ctx = format!("width {width} left [{a}, {b}] center {center:?} right {right:?}");
          for (i, &(x, w)) in spans.iter().enumerate() {
            assert!(x >= 2 && x + w + 2 <= width, "{ctx}: ({x}, {w}) off the border");
            for &(x2, w2) in &spans[i + 1..] {
              assert!(x + w < x2 || x2 + w2 < x, "{ctx}: ({x}, {w}) touches ({x2}, {w2})");
            }
          }
        }
      }
    }
  }

  #[test]
  fn fit_count_keeps_leading_items() {
    assert_eq!(fit_count(30, &[6, 9, 9], 2), 3); // 6 + 2 + 9 + 2 + 9 = 28
    assert_eq!(fit_count(27, &[6, 9, 9], 2), 2);
    assert_eq!(fit_count(6, &[6, 9, 9], 2), 1);
    assert_eq!(fit_count(5, &[6, 9, 9], 2), 0);
    assert_eq!(fit_count(0, &[], 2), 0);
    assert_eq!(fit_count(u16::MAX, &[u16::MAX, u16::MAX], 2), 1);
  }

  #[test]
  fn grid_single_column_when_rows_suffice() {
    let cells = grid_cells(rect(10, 5, 30, 14), 12);
    assert_eq!(cells.len(), 12);
    assert_eq!(cells[0], rect(10, 5, 30, 1));
    assert_eq!(cells[11], rect(10, 16, 30, 1));
  }

  #[test]
  fn grid_fills_columns_top_down_and_balances_rows() {
    // 12 cores in 5 rows: 3 columns of 4, one blank column between columns
    let cells = grid_cells(rect(0, 0, 32, 5), 12);
    assert_eq!(cells.len(), 12);
    assert_eq!(cells[0], rect(0, 0, 10, 1));
    assert_eq!(cells[3], rect(0, 3, 10, 1));
    assert_eq!(cells[4], rect(11, 0, 10, 1));
    assert_eq!(cells[11], rect(22, 3, 10, 1));

    // 32 cores (M3 Ultra) in 6 rows: 6 columns of 9 cells
    let cells = grid_cells(rect(0, 0, 60, 6), 32);
    assert_eq!(cells.len(), 32);
    assert_eq!(cells.last(), Some(&rect(50, 1, 9, 1)));
  }

  #[test]
  fn grid_empty_cases() {
    assert!(grid_cells(rect(0, 0, 30, 10), 0).is_empty());
    assert!(grid_cells(rect(0, 0, 0, 10), 4).is_empty());
    assert!(grid_cells(rect(0, 0, 30, 0), 4).is_empty());
    // 12 columns of 1 row don't fit in 10 cells
    assert!(grid_cells(rect(0, 0, 10, 1), 12).is_empty());
  }

  #[test]
  fn labels_are_padded_to_widest() {
    let labels = ["E0".to_string(), "D1 P11".to_string()];
    assert_eq!(pad_labels(&labels), ["E0    ", "D1 P11"]);
    assert!(pad_labels(&[]).is_empty());
  }

  #[test]
  fn temperature_maps_onto_gradient() {
    assert_eq!(temp_ratio(30.0), 0.0);
    assert_eq!(temp_ratio(65.0), 0.5);
    assert_eq!(temp_ratio(100.0), 1.0);
  }
}
