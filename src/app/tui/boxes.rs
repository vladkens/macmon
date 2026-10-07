//! Metrics box (the metric boxes of the original macmon: CPU clusters, GPU, RAM, CPU / GPU / ANE
//! power) and the box frame it shares with the process list: rounded borders, titles fitted on
//! the top border, a text and the key hints on the bottom border.

use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

use super::App;
use super::layout::{LayoutPlan, Metric};
use super::proc_view::{ELLIPSIS, head};
use super::store::{FreqStore, PowerStore, ratio};
use super::theme::{self, dim, gradient, heading, text};
use super::widgets::{Gauge, Graph};
use crate::config::ViewType;

/// Bytes in a GiB, the unit of the RAM box title.
const GIB: f64 = (1u64 << 30) as f64;
/// Between the parts of the power summary and between the key hints on a bottom border.
const SEPARATOR: &str = " | ";
/// Fewest cells worth a text on the left of a bottom border; with less room it is left out.
const BORDER_TEXT_MIN: u16 = 8;

/// Temperature position on the load gradient: 30 °C is cool, 100 °C is hot.
fn temp_ratio(celsius: f32) -> f64 {
  (f64::from(celsius) - 30.0) / 70.0
}

/// `width` cells as `u16`, the type of screen coordinates; saturating, as no text on screen is
/// wider than the screen.
pub(super) fn cells(width: usize) -> u16 {
  u16::try_from(width).unwrap_or(u16::MAX)
}

fn width_u16(line: &Line) -> u16 {
  cells(line.width())
}

fn spans_width(spans: &[Span]) -> u16 {
  cells(spans.iter().map(Span::width).sum())
}

// MARK: Titles

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
  let (left, free) = place_left(width, left);
  let right = right.filter(|&title| title > 0 && free.start.saturating_add(title) <= free.end);
  TitleSlots { left, right: right.map(|title| free.end - title) }
}

/// The left titles of `place_titles`: their `(x, visible width)` and the cells still free after
/// them, `[start, end)`.
fn place_left(width: u16, left: &[u16]) -> (Vec<(u16, u16)>, Range<u16>) {
  let mut slots = vec![];
  let mut free = 2..width.saturating_sub(2);
  for (i, &title) in left.iter().enumerate() {
    let room = free.end.saturating_sub(free.start);
    if room == 0 || (i > 0 && title > room) {
      break;
    }

    let title = title.min(room);
    slots.push((free.start, title));
    free.start = free.start.saturating_add(title).saturating_add(1);
  }
  (slots, free)
}

/// Cells for the text of one more left title after `titles` on the top border of a box `width`
/// cells wide, as `place_titles` places them: the cells still free, without the blank cell on both
/// sides of the text.
pub(super) fn title_text_room(width: u16, titles: &[Line]) -> u16 {
  let padded: Vec<u16> = titles.iter().map(|title| width_u16(title).saturating_add(2)).collect();
  let (_, free) = place_left(width, &padded);
  free.end.saturating_sub(free.start).saturating_sub(2)
}

/// Draws a rounded box with `titles` on the top border. Returns the area inside the borders.
pub(super) fn draw_box(f: &mut Frame, area: Rect, titles: Titles) -> Rect {
  let block = Block::bordered().border_type(BorderType::Rounded).border_style(theme::BORDER);
  let inner = block.inner(area);
  f.render_widget(block, area);
  // title text without a color of its own in the text color, not the border's
  titles.render(area, f.buffer_mut(), Style::new().fg(theme::TEXT));
  inner
}

// MARK: Bottom border

