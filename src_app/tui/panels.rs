//! Metrics box (CPU cluster / GPU / RAM / SWAP strips, power column) and the box frame it shares
//! with the process list: rounded borders, titles fitted on the top border and key hints on the
//! bottom border.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::App;
use super::layout::{Content, LayoutPlan, PowerSize, Strip, compute_layout};
use super::store::{FreqStore, PowerStore};
use super::widgets::{Graph, Meter};
use crate::config::RatioMode;

const GB: f64 = (1u64 << 30) as f64;
/// Between key hints on the bottom border.
const HINT_SEPARATOR: &str = " | ";
/// Narrowest power history graph: a power column with everything is the text plus this graph, a
/// narrower one drops the graphs.
const POWER_GRAPH_MIN: u16 = 12;
/// Between the Total power and the fans on one row.
const FANS_GAP: &str = "  ";
/// Narrowest detail column of the strips (`1.8GHz`, `20/36G`).
const DETAIL_MIN_WIDTH: usize = 6;
/// Strip labels besides the CPU clusters', for the label column width.
const STRIP_LABELS: [&str; 3] = ["GPU", "RAM", "SWAP"];

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

fn spans_width(spans: &[Span]) -> u16 {
  spans.iter().map(Span::width).sum::<usize>().min(u16::MAX as usize) as u16
}

/// Strip label of a CPU cluster: `E-CPU`.
fn cluster_name(label: &str) -> String {
  format!("{label}-CPU")
}

/// Frequency as `1.8GHz`.
fn format_ghz(mhz: u64) -> String {
  format!("{:.1}GHz", mhz as f64 / 1000.0)
}

/// Size in GB without a needless fraction: `1.2`, `4`, `21`.
fn format_gb(bytes: u64) -> String {
  let gb = bytes as f64 / GB;
  let text = if gb >= 10.0 { format!("{gb:.0}") } else { format!("{gb:.1}") };
  match text.strip_suffix(".0") {
    Some(whole) => whole.to_string(),
    None => text,
  }
}

/// Titles on the top border of a box: left titles and an optional right one. When the border is
/// too short, the first left title is truncated, while later left titles and then the right
/// title are dropped, so titles never overlap.
#[derive(Default)]
pub(super) struct Titles<'a> {
  left: Vec<Line<'a>>,
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

  pub(super) fn right(mut self, title: impl Into<Line<'a>>) -> Self {
    self.right = Some(title.into());
    self
  }

  /// Draws the titles over the top border of `area`; unstyled text gets `style`. Returns the
  /// cells of the text of the left titles that fit, in order, without the blank cell on both
  /// sides.
  fn render(self, area: Rect, buf: &mut Buffer, style: Style) -> Vec<Rect> {
    let pad = |line: Line<'a>| {
      let mut spans = vec![Span::raw(" ")];
      spans.extend(line.spans);
      spans.push(Span::raw(" "));
      Line::from(spans).style(style.patch(line.style))
    };

    let left: Vec<Line> = self.left.into_iter().map(pad).collect();
    let right = self.right.map(pad);

    let widths: Vec<u16> = left.iter().map(width_u16).collect();
    let slots = place_titles(area.width, &widths, right.as_ref().map(width_u16));

    let mut texts = vec![];
    for (line, (x, width)) in left.iter().zip(slots.left) {
      buf.set_line(area.x + x, area.y, line, width);
      // a truncated first title loses its trailing blank and then its text
      let text = (width_u16(line) - 2).min(width.saturating_sub(1));
      texts.push(Rect::new(area.x + x + 1, area.y, text, 1));
    }

    if let (Some(line), Some(x)) = (right, slots.right) {
      buf.set_line(area.x + x, area.y, &line, width_u16(&line));
    }

    texts
  }
}

/// Positions of titles on a border `width` cells wide, as offsets from the box's left edge.
#[derive(Debug, Default, PartialEq, Eq)]
struct TitleSlots {
  /// `(x, visible width)` of the left titles that fit.
  left: Vec<(u16, u16)>,
  right: Option<u16>,
}

