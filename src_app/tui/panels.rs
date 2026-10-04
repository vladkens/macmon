//! Metrics box (CPU cluster / GPU / RAM / SWAP strips, per-core bars, power column) and the box
//! frame it shares with the process list: rounded borders, titles fitted on the top border and
//! key hints on the bottom border.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::App;
use super::layout::{
  CORE_RUN_GAP, ClusterCores, Content, CoreLine, LayoutPlan, Strip, compute_layout, die_label,
};
use super::store::FreqStore;
use super::widgets::{Meter, core_bar, graph};
use crate::config::{RatioMode, ViewType};

const GB: f64 = (1u64 << 30) as f64;
/// Blank cells between key hints on the bottom border.
const HINT_GAP: u16 = 2;
/// Narrowest detail column of the strips (`1.8GHz`, `20/36G`).
const DETAIL_MIN_WIDTH: usize = 6;
/// Label of the per-core bars, in the strip label column of the first cores line.
const CORES_LABEL: &str = "cores";
/// Strip labels besides the CPU clusters', for the label column width.
const STRIP_LABELS: [&str; 4] = ["GPU", "RAM", "SWAP", CORES_LABEL];

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

  /// Draws the titles over the top border of `area`; unstyled text gets `style`.
  fn render(self, area: Rect, buf: &mut Buffer, style: Style) {
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

    for (line, (x, width)) in left.iter().zip(slots.left) {
      buf.set_line(area.x + x, area.y, line, width);
    }

    if let (Some(line), Some(x)) = (right, slots.right) {
      buf.set_line(area.x + x, area.y, &line, width_u16(&line));
    }
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

/// One row of the power column: text and an optional history graph after it.
struct PowerRow<'a> {
  text: Line<'static>,
  history: Option<&'a [u64]>,
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

  /// Screen layout for the visible panels and the current metrics.
  pub(super) fn layout(&self, area: Rect) -> LayoutPlan {
    let clusters: Vec<ClusterCores> = self
      .clusters
      .items
      .iter()
      .map(|c| ClusterCores { label: &c.label, dies: c.freq.dies() })
      .collect();
    let content = Content {
      clusters: &clusters,
      swap: self.mem.swap_total > 0,
      power_rows: self.power_rows().len() as u16,
      cores_indent: self.cores_indent() as u16,
    };

    compute_layout(area, self.cfg.panels, self.cfg.per_core_view, &content)
  }

  /// Width of the strip label column: the widest strip label.
  fn label_width(&self) -> usize {
    let clusters = self.clusters.items.iter().map(|c| cluster_name(&c.label).chars().count());
    STRIP_LABELS.iter().map(|label| label.len()).chain(clusters).max().unwrap_or(0)
  }

  /// Cells before the bars on a cores line: the label column and the space before the percent,
  /// so the bars start under the digits of `" 42%"`.
  fn cores_indent(&self) -> usize {
    self.label_width() + 2
  }

  /// Metrics box: chip summary, clock and version in the title, strips, cores lines and the power
  /// column inside.
  pub(super) fn render_metrics_box(&self, f: &mut Frame, plan: &LayoutPlan) {
    let Some(area) = plan.top else { return };
    self.draw_box(f, area, self.metrics_titles(area.width));
    let buf = f.buffer_mut();

    let label_width = self.label_width();
    let strips: Vec<(StripData, Rect)> =
      plan.strips.iter().filter_map(|(strip, r)| Some((self.strip_data(*strip)?, *r))).collect();
    let details = strips.iter().map(|(strip, _)| strip.detail.chars().count());
    let detail_width = details.max().unwrap_or(0).max(DETAIL_MIN_WIDTH);
    for (strip, area) in strips {
      self.render_strip(buf, strip, area, label_width, detail_width);
    }

    for (i, (line, area)) in plan.cores.iter().enumerate() {
      self.render_core_line(buf, line, *area, i == 0);
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

  /// Chip summary left (`M3 Pro · 6E+6P · 18GPU · 36GB`), clock and version right. The version
  /// gives way to the chip summary first, the clock stays as long as it fits.
  fn metrics_titles(&self, width: u16) -> Titles<'static> {
    let chip = self.chip_title();
    let clock = chrono::Local::now().format("%H:%M:%S").to_string();
    let full = format!(
      "{clock} · {} v{} · {}ms",
      env!("CARGO_PKG_NAME"),
      env!("CARGO_PKG_VERSION"),
      self.cfg.interval
    );

    let full_width = Line::from(full.as_str()).width() as u16;
    let fits = place_titles(width, &[width_u16(&chip) + 2], Some(full_width + 2)).right.is_some();
    Titles::new(chip).right(self.text(if fits { full } else { clock }))
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
        freq_strip(cluster_name(&cluster.label), &cluster.freq.aggregate, mode)
      }
      Strip::Gpu => freq_strip("GPU".to_string(), &self.igpu_freq, mode),
      Strip::Ram => size_strip("RAM", mem.ram_usage, mem.ram_total),
      Strip::Swap => size_strip("SWAP", mem.swap_usage, mem.swap_total),
    })
  }

  /// One strip: text on its first row, the graph (over all its rows) or meter after the text.
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
      Some(items) => graph(self.cfg.view_type, items, &self.theme).max(100).render(body, buf),
      None => {
        Meter::new(strip.ratio, &self.theme).block_chars(self.block_meters()).render(body, buf)
      }
    }
  }

  /// One line of core bars: `cores  E ▃▅▂▁  P ▇▆█▅`, `D1` prefix on multi-die lines.
  fn render_core_line(&self, buf: &mut Buffer, line: &CoreLine, area: Rect, first: bool) {
    let indent = self.cores_indent();
    let label = if first { CORES_LABEL } else { "" };
    let mut spans = vec![self.heading(format!("{label:<indent$}"))];
    if let Some(die) = line.die {
      spans.push(self.dim(format!("{} ", die_label(die))));
    }

    let mode = self.cfg.ratio_mode;
    for (i, run) in line.runs.iter().enumerate() {
      let Some(cluster) = self.clusters.items.get(run.cluster) else { continue };
      if i > 0 {
        spans.push(Span::raw(" ".repeat(CORE_RUN_GAP)));
      }

      // a wrapped cluster keeps its label cells blank, so the bars line up
      let blank = || " ".repeat(cluster.label.chars().count());
      let label = if run.label { cluster.label.clone() } else { blank() };
      spans.push(self.heading(format!("{label} ")));

      let ratios = cluster.freq.core_ratios(mode);
      for &ratio in ratios.get(run.cores.clone()).unwrap_or_default() {
        spans.push(Span::styled(core_bar(ratio), self.theme.gradient(ratio)));
      }
    }

    buf.set_line(area.x, area.y, &Line::from(spans), area.width);
  }

  /// CPU / GPU / ANE power with temperature and history, SYS power and fans (when available) and
  /// the total with its average and maximum.
  fn power_rows(&self) -> Vec<PowerRow<'_>> {
    let units = [
      ("CPU", &self.cpu_power, self.cpu_temp.last()),
      ("GPU", &self.gpu_power, self.gpu_temp.last()),
      ("ANE", &self.ane_power, 0.0),
    ];

    let mut rows: Vec<PowerRow> = units
      .into_iter()
      .map(|(label, store, temp)| {
        // a missing sensor leaves the cells blank, so the graphs line up
        let temp = if temp > 0.0 { self.temp(temp) } else { Span::raw("     ") };
        let text = Line::from(vec![
          self.heading(format!("{label:<4}")),
          self.text(format!("{:>6.2}W ", store.top_value)),
          temp,
          Span::raw(" "),
        ]);
        PowerRow { text, history: Some(&store.items) }
      })
      .collect();

    rows.extend(self.sys_fans_row().map(|text| PowerRow { text, history: None }));

    let all = &self.all_power;
    let total = Line::from(vec![
      self.heading("all "),
      self.text(format!("{:>6.2}W", all.top_value)),
      self.dim(" avg "),
      self.text(format!("{:.1}", all.avg_value)),
      self.dim(" max "),
      self.text(format!("{:.1}", all.max_value)),
    ]);
    rows.push(PowerRow { text: total, history: None });
    rows
  }

  /// SYS power and fan speeds, `None` when neither sensor is available.
  fn sys_fans_row(&self) -> Option<Line<'static>> {
    let mut spans = vec![];

    let sys = self.sys_power.top_value;
    if sys > 0.0 {
      spans.extend([self.heading("SYS "), self.text(format!("{sys:>6.2}W"))]);
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

  /// Power rows top-down in `area`, history graphs right of the text.
  fn render_power(&self, buf: &mut Buffer, area: Rect) {
    for (row, y) in self.power_rows().into_iter().zip(area.top()..area.bottom()) {
      buf.set_line(area.x, y, &row.text, area.width);

      let width = width_u16(&row.text);
      if let Some(items) = row.history
        && area.width > width
      {
        let graph_area = Rect::new(area.x + width, y, area.width - width, 1);
        graph(self.cfg.view_type, items, &self.theme).render(graph_area, buf);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{TitleSlots, fit_count, format_gb, format_ghz, place_titles, temp_ratio};

  const GB: u64 = 1 << 30;

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