/// A key hint: the key symbol bold, then what it does (`q quit`).
pub(super) fn hint(key: &'static str, label: impl Into<String>) -> Vec<Span<'static>> {
  vec![heading(key), text(format!(" {}", label.into()))]
}

/// `items` joined by a dim ` | `.
fn join(items: impl IntoIterator<Item = Vec<Span<'static>>>) -> Vec<Span<'static>> {
  let mut spans = vec![];
  for (i, item) in items.into_iter().enumerate() {
    if i > 0 {
      spans.push(dim(SEPARATOR));
    }
    spans.extend(item);
  }
  spans
}

/// `spans` with a blank cell at both ends.
fn padded(spans: Vec<Span<'static>>) -> Line<'static> {
  let mut line = vec![Span::raw(" ")];
  line.extend(spans);
  line.push(Span::raw(" "));
  Line::from(line)
}

/// Number of leading `items` (by width) that fit in `room` cells joined by ` | `, with a blank
/// cell at both ends.
fn fit_joined(room: u16, items: &[u16]) -> usize {
  // the blank cells at both ends, then each item after its separator
  let mut used = 2;
  for (i, &item) in items.iter().enumerate() {
    let gap = if i == 0 { 0 } else { SEPARATOR.len() as u32 };
    used += gap + u32::from(item);
    if used > u32::from(room) {
      return i;
    }
  }

  items.len()
}

/// `spans` cut to `max` cells, ending with a dim `…` when cut (right after the last word kept).
fn cut_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
  if spans.iter().map(Span::width).sum::<usize>() <= max {
    return spans;
  }

  let mut room = max.saturating_sub(1);
  let mut cut = vec![];
  for span in spans {
    let kept = head(&span.content, room);
    if kept.len() < span.content.len() {
      cut.push(Span::styled(kept.trim_end().to_string(), span.style));
      break;
    }
    room -= Span::raw(kept).width();
    cut.push(span);
  }
  cut.push(dim(ELLIPSIS));
  cut
}

/// Draws `hints` (right-aligned) and a text (left) over the bottom border of box `area`. Like
/// titles, they keep a border cell next to the corners and between each other. The hints come
/// first and drop from the end when the border is too short, so `q quit` stays as long as it
/// fits. The text gets the cells left: `text` makes it for that many cells (without its blank
/// cells), and it is cut with `…` when longer, or left out with fewer than `BORDER_TEXT_MIN`.
pub(super) fn render_bottom_border(
  f: &mut Frame,
  area: Rect,
  hints: Vec<Vec<Span<'static>>>,
  text: impl FnOnce(usize) -> Vec<Span<'static>>,
) {
  let y = area.bottom() - 1;
  let mut free = area.width.saturating_sub(4);

  let widths: Vec<u16> = hints.iter().map(|hint| spans_width(hint)).collect();
  let shown = fit_joined(free, &widths);
  if shown > 0 {
    let line = padded(join(hints.into_iter().take(shown)));
    let width = width_u16(&line);
    f.buffer_mut().set_line(area.right() - 2 - width, y, &line, width);
    free = free.saturating_sub(width + 1);
  }

  let room = free.saturating_sub(2);
  if room >= BORDER_TEXT_MIN {
    let spans = cut_spans(text(usize::from(room)), usize::from(room));
    if !spans.is_empty() {
      f.buffer_mut().set_line(area.x + 2, y, &padded(spans), room + 2);
    }
  }
}

// MARK: Metric boxes

/// How a metric box draws its history.
enum Scale {
  /// A load (CPU cluster, GPU, RAM): the graph is scaled to `max` and colored by load, and `v`
  /// switches it to a gauge of the current `load` (`0.0..=1.0`).
  Load { max: u64, load: f64 },
  /// Power: the graph is scaled to its largest visible sample, in the low load color, as in the
  /// original; no gauge.
  Power,
}

/// Titles and graph of one metric box.
struct MetricBox<'a> {
  /// Title variants, longest first; the box shows the first that fits (see `fit_titles`).
  titles: Vec<Titles<'static>>,
  /// History, newest first.
  data: &'a [u64],
  scale: Scale,
}

impl App {
  /// `load` (`0.0..=1.0`) as a percent with `decimals`, in its load color.
  fn percent(&self, load: f64, decimals: usize) -> Span<'static> {
    Span::styled(format!("{:.decimals$}%", load * 100.0), gradient(load))
  }

  /// Key hints for the bottom border of the lowest box, keys bold, labels plain: the global keys
  /// in the order of the original UI with the state of the toggles (`q quit | ? help | p procs |
  /// v graph | r scaled | -/+ 1000ms`; no `p procs` while the window is too small for the process
  /// list), or the filter keys while a filter is typed (`Enter keep | Esc clear | ↑↓ select`).
  pub(super) fn footer_hints(&self) -> Vec<Vec<Span<'static>>> {
    if self.proc_view.typing() {
      return vec![hint("Enter", "keep"), hint("Esc", "clear"), hint("↑↓", "select")];
    }

    let mut hints = vec![hint("q", "quit"), hint("?", "help")];
    if self.procs_fit {
      hints.push(hint("p", "procs"));
    }
    hints.extend([
      hint("v", self.cfg.view_type.label()),
      hint("r", self.cfg.ratio_mode.label()),
      hint("-/+", format!("{}ms", self.cfg.interval())),
    ]);
    hints
  }

  /// Metrics box: chip and version in the title, the metric boxes inside and the power summary on
  /// the bottom border, with the key hints when it is the lowest box.
  pub(super) fn render_metrics_box(&self, f: &mut Frame, plan: &LayoutPlan) {
    let Some(area) = plan.top else { return };
    draw_box(f, area, self.metrics_titles());

    for &(metric, r) in &plan.boxes {
      let metric = self.metric_box(metric);
      let inner = draw_box(f, r, fit_titles(r.width, metric.titles));
      let graph = Graph::new(metric.data).three_levels(self.three_level_bars);
      match metric.scale {
        Scale::Load { load, .. } if self.cfg.view_type == ViewType::Gauge => {
          f.render_widget(Gauge::new(load), inner)
        }
        Scale::Load { max, .. } => f.render_widget(graph.max(max), inner),
        Scale::Power => f.render_widget(graph.color(gradient(0.0)), inner),
      }
    }

    let hints = if plan.proc.is_none() { self.footer_hints() } else { vec![] };
    render_bottom_border(f, area, hints, |_| self.power_summary());
  }

  /// Chip as in the original UI (`Apple M3 Pro (6E+6P+18GPU 36GB)`) left, `macmon vX` right; the
  /// version gives way to the chip.
  fn metrics_titles(&self) -> Titles<'static> {
    let version = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    Titles::new(self.chip_title()).right(text(version))
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

    // a chip name the library couldn't read still leaves a title
    let name = if soc.chip_name.is_empty() { env!("CARGO_PKG_NAME") } else { &soc.chip_name };
    let mut spans = vec![text(name.to_string())];
    if !details.is_empty() {
      spans.push(dim(format!(" ({details})")));
    }
    Line::from(spans)
  }

  /// Power summary for the bottom border of the metrics box: `Power 6.60W (6.60, 6.60)` (CPU +
  /// GPU + ANE: current, average, maximum), the fans (`Fan 1200 RPM`) and `Total 12.00W (12.00,
  /// 12.00)` (the whole system). The fans and Total only when their sensors exist.
  fn power_summary(&self) -> Vec<Span<'static>> {
    let power = |name: &str, store: &PowerStore| {
      let stats = format!(" ({:.2}, {:.2})", store.avg_value, store.max_value);
      vec![text(format!("{name} {:.2}W", store.top_value)), dim(stats)]
    };

    let mut parts = vec![power("Power", &self.all_power)];
    let fans = self.fans.label();
    if !fans.is_empty() {
      parts.push(vec![text(fans)]);
    }
    if self.sys_power.top_value > 0.0 {
      parts.push(power("Total", &self.sys_power));
    }
    join(parts)
  }

  /// Titles and history of `metric`; its cluster comes from the same list the layout counts.
  fn metric_box(&self, metric: Metric) -> MetricBox<'_> {
    match metric {
      Metric::Cluster(i) => {
        let cluster = &self.clusters.items[i];
        self.freq_box(format!("{}-CPU", cluster.label), &cluster.freq)
      }
      Metric::Gpu => self.freq_box("GPU".to_string(), &self.igpu_freq),
      Metric::Ram => self.ram_box(),
      Metric::CpuPower => self.power_box("CPU", &self.cpu_power, self.cpu_temp.last()),
      Metric::GpuPower => self.power_box("GPU", &self.gpu_power, self.gpu_temp.last()),
      Metric::AnePower => self.power_box("ANE", &self.ane_power, None),
    }
  }

  /// `E-CPU 42% @ 1800 MHz` (the percent colored by load), or `E-CPU 42%` when the frequency
  /// doesn't fit, over the usage history scaled to 100 %, or a gauge.
  fn freq_box<'a>(&self, label: String, freq: &'a FreqStore) -> MetricBox<'a> {
    let series = freq.ratio(self.cfg.ratio_mode);
    let short = vec![heading(label), text(" "), self.percent(series.ratio, 0)];
    let mut full = short.clone();
    full.push(text(format!(" @ {} MHz", freq.freq_mhz)));

    MetricBox {
      titles: vec![Titles::new(full), Titles::new(short)],
      data: &series.items,
      scale: Scale::Load { max: 100, load: series.ratio },
    }
  }

  /// RAM and swap usage over the RAM usage history scaled to the total RAM, or a gauge. The total
  /// RAM is in the chip title, so the title shows what is used: `RAM 16.81 GB (70.0%) · SWAP 2.37
  /// / 3.0 GB`, or the percentages `RAM 70% · SWAP 79%` / `RAM 70% SW 79%` when that doesn't fit
  /// (without swap only the RAM part). Narrower, the swap part drops whole (`RAM 70%`), then the
  /// title, so no number is ever cut.
  fn ram_box(&self) -> MetricBox<'_> {
    let mem = &self.mem;
    let gib = |bytes: u64| bytes as f64 / GIB;
    let (used, load) = (gib(mem.ram_usage), ratio(gib(mem.ram_usage), gib(mem.ram_total)));
    let full = [heading("RAM"), text(format!(" {used:.2} GB (")), self.percent(load, 1), text(")")];
    let short = [heading("RAM"), text(" "), self.percent(load, 0)];

    let mut titles = vec![];
    if mem.swap_total > 0 {
      let (used, total) = (gib(mem.swap_usage), gib(mem.swap_total));
      let swap = [dim(" · "), heading("SWAP")];
      let full_swap = [text(format!(" {used:.2} / {total:.1} GB"))];
      let short_swap = [text(" "), self.percent(ratio(used, total), 0)];
      titles.push([&full[..], &swap, &full_swap].concat());
      titles.push([&short[..], &swap, &short_swap].concat());
      titles.push([&short[..], &[text(" "), heading("SW")], &short_swap].concat());
    } else {
      titles.push(full.into());
    }
    titles.push(short.into());

    let mut titles: Vec<Titles> = titles.into_iter().map(Titles::new).collect();
    titles.push(Titles::default());
    MetricBox { titles, data: &mem.items, scale: Scale::Load { max: mem.ram_total, load } }
  }

  /// `CPU 4.50W (3.10, 8.20)` (current, average, maximum; original format), or `CPU 4.50W` when
  /// that doesn't fit, with the temperature (`45°C`) on the right when the sensor exists, over the
  /// power history in the low load color, scaled to its largest visible sample. Always a graph,
  /// as in the original.
  fn power_box<'a>(
    &self,
    label: &'static str,
    store: &'a PowerStore,
    temp: Option<f32>,
  ) -> MetricBox<'a> {
    let short = vec![heading(label), text(format!(" {:.2}W", store.top_value))];
    let mut full = short.clone();
    full.push(dim(format!(" ({:.2}, {:.2})", store.avg_value, store.max_value)));

    let titles = [full, short].map(|left| {
      let titles = Titles::new(left);
      match temp {
        Some(temp) => {
          titles.right(Span::styled(format!("{temp:.0}°C"), gradient(temp_ratio(temp))))
        }
        None => titles,
      }
    });

    MetricBox { titles: titles.into(), data: &store.items, scale: Scale::Power }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Style;

  use super::{Titles, fit_titles, hint, place_titles, render_bottom_border};
  use crate::tui::theme::{dim, text};

  /// Top border of a box `width` cells wide with `titles` drawn on it.
  fn border_with(titles: Titles, width: u16) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
    buf.set_string(0, 0, "─".repeat(width.into()), Style::new());
    titles.render(buf.area, &mut buf, Style::new());
    (0..width).map(|x| buf[(x, 0)].symbol()).collect()
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
  fn bottom_border_hints_come_first_then_the_text() {
    let border = |width| {
      let hints = vec![hint("q", "quit"), hint("?", "help")];
      let power = |_: usize| vec![text("Power 6.60W"), dim(" (6.60, 6.60)")];
      let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
      let frame = term.draw(|f| render_bottom_border(f, f.area(), hints, power)).unwrap();
      (0..width).map(|x| frame.buffer[(x, 0)].symbol()).collect::<String>().replace(' ', "_")
    };
    let blank = |cells: usize| "_".repeat(cells);
    let help = "_q_quit_|_?_help_";

    // the hints right-aligned, the text left, a border cell next to the corners
    assert_eq!(border(60), format!("___Power_6.60W_(6.60,_6.60)_{}{help}__", blank(13)));
    // the text is cut with `…` in the room the hints leave, and left out with fewer than 8 cells
    assert_eq!(border(40), format!("___Power_6.60W_(6.…__{help}__"));
    assert_eq!(border(32), format!("___Power_6…__{help}__"));
    assert_eq!(border(31), format!("{}{help}__", blank(12)));
    // hints drop from the end, `q quit` stays as long as it fits
    assert_eq!(border(20), format!("{}_q_quit___", blank(10)));
    assert_eq!(border(12), "___q_quit___");
    assert_eq!(border(11), blank(11));
  }
}