/// Fits titles of the given widths on a border `width` cells wide. Titles keep at least one border
/// cell between each other and next to the corners. The first left title is truncated to fit; the
/// other titles are placed in full or dropped: left ones first, then the right one.
fn place_titles(width: u16, left: &[u16], right: Option<u16>) -> TitleSlots {
  let mut slots = TitleSlots::default();
  // free cells for titles: [start, end)
  let mut start = 2;
  let end = width.saturating_sub(2);

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

/// Text and body of one strip: `{label} {pct}% {detail} {graph or meter}`.
struct StripData<'a> {
  label: String,
  ratio: f64,
  detail: String,
  /// Percent history drawn as a graph; `None` draws a meter of `ratio` instead.
  history: Option<&'a [u64]>,
}

/// Strip of a frequency / usage history (CPU cluster, GPU).
fn freq_strip(label: String, freq: &FreqStore, mode: RatioMode) -> StripData<'_> {
  let series = freq.ratio(mode);
  StripData {
    label,
    ratio: series.ratio,
    detail: format_ghz(freq.freq_mhz),
    history: Some(&series.items),
  }
}

/// Meter strip of a used / total size (RAM, SWAP).
fn size_strip(label: &str, used: u64, total: u64) -> StripData<'static> {
  StripData {
    label: label.to_string(),
    ratio: ratio(used as f64, total as f64),
    detail: format!("{}/{}G", format_gb(used), format_gb(total)),
    history: None,
  }
}

/// One row of the power column in parts: `CPU    4.50W` ` (3.10, 8.20)` `  45°C` `▃▅▂▃`.
#[derive(Default)]
struct PowerRow<'a> {
  /// `CPU    4.50W`, or the fans on a row of their own: always shown.
  head: Vec<Span<'static>>,
  /// ` (3.10, 8.20)`: average and maximum.
  stats: Vec<Span<'static>>,
  /// `  45°C`, blank for a unit without a sensor; empty for rows without a temperature. The
  /// leading gap lines the temperatures up across rows.
  temp: Vec<Span<'static>>,
  /// Fans after the Total power, when they fit on its row.
  tail: Vec<Span<'static>>,
  history: Option<&'a [u64]>,
}

impl PowerRow<'_> {
  fn widths(&self) -> PowerWidths {
    PowerWidths {
      head: spans_width(&self.head),
      stats: spans_width(&self.stats),
      temp: spans_width(&self.temp),
      tail: spans_width(&self.tail),
      graph: self.history.is_some(),
    }
  }
}

/// Cells taken by the parts of a power row.
#[derive(Debug, Default, Clone, Copy)]
struct PowerWidths {
  head: u16,
  stats: u16,
  temp: u16,
  tail: u16,
  /// The row has a history graph.
  graph: bool,
}

impl PowerWidths {
  /// Text without the fans, with or without the average / maximum and the temperature.
  fn text(&self, stats: bool, temp: bool) -> u16 {
    let stats = if stats { self.stats } else { 0 };
    let temp = if temp { self.temp } else { 0 };
    self.head + stats + temp
  }
}

/// Parts of the power rows shown in a column. They are the same for every row, so the columns
/// line up.
#[derive(Debug, PartialEq, Eq)]
struct PowerFit {
  /// Average and maximum.
  stats: bool,
  temp: bool,
  /// Offset of the history graphs from the left edge of the column; `None` hides them.
  graph: Option<u16>,
}

