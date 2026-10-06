//! Metrics box (the metric boxes of the original macmon: CPU clusters, GPU, RAM, CPU / GPU / ANE
//! power) and the box frame it shares with the process list: rounded borders, titles fitted on
//! the top border, the power summary and the key hints on the bottom border.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

use super::App;
use super::layout::{LayoutPlan, Metric, compute_layout};
use super::store::{FreqStore, PowerStore};
use super::widgets::{Gauge, Graph};
use crate::config::ViewType;

const GB: f64 = (1u64 << 30) as f64;
/// Between the parts of the power summary and between the key hints on a bottom border.
const SEPARATOR: &str = " | ";

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

  /// Whether every title fits uncut on the top border of a box `width` cells wide.
  fn fits(&self, width: u16) -> bool {
    let padded = |line: &Line| width_u16(line).saturating_add(2);
    let left: Vec<u16> = self.left.iter().map(padded).collect();
    let right = self.right.as_ref().map(padded);
    let slots = place_titles(width, &left, right);
    slots.left.iter().map(|&(_, w)| w).eq(left.iter().copied())
      && slots.right.is_some() == right.is_some()
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

/// The first of `variants` (longest first) whose titles fit uncut on the top border of a box
/// `width` cells wide; when none does, the last one, which `Titles::render` cuts to fit.
fn fit_titles(width: u16, variants: Vec<Titles>) -> Titles {
  let mut variants = variants.into_iter().peekable();
  while let Some(titles) = variants.next() {
    if variants.peek().is_none() || titles.fits(width) {
      return titles;
    }
  }
  Titles::default()
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

/// Cells of `items` (by width) joined by ` | `, with a blank cell at both ends; 0 for none.
fn joined_width(items: &[u16]) -> u16 {
  if items.is_empty() {
    return 0;
  }

  let gaps = (items.len() - 1) * SEPARATOR.len();
  let text = items.iter().map(|&w| usize::from(w)).sum::<usize>() + gaps + 2;
  text.min(usize::from(u16::MAX)) as u16
}

/// Number of leading `items` (by width) that fit in `room` cells joined as by `joined_width`.
fn fit_joined(room: u16, items: &[u16]) -> usize {
  fit_count(room.saturating_sub(2), items, SEPARATOR.len() as u16)
}

/// What fits on a bottom border: power summary parts on the left, key hints on the right.
#[derive(Debug, PartialEq, Eq)]
struct BorderFit {
  /// Summary parts shown; with a too short border, only the first one, cut.
  summary: usize,
  /// Cells taken by the summary.
  summary_width: u16,
  /// Key hints shown.
  hints: usize,
}

/// Shares a bottom border `width` cells wide between the power summary (left) and the key hints
/// (right), both given as item widths. Like titles, they keep a border cell next to the corners
/// and between each other. `q quit` (the first hint) is placed first, then the summary takes the
/// room it needs (parts drop from the end, a first part that doesn't fit is cut), and the hints
/// get the room left (dropped from the end).
fn share_border(width: u16, summary: &[u16], hints: &[u16]) -> BorderFit {
  let avail = width.saturating_sub(4);
  let first_hint = &hints[..hints.len().min(1)];
  let reserved = match fit_joined(avail, first_hint) {
    0 => 0,
    _ => joined_width(first_hint) + 1,
  };

  let room = avail.saturating_sub(reserved);
  let (summary_count, summary_width) = match fit_joined(room, summary) {
    // cut, as long as a character of it shows after the blank cell
    0 if !summary.is_empty() && room >= 2 => (1, room),
    0 => (0, 0),
    count => (count, joined_width(&summary[..count])),
  };

  let used = if summary_width > 0 { summary_width + 1 } else { 0 };
  BorderFit {
    summary: summary_count,
    summary_width,
    hints: fit_joined(avail.saturating_sub(used), hints),
  }
}

/// Titles and graph of one metric box.
struct MetricBox<'a> {
  /// Title variants, longest first; the box shows the first that fits (see `fit_titles`).
  titles: Vec<Titles<'static>>,
  /// History, newest first.
  data: &'a [u64],
  /// Value at full height; `None` scales the graph to its largest visible sample.
  max: Option<u64>,
  /// One color for every bar instead of its load color.
  color: Option<Color>,
  /// Current load (`0.0..=1.0`) for the gauge view (`v`); `None` keeps the graph in both views.
  gauge: Option<f64>,
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

  /// `load` (`0.0..=1.0`) as a percent with `decimals`, in its load color.
  fn percent(&self, load: f64, decimals: usize) -> Span<'static> {
    Span::styled(format!("{:.decimals$}%", load * 100.0), self.theme.gradient(load))
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

  /// Global key hints in the order of the original UI: `q quit`, `p procs`, `v chart`,
  /// `r scaled`, `-/+ 1000ms`; keys bold, labels plain.
  fn key_hints(&self) -> Vec<Vec<Span<'static>>> {
    let hints = [
      ("q", "quit".to_string()),
      ("p", "procs".to_string()),
      ("v", "chart".to_string()),
      ("r", self.cfg.ratio_mode.label().to_string()),
      ("-/+", format!("{}ms", self.cfg.interval)),
    ];
    hints
      .into_iter()
      .map(|(key, label)| vec![self.heading(key), self.text(format!(" {label}"))])
      .collect()
  }

  /// `items` joined by a dim ` | `, with a blank cell at both ends.
  fn joined<'a>(&self, items: impl IntoIterator<Item = Vec<Span<'a>>>) -> Line<'a> {
    let mut spans = vec![Span::raw(" ")];
    for (i, item) in items.into_iter().enumerate() {
      if i > 0 {
        spans.push(self.dim(SEPARATOR));
      }
      spans.extend(item);
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
  }

  /// Draws the `summary` parts (left) and, with `hints`, the global key hints (right-aligned)
  /// over the bottom border of box `area`, sharing it as `share_border` does.
  fn render_bottom_border(
    &self,
    f: &mut Frame,
    area: Rect,
    summary: Vec<Vec<Span<'static>>>,
    hints: bool,
  ) {
    let hints = if hints { self.key_hints() } else { vec![] };
    let widths =
      |items: &[Vec<Span>]| items.iter().map(|item| spans_width(item)).collect::<Vec<_>>();
    let fit = share_border(area.width, &widths(&summary), &widths(&hints));
    let y = area.bottom() - 1;

    if fit.summary > 0 {
      let line = self.joined(summary.into_iter().take(fit.summary));
      f.buffer_mut().set_line(area.x + 2, y, &line, fit.summary_width);
    }

    if fit.hints > 0 {
      let line = self.joined(hints.into_iter().take(fit.hints));
      let width = width_u16(&line);
      f.buffer_mut().set_line(area.right() - 2 - width, y, &line, width);
    }
  }

  /// Draws the global key hints right-aligned over the bottom border of box `area`: `q quit |
  /// p procs | v chart | r scaled | -/+ 1000ms`. Hints that don't fit are dropped from the end, so
  /// `q quit` stays as long as it fits.
  pub(super) fn render_key_hints(&self, f: &mut Frame, area: Rect) {
    self.render_bottom_border(f, area, vec![], true);
  }

  /// Screen layout for the current metrics.
  pub(super) fn layout(&self, area: Rect) -> LayoutPlan {
    compute_layout(area, self.cfg.show_procs, self.clusters.items.len())
  }

  /// Metrics box: chip and version in the title, the metric boxes inside and the power summary on
  /// the bottom border, with the key hints when it is the lowest box.
  pub(super) fn render_metrics_box(&self, f: &mut Frame, plan: &LayoutPlan) {
    let Some(area) = plan.top else { return };
    self.draw_box(f, area, self.metrics_titles());

    for &(metric, r) in &plan.boxes {
      let Some(metric) = self.metric_box(metric) else { continue };
      let (inner, _) = self.draw_box(f, r, fit_titles(r.width, metric.titles));
      match metric.gauge.filter(|_| self.cfg.view_type == ViewType::Gauge) {
        Some(load) => f.render_widget(Gauge::new(load, &self.theme), inner),
        None => {
          let graph = Graph::new(metric.data, &self.theme).max(metric.max).color(metric.color);
          f.render_widget(graph, inner);
        }
      }
    }

    self.render_bottom_border(f, area, self.power_summary(), plan.proc.is_none());
  }

  /// Chip as in the original UI (`Apple M3 Pro (6E+6P+18GPU 36GB)`) left, `macmon vX` right; the
  /// version gives way to the chip.
  fn metrics_titles(&self) -> Titles<'static> {
    let version = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    Titles::new(self.chip_title()).right(self.text(version))
  }

  /// `Apple M3 Pro (6E+6P+18GPU 36GB)`, the details in parentheses dim. Core counts come from the
  /// CPU clusters, so a third tier shows up as `6E+4P+2S`.
  fn chip_title(&self) -> Line<'static> {
    let soc = &self.soc;
    let mut units: Vec<String> =
      self.clusters.items.iter().map(|c| format!("{}{}", c.count, c.label)).collect();
    if soc.gpu_cores > 0 {
      units.push(format!("{}GPU", soc.gpu_cores));
    }

    let mut details = units.join("+");
    if soc.memory_gb > 0 {
      if !details.is_empty() {
        details.push(' ');
      }
      details.push_str(&format!("{}GB", soc.memory_gb));
    }

    let name = if soc.chip_name.is_empty() { env!("CARGO_PKG_NAME") } else { &soc.chip_name };
    let mut spans = vec![self.text(name.to_string())];
    if !details.is_empty() {
      spans.push(self.dim(format!(" ({details})")));
    }
    Line::from(spans)
  }

  /// Power summary of the original UI for the bottom border of the metrics box: `Power: 6.60W
  /// (avg 6.60W, max 6.60W)` (CPU + GPU + ANE), the fans (`Fan 1200 RPM`) and `Total 12.00W
  /// (12.00, 12.00)` (the whole system); the fans and Total only when their sensors exist.
  fn power_summary(&self) -> Vec<Vec<Span<'static>>> {
    let all = &self.all_power;
    let mut parts = vec![vec![
      self.text(format!("Power: {:.2}W", all.top_value)),
      self.dim(format!(" (avg {:.2}W, max {:.2}W)", all.avg_value, all.max_value)),
    ]];

    let fans = self.fans.label();
    if !fans.is_empty() {
      parts.push(vec![self.text(fans)]);
    }

    let sys = &self.sys_power;
    if sys.top_value > 0.0 {
      parts.push(vec![
        self.text(format!("Total {:.2}W", sys.top_value)),
        self.dim(format!(" ({:.2}, {:.2})", sys.avg_value, sys.max_value)),
      ]);
    }

    parts
  }

  fn metric_box(&self, metric: Metric) -> Option<MetricBox<'_>> {
    Some(match metric {
      Metric::Cluster(i) => {
        let cluster = self.clusters.items.get(i)?;
        self.freq_box(format!("{}-CPU", cluster.label), &cluster.freq)
      }
      Metric::Gpu => self.freq_box("GPU".to_string(), &self.igpu_freq),
      Metric::Ram => self.ram_box(),
      Metric::CpuPower => self.power_box("CPU", &self.cpu_power, self.cpu_temp.last()),
      Metric::GpuPower => self.power_box("GPU", &self.gpu_power, self.gpu_temp.last()),
      Metric::AnePower => self.power_box("ANE", &self.ane_power, 0.0),
    })
  }

  /// `E-CPU 42% @ 1800 MHz` (the percent colored by load), or `E-CPU 42%` when the frequency
  /// doesn't fit, over the usage history scaled to 100 %, or a gauge.
  fn freq_box<'a>(&self, label: String, freq: &'a FreqStore) -> MetricBox<'a> {
    let series = freq.ratio(self.cfg.ratio_mode);
    let short = vec![self.heading(label), self.text(" "), self.percent(series.ratio, 0)];
    let mut full = short.clone();
    full.push(self.text(format!(" @ {} MHz", freq.freq_mhz)));

    MetricBox {
      titles: vec![Titles::new(full), Titles::new(short)],
      data: &series.items,
      max: Some(100),
      color: None,
      gauge: Some(series.ratio),
    }
  }

  /// RAM and swap usage over the RAM usage history scaled to the total RAM, or a gauge. The total
  /// RAM is in the chip title, so the title shows what is used, in steps from the longest that
  /// fits, percentages last: `RAM 16.81 GB (70.0%) · SWAP 2.37 / 3.0 GB`, `RAM 16.8G 70% · SWAP
  /// 2.4G 79%`, `RAM 70% · SWAP 79%`, `RAM 70% SW 79%`; without swap only the RAM part.
  fn ram_box(&self) -> MetricBox<'_> {
    let mem = &self.mem;
    let gb = |bytes: u64| bytes as f64 / GB;
    let ram = (gb(mem.ram_usage), ratio(gb(mem.ram_usage), gb(mem.ram_total)));
    let titles: Vec<Vec<Span>> = if mem.swap_total > 0 {
      let swap = (gb(mem.swap_usage), gb(mem.swap_total));
      let load = ratio(swap.0, swap.1);
      (0..4)
        .map(|step| [self.ram_part(step, ram), self.swap_part(step, swap, load)].concat())
        .collect()
    } else {
      (0..3).map(|step| self.ram_part(step, ram)).collect()
    };

    MetricBox {
      titles: titles.into_iter().map(Titles::new).collect(),
      data: &mem.items,
      max: Some(mem.ram_total),
      color: None,
      gauge: Some(ram.1),
    }
  }

  /// RAM part of the RAM box title at `step` for `(used GB, load)`: `RAM 16.81 GB (70.0%)`, `RAM
  /// 16.8G 70%`, then `RAM 70%`.
  fn ram_part(&self, step: usize, (used, load): (f64, f64)) -> Vec<Span<'static>> {
    let mut spans = vec![self.heading("RAM")];
    match step {
      0 => {
        spans.extend([self.text(format!(" {used:.2} GB (")), self.percent(load, 1), self.text(")")])
      }
      1 => spans.extend([self.text(format!(" {used:.1}G ")), self.percent(load, 0)]),
      _ => spans.extend([self.text(" "), self.percent(load, 0)]),
    }
    spans
  }

  /// Swap part of the RAM box title at `step` for `(used GB, total GB)` and `load`, after the
  /// RAM part: ` · SWAP 2.37 / 3.0 GB`, ` · SWAP 2.4G 79%`, ` · SWAP 79%`, then ` SW 79%`.
  fn swap_part(&self, step: usize, (used, total): (f64, f64), load: f64) -> Vec<Span<'static>> {
    let mut spans = match step {
      0..=2 => vec![self.dim(" · "), self.heading("SWAP")],
      _ => vec![self.text(" "), self.heading("SW")],
    };
    match step {
      0 => spans.push(self.text(format!(" {used:.2} / {total:.1} GB"))),
      1 => spans.extend([self.text(format!(" {used:.1}G ")), self.percent(load, 0)]),
      _ => spans.extend([self.text(" "), self.percent(load, 0)]),
    }
    spans
  }

  /// `CPU 4.50W (3.10, 8.20)` (current, average, maximum; original format) and the temperature
  /// (`45°C`) on the right when the sensor exists, over the power history in the low load color,
  /// scaled to its largest visible sample. Narrow boxes drop the temperature, then the average
  /// and maximum. Always a graph, as in the original.
  fn power_box<'a>(&self, label: &'static str, store: &'a PowerStore, temp: f32) -> MetricBox<'a> {
    let short = vec![self.heading(label), self.text(format!(" {:.2}W", store.top_value))];
    let mut full = short.clone();
    full.push(self.dim(format!(" ({:.2}, {:.2})", store.avg_value, store.max_value)));

    let mut titles = vec![];
    if temp > 0.0 {
      let color = self.theme.gradient(temp_ratio(temp));
      titles.push(Titles::new(full.clone()).right(Span::styled(format!("{temp:.0}°C"), color)));
    }
    titles.extend([Titles::new(full), Titles::new(short)]);

    let color = Some(self.theme.gradient(0.0));
    MetricBox { titles, data: &store.items, max: None, color, gauge: None }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Style;

  use super::{
    BorderFit, TitleSlots, Titles, fit_count, fit_titles, joined_width, place_titles, share_border,
    temp_ratio,
  };

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

  /// Top border of a box `width` cells wide with `titles` drawn on it.
  fn border_with(titles: Titles, width: u16) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
    buf.set_string(0, 0, "─".repeat(width.into()), Style::new());
    titles.render(buf.area, &mut buf, Style::new());
    (0..width).map(|x| buf[(x, 0)].symbol()).collect()
  }

  #[test]
  fn titles_fit_only_uncut() {
    // `╭─ cpu ─ 45°C ─╮`: 2 cells before the left title, 1 between, 2 after the right one
    let titles = || Titles::new("cpu").right("45°C");
    assert!(titles().fits(16));
    assert!(!titles().fits(15));
    assert!(Titles::new("cpu").fits(9) && !Titles::new("cpu").fits(8));
    // a second left title must fit whole too
    assert!(Titles::new("proc").left("ab").fits(15) && !Titles::new("proc").left("ab").fits(14));
    assert!(Titles::default().fits(0));
  }

  #[test]
  fn fit_titles_picks_the_longest_variant_that_fits() {
    let (full, short) = ("CPU 4.50W (4.50, 4.50)", "CPU 4.50W");
    let variants = || vec![Titles::new(full).right("45°C"), Titles::new(full), Titles::new(short)];
    let fitted = |width: u16| border_with(fit_titles(width, variants()), width);
    let dashes = |n: usize| "─".repeat(n);

    assert_eq!(fitted(40), format!("── {full} {} 45°C ──", dashes(6)));
    assert_eq!(fitted(35), format!("── {full} ─ 45°C ──"));
    // the right title goes first, then the left one gets shorter
    assert_eq!(fitted(34), format!("── {full} {}", dashes(8)));
    assert_eq!(fitted(28), format!("── {full} ──"));
    assert_eq!(fitted(27), format!("── {short} {}", dashes(14)));
    assert_eq!(fitted(15), format!("── {short} ──"));
    // none fits: the last one is cut
    assert_eq!(fitted(14), "── CPU 4.50W──");
    assert_eq!(fitted(10), "── CPU 4──");
    assert_eq!(border_with(fit_titles(10, vec![]), 10), dashes(10));
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
  fn joined_items_have_separators_and_blank_ends() {
    // ` a | bb `
    assert_eq!(joined_width(&[1, 2]), 8);
    assert_eq!(joined_width(&[6]), 8);
    assert_eq!(joined_width(&[]), 0);
    assert_eq!(joined_width(&[u16::MAX, u16::MAX]), u16::MAX);
  }

  /// Widths of the power summary parts (`Power: …`, `Fan 1200 RPM`, `Total …`) and the key hints
  /// (`q quit`, `p procs`, `v chart`, `r scaled`, `-/+ 1000ms`) of the test metrics.
  const SUMMARY: [u16; 3] = [35, 12, 27];
  const HINTS: [u16; 5] = [6, 7, 7, 8, 10];

  fn fit(summary: usize, summary_width: u16, hints: usize) -> BorderFit {
    BorderFit { summary, summary_width, hints }
  }

  #[test]
  fn bottom_border_shares_summary_and_hints() {
    // everything: ` Power… | Fan… | Total… ` is 82 cells, the hints 52, plus the corners and a
    // border cell between them
    assert_eq!(share_border(139, &SUMMARY, &HINTS), fit(3, 82, 5));
    assert_eq!(share_border(400, &SUMMARY, &HINTS), fit(3, 82, 5));
    // hints drop from the end first, `q quit` stays
    assert_eq!(share_border(138, &SUMMARY, &HINTS), fit(3, 82, 4));
    assert_eq!(share_border(125, &SUMMARY, &HINTS), fit(3, 82, 3));
    assert_eq!(share_border(95, &SUMMARY, &HINTS), fit(3, 82, 1));
    // then the summary parts, the room goes back to the hints
    assert_eq!(share_border(94, &SUMMARY, &HINTS), fit(2, 52, 3));
    assert_eq!(share_border(65, &SUMMARY, &HINTS), fit(2, 52, 1));
    assert_eq!(share_border(64, &SUMMARY, &HINTS), fit(1, 37, 2));
    assert_eq!(share_border(50, &SUMMARY, &HINTS), fit(1, 37, 1));
    // the Power part is cut next to `q quit`
    assert_eq!(share_border(49, &SUMMARY, &HINTS), fit(1, 36, 1));
    assert_eq!(share_border(15, &SUMMARY, &HINTS), fit(1, 2, 1));
    assert_eq!(share_border(14, &SUMMARY, &HINTS), fit(0, 0, 1));
    assert_eq!(share_border(12, &SUMMARY, &HINTS), fit(0, 0, 1));
    // no room for `q quit`: the summary gets the whole border
    assert_eq!(share_border(11, &SUMMARY, &HINTS), fit(1, 7, 0));
    assert_eq!(share_border(5, &SUMMARY, &HINTS), fit(0, 0, 0));
    assert_eq!(share_border(0, &SUMMARY, &HINTS), fit(0, 0, 0));
  }

  #[test]
  fn bottom_border_with_one_side_only() {
    // no hints (the process box is below): the summary takes the whole border
    assert_eq!(share_border(86, &SUMMARY, &[]), fit(3, 82, 0));
    assert_eq!(share_border(85, &SUMMARY, &[]), fit(2, 52, 0));
    assert_eq!(share_border(20, &SUMMARY, &[]), fit(1, 16, 0));

    // no summary (the process box): hints only, as many as fit
    assert_eq!(share_border(200, &[], &HINTS), fit(0, 0, 5));
    assert_eq!(share_border(56, &[], &HINTS), fit(0, 0, 5));
    assert_eq!(share_border(55, &[], &HINTS), fit(0, 0, 4));
    assert_eq!(share_border(12, &[], &HINTS), fit(0, 0, 1));
    assert_eq!(share_border(11, &[], &HINTS), fit(0, 0, 0));
  }

  #[test]
  fn bottom_border_parts_never_overlap() {
    for width in 0..=200 {
      for summary in [&SUMMARY[..], &SUMMARY[..1], &[]] {
        for hints in [&HINTS[..], &[]] {
          let fit = share_border(width, summary, hints);
          let ctx = format!("width {width} {summary:?} {hints:?}: {fit:?}");
          let hints_width = joined_width(&hints[..fit.hints]);
          let gap = u16::from(fit.summary_width > 0 && hints_width > 0);
          let used = u32::from(fit.summary_width) + u32::from(gap) + u32::from(hints_width);
          assert!(used + 4 <= u32::from(width.max(4)), "{ctx}");
          assert!(fit.summary <= summary.len() && fit.hints <= hints.len(), "{ctx}");
          // `q quit` shows whenever it fits on its own
          assert_eq!(fit.hints > 0, !hints.is_empty() && width >= 12, "{ctx}");
        }
      }
    }
  }

  #[test]
  fn temperature_maps_onto_gradient() {
    assert_eq!(temp_ratio(30.0), 0.0);
    assert_eq!(temp_ratio(65.0), 0.5);
    assert_eq!(temp_ratio(100.0), 1.0);
  }
}