/// Fits power rows of the given widths in a column `width` cells wide. Parts are dropped from the
/// right: the history graphs first, then the temperatures, the average and maximum last; the
/// current power always stays. A dropped part doesn't come back in the room freed by later ones.
fn fit_power(rows: &[PowerWidths], width: u16) -> PowerFit {
  let widest = |stats, temp| rows.iter().map(|row| row.text(stats, temp)).max().unwrap_or(0);
  let stats = widest(true, false) <= width;
  let temp = stats && widest(true, true) <= width;

  // the graphs start one cell after the widest text of the rows with a graph, and only next to
  // everything else
  let graph_rows = rows.iter().filter(|row| row.graph);
  let x = graph_rows.map(|row| row.text(true, true)).max().map(|text| text + 1);
  let graph = x.filter(|x| temp && x + POWER_GRAPH_MIN <= width);
  PowerFit { stats, temp, graph }
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

  /// Temperature colored by the load gradient, 5 cells wide (` 45°C`).
  fn temp<'a>(&self, celsius: f32) -> Span<'a> {
    Span::styled(format!("{celsius:>3.0}°C"), self.theme.gradient(temp_ratio(celsius)))
  }

  /// Draws a rounded box with `titles` on the top border. Returns the area inside the borders and
  /// the cells of the left titles' text that fit (see `Titles::render`).
  pub(super) fn draw_box(&self, f: &mut Frame, area: Rect, titles: Titles) -> (Rect, Vec<Rect>) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(self.theme.border);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let texts = titles.render(area, f.buffer_mut(), Style::new().fg(self.theme.text));
    (inner, texts)
  }

  /// Draws the global key hints right-aligned over the bottom border of box `area`: `q quit |
  /// p procs | r scaled | -/+ 1000ms`, with or without the process list (its own controls are in
  /// its box). Hints that don't fit are dropped from the end, so `q quit` stays as long as it fits.
  pub(super) fn render_key_hints(&self, f: &mut Frame, area: Rect) {
    let hints = [
      ("q", "quit".to_string()),
      ("p", "procs".to_string()),
      ("r", self.cfg.ratio_mode.label().to_string()),
      ("-/+", format!("{}ms", self.cfg.interval)),
    ];

    let items: Vec<[Span; 2]> = hints
      .into_iter()
      .map(|(key, label)| [self.heading(key), self.text(format!(" {label}"))])
      .collect();
    let widths: Vec<u16> = items.iter().map(|item| spans_width(item)).collect();

    // as the right title: a border cell before the corner, a blank cell on both sides of the text
    let room = area.width.saturating_sub(4);
    let separator = HINT_SEPARATOR.len() as u16;
    let count = fit_count(room.saturating_sub(2), &widths, separator);
    if count == 0 {
      return;
    }

    let mut spans = vec![Span::raw(" ")];
    for (i, item) in items.into_iter().take(count).enumerate() {
      if i > 0 {
        spans.push(self.dim(HINT_SEPARATOR));
      }
      spans.extend(item);
    }
    spans.push(Span::raw(" "));

    let line = Line::from(spans);
    let width = width_u16(&line);
    f.buffer_mut().set_line(area.right() - 2 - width, area.bottom() - 1, &line, width);
  }

  /// Screen layout for the current metrics.
  pub(super) fn layout(&self, area: Rect) -> LayoutPlan {
    let content = Content {
      clusters: self.clusters.items.len(),
      swap: self.mem.swap_total > 0,
      power: self.power_size(),
    };

    compute_layout(area, self.cfg.show_procs, &content)
  }

  /// Width of the strip label column: the widest strip label.
  fn label_width(&self) -> usize {
    let clusters = self.clusters.items.iter().map(|c| cluster_name(&c.label).chars().count());
    STRIP_LABELS.iter().map(|label| label.len()).chain(clusters).max().unwrap_or(0)
  }

  /// Metrics box: chip summary and version in the title, strips and the power column inside.
  pub(super) fn render_metrics_box(&self, f: &mut Frame, plan: &LayoutPlan) {
    let Some(area) = plan.top else { return };
    self.draw_box(f, area, self.metrics_titles());
    let buf = f.buffer_mut();

    let label_width = self.label_width();
    let strips: Vec<(StripData, Rect)> =
      plan.strips.iter().filter_map(|(strip, r)| Some((self.strip_data(*strip)?, *r))).collect();
    let details = strips.iter().map(|(strip, _)| strip.detail.chars().count());
    let detail_width = details.max().unwrap_or(0).max(DETAIL_MIN_WIDTH);
    for (strip, area) in strips {
      self.render_strip(buf, strip, area, label_width, detail_width);
    }

    if let Some(sep) = plan.separator {
      for y in sep.top()..sep.bottom() {
        buf[(sep.x, y)].set_symbol("│").set_fg(self.theme.border);
      }
    }

    if let Some(power) = plan.power {
      self.render_power(buf, power);
    }
  }

  /// Chip summary left (`M3 Pro · 6E+6P · 18GPU · 36GB`), `macmon vX` right; the version gives
  /// way to the chip summary.
  fn metrics_titles(&self) -> Titles<'static> {
    let version = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    Titles::new(self.chip_title()).right(self.text(version))
  }

  /// `M3 Pro · 6E+6P · 18GPU · 36GB`; core counts come from the CPU clusters, so a third tier
  /// shows up as `6E+4P+2S`.
  fn chip_title(&self) -> Line<'static> {
    let soc = &self.soc;
    let name = soc.chip_name.strip_prefix("Apple ").unwrap_or(&soc.chip_name);
    let counts: Vec<String> =
      self.clusters.items.iter().map(|c| format!("{}{}", c.count, c.label)).collect();

    let mut parts = vec![];
    if !name.is_empty() {
      parts.push(self.heading(name.to_string()));
    }
    if !counts.is_empty() {
      parts.push(self.text(counts.join("+")));
    }
    if soc.gpu_cores > 0 {
      parts.push(self.text(format!("{}GPU", soc.gpu_cores)));
    }
    if soc.memory_gb > 0 {
      parts.push(self.text(format!("{}GB", soc.memory_gb)));
    }
    if parts.is_empty() {
      parts.push(self.heading(env!("CARGO_PKG_NAME")));
    }

    let mut spans = vec![];
    for (i, part) in parts.into_iter().enumerate() {
      if i > 0 {
        spans.push(self.dim(" · "));
      }
      spans.push(part);
    }
    Line::from(spans)
  }

  fn strip_data(&self, strip: Strip) -> Option<StripData<'_>> {
    let mode = self.cfg.ratio_mode;
    let mem = &self.mem;
    Some(match strip {
      Strip::Cluster(i) => {
        let cluster = self.clusters.items.get(i)?;
        freq_strip(cluster_name(&cluster.label), &cluster.freq, mode)
      }
      Strip::Gpu => freq_strip("GPU".to_string(), &self.igpu_freq, mode),
      Strip::Ram => size_strip("RAM", mem.ram_usage, mem.ram_total),
      Strip::Swap => size_strip("SWAP", mem.swap_usage, mem.swap_total),
    })
  }

  /// One strip: its text, then the history graph or meter up to the end of the row.
  fn render_strip(
    &self,
    buf: &mut Buffer,
    strip: StripData,
    area: Rect,
    label_width: usize,
    detail_width: usize,
  ) {
    let line = Line::from(vec![
      self.heading(format!("{:<label_width$}", strip.label)),
      Span::styled(format!(" {:>3.0}% ", strip.ratio * 100.0), self.theme.gradient(strip.ratio)),
      self.text(format!("{:<detail_width$} ", strip.detail)),
    ]);
    buf.set_line(area.x, area.y, &line, area.width);

    let text_width = width_u16(&line);
    if area.width <= text_width {
      return;
    }

    let body = Rect { x: area.x + text_width, width: area.width - text_width, ..area };
    match strip.history {
      Some(items) => Graph::new(items, &self.theme).max(100).render(body, buf),
      None => Meter::new(strip.ratio, &self.theme).render(body, buf),
    }
  }

  /// `{label} {current}W ({avg}, {max})` of one power sensor, as in the original UI; labels are 5
  /// cells wide (`Power`, `Total`), the parentheses and the comma dim.
  fn power_row(&self, label: &str, store: &PowerStore) -> PowerRow<'static> {
    PowerRow {
      head: vec![
        self.heading(format!("{label:<5}")),
        self.text(format!(" {:>5.2}W", store.top_value)),
      ],
      stats: vec![
        self.dim(" ("),
        self.text(format!("{:.2}", store.avg_value)),
        self.dim(", "),
        self.text(format!("{:.2}", store.max_value)),
        self.dim(")"),
      ],
      ..Default::default()
    }
  }

  /// Power rows `width` cells wide: CPU / GPU / ANE with average, maximum, temperature and history,
  /// then `Power` (CPU + GPU + ANE) and `Total` (system) with average and maximum, and the fans.
  /// Total and the fans show only when available; the fans get a row of their own when they don't
  /// fit after Total.
  fn power_rows(&self, width: u16) -> Vec<PowerRow<'_>> {
    let units = [
      ("CPU", &self.cpu_power, self.cpu_temp.last()),
      ("GPU", &self.gpu_power, self.gpu_temp.last()),
      ("ANE", &self.ane_power, 0.0),
    ];

    let mut rows: Vec<PowerRow> = units
      .iter()
      .map(|&(label, store, _)| PowerRow {
        history: Some(&store.items),
        ..self.power_row(label, store)
      })
      .collect();

    // the temperatures start one cell after the widest numbers; a missing sensor leaves the cells
    // blank, so the graphs line up
    let numbers = |row: &PowerRow| spans_width(&row.head) + spans_width(&row.stats);
    let temp_x = rows.iter().map(numbers).max().unwrap_or(0);
    for (row, (_, _, temp)) in rows.iter_mut().zip(units) {
      let gap = Span::raw(" ".repeat(usize::from(temp_x - numbers(row)) + 1));
      let temp = if temp > 0.0 { self.temp(temp) } else { Span::raw("     ") };
      row.temp = vec![gap, temp];
    }
    rows.push(self.power_row("Power", &self.all_power));

    let total = (self.sys_power.top_value > 0.0).then(|| self.power_row("Total", &self.sys_power));
    let fans = self.fans.label();
    let fans = (!fans.is_empty()).then(|| self.text(fans));
    match (total, fans) {
      (Some(total), Some(fans)) => {
        let tail = FANS_GAP.len() + fans.width();
        if usize::from(total.widths().text(true, false)) + tail <= usize::from(width) {
          rows.push(PowerRow { tail: vec![Span::raw(FANS_GAP), fans], ..total });
        } else {
          rows.extend([total, PowerRow { head: vec![fans], ..Default::default() }]);
        }
      }
      (total, fans) => {
        rows.extend(total);
        rows.extend(fans.map(|fans| PowerRow { head: vec![fans], ..Default::default() }));
      }
    }

    rows
  }

  /// Size of the power rows for the layout.
  fn power_size(&self) -> PowerSize {
    // fans on the Total row, as wide rows have them
    let rows: Vec<PowerWidths> = self.power_rows(u16::MAX).iter().map(PowerRow::widths).collect();
    let full = |row: &PowerWidths| {
      let graph = if row.graph { 1 + POWER_GRAPH_MIN } else { 0 };
      row.text(true, true) + row.tail + graph
    };
    let fans_inline =
      rows.iter().find(|row| row.tail > 0).map(|row| row.text(true, false) + row.tail);

    PowerSize {
      rows: rows.len().min(u16::MAX as usize) as u16,
      width: rows.iter().map(full).max().unwrap_or(0),
      min_width: rows.iter().map(|row| row.text(true, false)).max().unwrap_or(0),
      fans_inline: fans_inline.unwrap_or(0),
    }
  }

  /// Power rows top-down in `area`, history graphs in the low load color right of the text.
  fn render_power(&self, buf: &mut Buffer, area: Rect) {
    let rows = self.power_rows(area.width);
    let fit = fit_power(&rows.iter().map(PowerRow::widths).collect::<Vec<_>>(), area.width);
    let color = self.theme.gradient(0.0);

    for (row, y) in rows.into_iter().zip(area.top()..area.bottom()) {
      let mut spans = row.head;
      if fit.stats {
        spans.extend(row.stats);
      }
      if fit.temp {
        spans.extend(row.temp);
      }
      spans.extend(row.tail);
      buf.set_line(area.x, y, &Line::from(spans), area.width);

      if let (Some(x), Some(items)) = (fit.graph, row.history) {
        let graph_area = Rect::new(area.x + x, y, area.width - x, 1);
        Graph::new(items, &self.theme).color(color).render(graph_area, buf);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Style;

  use super::{
    PowerFit, PowerWidths, TitleSlots, Titles, fit_count, fit_power, format_gb, format_ghz,
    place_titles, temp_ratio,
  };

  const GB: u64 = 1 << 30;

  /// Widths of real power rows: CPU / GPU / ANE `CPU    4.50W` ` (4.50, 4.50)` `  45°C` and a
  /// graph, then Power ` (6.60, 6.60)` and Total ` (12.00, 12.00)` without temperature and graph.
  fn power_widths() -> Vec<PowerWidths> {
    let unit = PowerWidths { head: 12, stats: 13, temp: 6, tail: 0, graph: true };
    let power = PowerWidths { temp: 0, graph: false, ..unit };
    let total = PowerWidths { stats: 15, ..power };
    vec![unit, unit, unit, power, total]
  }

  fn parts(stats: bool, temp: bool, graph: Option<u16>) -> PowerFit {
    PowerFit { stats, temp, graph }
  }

  #[test]
  fn power_parts_drop_graph_then_temp_then_stats() {
    let rows = power_widths();
    let fit = |width| fit_power(&rows, width);

    // 31 cells of text, a gap and at least 12 graph cells
    assert_eq!(fit(200), parts(true, true, Some(32)));
    assert_eq!(fit(44), parts(true, true, Some(32)));
    assert_eq!(fit(43), parts(true, true, None));
    assert_eq!(fit(31), parts(true, true, None));
    assert_eq!(fit(30), parts(true, false, None));
    // the Total numbers are the widest
    assert_eq!(fit(27), parts(true, false, None));
    assert_eq!(fit(26), parts(false, false, None));
    assert_eq!(fit(0), parts(false, false, None));

    // a narrower column never shows more, and parts go in a fixed order
    let shown = |fit: &PowerFit| [fit.stats, fit.temp, fit.graph.is_some()];
    for width in 0..80 {
      let [stats, temp, graph] = shown(&fit(width));
      assert!(stats || !temp, "width {width}: temperature without average / maximum");
      assert!(temp || !graph, "width {width}: graph without temperature");
      let wider = shown(&fit(width + 1));
      assert!([stats, temp, graph].iter().zip(wider).all(|(a, b)| !a || b), "width {width}");
    }
  }

  #[test]
  fn widest_power_row_decides_for_every_row() {
    // 100 W and more: the total is one cell wider, so every row drops average and maximum
    let mut rows = power_widths();
    rows[4].head += 1;
    assert_eq!(fit_power(&rows, 28), parts(true, false, None));
    assert_eq!(fit_power(&rows, 27), parts(false, false, None));
    // graphs start after the widest row with a graph
    assert_eq!(fit_power(&rows, 44), parts(true, true, Some(32)));
    rows[0].head += 1;
    assert_eq!(fit_power(&rows, 44), parts(true, true, None));
    assert_eq!(fit_power(&rows, 45), parts(true, true, Some(33)));

    // no rows with a graph, no rows at all
    assert_eq!(fit_power(&rows[3..], 100), parts(true, true, None));
    assert_eq!(fit_power(&[], 0), parts(true, true, None));
  }

  #[test]
  fn titles_fit_on_wide_border() {
    // left at 2, right ends 2 cells before the box edge
    let slots = place_titles(60, &[10, 8], Some(12));
    assert_eq!(slots.left, vec![(2, 10), (13, 8)]);
    assert_eq!(slots.right, Some(46));
  }

  #[test]
  fn right_title_dropped_instead_of_overlapping_left() {
    let slots = place_titles(40, &[30], Some(20));
    assert_eq!(slots, TitleSlots { left: vec![(2, 30)], right: None });

    // exactly one border cell between them still fits
    let slots = place_titles(40, &[15], Some(20));
    assert_eq!(slots.right, Some(18));
    assert_eq!(2 + 15 + 1, 18);
  }

  #[test]
  fn first_left_title_is_truncated_others_dropped() {
    let slots = place_titles(20, &[30, 4], Some(4));
    assert_eq!(slots, TitleSlots { left: vec![(2, 16)], right: None });

    // a later left title is dropped when it doesn't fit in full
    let slots = place_titles(20, &[10, 8], None);
    assert_eq!(slots.left, vec![(2, 10)]);
  }

  #[test]
  fn titles_return_the_cells_of_their_text() {
    let render = |width: u16| {
      let area = Rect::new(5, 2, width, 1);
      let mut buf = Buffer::empty(Rect::new(0, 0, 60, 4));
      let titles = Titles::new("proc 3").left("/ filter").right("cpu");
      let cells = titles.render(area, &mut buf, Style::new());
      let text = |r: &Rect| (r.left()..r.right()).map(|x| buf[(x, r.y)].symbol()).collect();
      cells.iter().map(|r| (*r, text(r))).collect::<Vec<(Rect, String)>>()
    };

    // `╭─ proc 3 ─ / filter ─…`: the blank cells around the text aren't part of it
    let cells = render(40);
    assert_eq!(cells[0], (Rect::new(8, 2, 6, 1), "proc 3".to_string()));
    assert_eq!(cells[1], (Rect::new(17, 2, 8, 1), "/ filter".to_string()));

    // the second title doesn't fit, the first one is cut
    assert_eq!(render(8), [(Rect::new(8, 2, 3, 1), "pro".to_string())]);
    assert!(render(4).is_empty());
  }

  #[test]
  fn tiny_borders_place_nothing() {
    for width in 0..=4 {
      let slots = place_titles(width, &[5], Some(3));
      assert!(slots.right.is_none(), "width {width}");
      assert!(slots.left.iter().all(|(x, w)| x + w <= width.saturating_sub(2)), "width {width}");
    }
    assert_eq!(place_titles(5, &[5], None).left, vec![(2, 1)]);
  }

  #[test]
  fn titles_never_overlap() {
    let sizes = [0, 1, 3, 6, 9, 14, 20, 33];
    for width in 0..=120 {
      for (a, b) in sizes.iter().flat_map(|a| sizes.map(|b| (*a, b))) {
        for right in [None, Some(a), Some(b)] {
          let slots = place_titles(width, &[a, b], right);
          let mut spans: Vec<(u16, u16)> = slots.left.clone();
          spans.extend(slots.right.zip(right));

          let ctx = format!("width {width} left [{a}, {b}] right {right:?}");
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
  fn strip_details() {
    assert_eq!(format_ghz(1800), "1.8GHz");
    assert_eq!(format_ghz(3228), "3.2GHz");
    assert_eq!(format_ghz(0), "0.0GHz");

    assert_eq!(format_gb(21 * GB), "21");
    assert_eq!(format_gb(4 * GB), "4");
    assert_eq!(format_gb(GB + GB / 5), "1.2");
    assert_eq!(format_gb(GB * 99 / 10), "9.9");
    assert_eq!(format_gb(GB * 1001 / 100), "10");
    assert_eq!(format_gb(0), "0");
  }

  #[test]
  fn temperature_maps_onto_gradient() {
    assert_eq!(temp_ratio(30.0), 0.0);
    assert_eq!(temp_ratio(65.0), 0.5);
    assert_eq!(temp_ratio(100.0), 1.0);
  }
}
