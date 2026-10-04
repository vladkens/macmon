//! Process panel: a process table with sorting, filtering and a selection that follows its pid.

use std::cmp::Ordering;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::App;
use super::panels::{Titles, ratio};
use crate::config::ProcSort;
use crate::procs::ProcInfo;

/// Narrowest NAME column; other columns are dropped to keep it.
const NAME_MIN_WIDTH: u16 = 8;
/// Process power at the hot end of the load gradient.
const POWER_HOT_W: f64 = 10.0;
/// Cursor shown after the filter text while typing it.
const FILTER_CURSOR: &str = "█";

/// Process table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
  Pid,
  Name,
  User,
  Cpu,
  Mem,
  Power,
  Gpu,
}

/// Columns in display order.
const COLUMNS: [Column; 7] =
  [Column::Pid, Column::Name, Column::User, Column::Cpu, Column::Mem, Column::Power, Column::Gpu];

/// Columns dropped (in this order) when the table is too narrow; PID and NAME always stay.
const DROP_ORDER: [Column; 5] =
  [Column::User, Column::Power, Column::Gpu, Column::Mem, Column::Cpu];

impl Column {
  fn header(self) -> &'static str {
    match self {
      Self::Pid => "PID",
      Self::Name => "NAME",
      Self::User => "USER",
      Self::Cpu => "CPU%",
      Self::Mem => "MEM",
      Self::Power => "POWER",
      Self::Gpu => "GPU%",
    }
  }

  /// Column width; NAME's is its minimum, it takes the space other columns leave.
  fn width(self) -> u16 {
    match self {
      Self::Pid => 5,
      Self::Name => NAME_MIN_WIDTH,
      Self::User => 10,
      Self::Cpu => 6,
      Self::Mem => 6,
      Self::Power => 6,
      Self::Gpu => 5,
    }
  }

  fn right_aligned(self) -> bool {
    !matches!(self, Self::Name | Self::User)
  }

  fn sort(self) -> Option<ProcSort> {
    match self {
      Self::Pid => Some(ProcSort::Pid),
      Self::Name => Some(ProcSort::Name),
      Self::User => None,
      Self::Cpu => Some(ProcSort::Cpu),
      Self::Mem => Some(ProcSort::Mem),
      Self::Power => Some(ProcSort::Power),
      Self::Gpu => Some(ProcSort::Gpu),
    }
  }
}

/// Columns of a table `width` cells wide with their widths, one blank cell between columns.
/// Columns are dropped in `DROP_ORDER` until the rest fit next to a `NAME_MIN_WIDTH` NAME column;
/// NAME takes all the space left and is dropped only when no cell is left for it.
fn fit_columns(width: u16) -> Vec<(Column, u16)> {
  let need = |columns: &[Column]| -> u32 {
    columns.iter().map(|c| u32::from(c.width()) + 1).sum::<u32>().saturating_sub(1)
  };

  let mut columns = COLUMNS.to_vec();
  for column in DROP_ORDER {
    if need(&columns) <= u32::from(width) {
      break;
    }
    columns.retain(|c| *c != column);
  }

  let fixed: u32 =
    columns.iter().filter(|c| **c != Column::Name).map(|c| u32::from(c.width()) + 1).sum();
  let name = u32::from(width).saturating_sub(fixed) as u16;
  columns
    .into_iter()
    .filter_map(|c| match c {
      Column::Name => (name > 0).then_some((c, name)),
      c => Some((c, c.width())),
    })
    .collect()
}

/// Memory size in the most readable unit: `512K`, `64M`, `1.5G`.
fn format_mem(bytes: u64) -> String {
  const KB: f64 = 1024.0;
  let bytes = bytes as f64;
  if bytes >= KB * KB * KB {
    format!("{:.1}G", bytes / (KB * KB * KB))
  } else if bytes >= KB * KB {
    format!("{:.0}M", bytes / (KB * KB))
  } else {
    format!("{:.0}K", bytes / KB)
  }
}

/// Order of `a` and `b` by the value of `sort`, ascending.
fn compare(a: &ProcInfo, b: &ProcInfo, sort: ProcSort) -> Ordering {
  fn lower(p: &ProcInfo) -> impl Iterator<Item = char> + '_ {
    p.name.chars().flat_map(char::to_lowercase)
  }

  match sort {
    ProcSort::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
    ProcSort::Mem => a.mem_bytes.cmp(&b.mem_bytes),
    ProcSort::Power => a.power_w.unwrap_or(0.0).total_cmp(&b.power_w.unwrap_or(0.0)),
    ProcSort::Gpu => a.gpu_pct.total_cmp(&b.gpu_pct),
    ProcSort::Pid => a.pid.cmp(&b.pid),
    ProcSort::Name => lower(a).cmp(lower(b)),
  }
}

/// Sorts by `sort` in the given direction. Processes without a power reading go last when sorted
/// by power, ties are ordered by pid.
fn sort_procs(procs: &mut [ProcInfo], sort: ProcSort, desc: bool) {
  let missing = |p: &ProcInfo| sort == ProcSort::Power && p.power_w.is_none();
  procs.sort_by(|a, b| {
    let ord = compare(a, b, sort);
    let ord = if desc { ord.reverse() } else { ord };
    missing(a).cmp(&missing(b)).then(ord).then(a.pid.cmp(&b.pid))
  });
}

/// Case-insensitive substring match on the name or pid; `filter` is already lowercase.
fn matches(proc: &ProcInfo, filter: &str) -> bool {
  filter.is_empty()
    || proc.name.to_lowercase().contains(filter)
    || proc.pid.to_string().contains(filter)
}

/// First visible row so that row `selected` is on screen, moving as little as possible from
/// `offset`. Without a selection the table shows its top.
fn scroll_offset(offset: usize, selected: Option<usize>, height: usize, len: usize) -> usize {
  let Some(selected) = selected else { return 0 };
  let offset = if selected < offset {
    selected
  } else if selected >= offset + height {
    (selected + 1).saturating_sub(height)
  } else {
    offset
  };

  // no blank rows below the last process when the list shrinks
  offset.min(len.saturating_sub(height))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nav {
  Up,
  Down,
  PageUp,
  PageDown,
  Home,
  End,
}

impl Nav {
  fn from_key(code: KeyCode) -> Option<Self> {
    match code {
      KeyCode::Up => Some(Self::Up),
      KeyCode::Down => Some(Self::Down),
      KeyCode::PageUp => Some(Self::PageUp),
      KeyCode::PageDown => Some(Self::PageDown),
      KeyCode::Home => Some(Self::Home),
      KeyCode::End => Some(Self::End),
      _ => None,
    }
  }
}

/// Selected process: its pid, and its row to fall back on when the process goes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
  pid: i32,
  index: usize,
}

/// State of the process panel.
#[derive(Debug)]
pub struct ProcView {
  pub sort: ProcSort,
  pub sort_desc: bool,
  filter: String,
  /// Filter input mode: keys edit the filter instead of acting as shortcuts.
  typing: bool,
  /// Latest sample, sorted; `None` until the first one arrives.
  procs: Option<Vec<ProcInfo>>,
  /// Indexes of `procs` that pass the filter, in display order.
  rows: Vec<usize>,
  selected: Option<Selection>,
  /// First row on screen.
  offset: usize,
  /// Rows on screen at the last render, the PgUp / PgDn step.
  page: usize,
}

impl Default for ProcView {
  fn default() -> Self {
    Self::new(ProcSort::Cpu, true)
  }
}

impl ProcView {
  pub fn new(sort: ProcSort, sort_desc: bool) -> Self {
    Self {
      sort,
      sort_desc,
      filter: String::new(),
      typing: false,
      procs: None,
      rows: vec![],
      selected: None,
      offset: 0,
      page: 0,
    }
  }

  pub fn procs(&self) -> Option<&[ProcInfo]> {
    self.procs.as_deref()
  }

  /// Processes that pass the filter, in display order.
  pub fn rows(&self) -> impl Iterator<Item = &ProcInfo> {
    let procs = self.procs.as_deref().unwrap_or_default();
    self.rows.iter().map(move |&i| &procs[i])
  }

  pub fn row_count(&self) -> usize {
    self.rows.len()
  }

  pub fn filter(&self) -> &str {
    &self.filter
  }

  pub fn typing(&self) -> bool {
    self.typing
  }

  pub fn selected_pid(&self) -> Option<i32> {
    self.selected.map(|s| s.pid)
  }

  /// Replaces the process list; the selection follows its pid.
  pub fn set_procs(&mut self, procs: Vec<ProcInfo>) {
    self.procs = Some(procs);
    self.refresh();
  }

  /// Drops the list and ends filter input (the panel got hidden). The filter and sort stay.
  pub fn clear(&mut self) {
    self.procs = None;
    self.typing = false;
    self.refresh();
  }

  /// Re-sorts and re-filters the list. The selection stays on its pid; when that process is gone,
  /// the row at the same position (or the last one) is selected instead.
  fn refresh(&mut self) {
    let procs = self.procs.as_deref_mut().unwrap_or_default();
    sort_procs(procs, self.sort, self.sort_desc);

    let filter = self.filter.to_lowercase();
    self.rows = (0..procs.len()).filter(|&i| matches(&procs[i], &filter)).collect();

    let procs = self.procs.as_deref().unwrap_or_default();
    let rows = &self.rows;
    self.selected = self.selected.and_then(|sel| {
      let index = match rows.iter().position(|&i| procs[i].pid == sel.pid) {
        Some(index) => index,
        None => sel.index.min(rows.len().checked_sub(1)?),
      };
      Some(Selection { pid: procs[rows[index]].pid, index })
    });
  }

  /// Fits the scroll position to a table body `height` rows high, keeping the selection visible.
  pub fn fit(&mut self, height: usize) {
    self.page = height;
    self.offset =
      scroll_offset(self.offset, self.selected.map(|s| s.index), height, self.rows.len());
  }

  /// Rows on screen after `fit`, with a flag for the selected one.
  pub fn page_rows(&self) -> impl Iterator<Item = (bool, &ProcInfo)> {
    let selected = self.selected.map(|s| s.index);
    let rows = self.rows().enumerate().skip(self.offset).take(self.page);
    rows.map(move |(i, proc)| (Some(i) == selected, proc))
  }

  fn select(&mut self, index: usize) {
    let procs = self.procs.as_deref().unwrap_or_default();
    self.selected = self.rows.get(index).map(|&i| Selection { pid: procs[i].pid, index });
  }

  /// Moves the selection; without one, it starts above the first row.
  fn navigate(&mut self, nav: Nav) {
    let Some(last) = self.rows.len().checked_sub(1) else { return };
    let page = self.page.max(1);
    let index = match (nav, self.selected.map(|s| s.index)) {
      (Nav::Home, _) | (Nav::Up | Nav::PageUp | Nav::Down, None) => 0,
      (Nav::End, _) => last,
      (Nav::PageDown, None) => page - 1,
      (Nav::Up, Some(i)) => i.saturating_sub(1),
      (Nav::Down, Some(i)) => i + 1,
      (Nav::PageUp, Some(i)) => i.saturating_sub(page),
      (Nav::PageDown, Some(i)) => i + page,
    };
    self.select(index.min(last));
  }

  /// Applies a key press. Returns `false` for keys the panel doesn't use, so they can act as
  /// global shortcuts; while typing a filter every key is used.
  pub fn handle_key(&mut self, key: KeyEvent) -> bool {
    if let Some(nav) = Nav::from_key(key.code) {
      self.navigate(nav);
      return true;
    }

    if self.typing {
      self.type_key(key);
      return true;
    }

    match key.code {
      KeyCode::Char('/') => self.typing = true,
      KeyCode::Char('S') => self.reverse_sort(),
      KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::SHIFT) => self.reverse_sort(),
      KeyCode::Char('s') => {
        self.sort = self.sort.next();
        self.refresh();
      }
      KeyCode::Esc if self.selected.is_some() => self.selected = None,
      KeyCode::Esc if !self.filter.is_empty() => {
        self.filter.clear();
        self.refresh();
      }
      _ => return false,
    }

    true
  }

  fn reverse_sort(&mut self) {
    self.sort_desc = !self.sort_desc;
    self.refresh();
  }

  /// Filter input: characters and Backspace edit, Enter keeps the filter, Esc clears it.
  fn type_key(&mut self, key: KeyEvent) {
    match key.code {
      KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
        self.filter.push(c);
      }
      KeyCode::Backspace => {
        self.filter.pop();
      }
      KeyCode::Enter => {
        self.typing = false;
        return;
      }
      KeyCode::Esc => {
        self.typing = false;
        self.filter.clear();
      }
      _ => return,
    }

    self.refresh();
  }
}

impl App {
  /// Process panel: count, filter and sort in the title, a header row and the process rows.
  pub(super) fn render_proc_box(&mut self, f: &mut Frame, area: Rect) {
    let inner = self.draw_box(f, area, self.proc_titles());
    if self.proc_view.procs().is_none() {
      let row = inner.centered_vertically(Constraint::Length(1));
      f.render_widget(Line::from(self.dim("collecting…")).centered(), row);
      return;
    }

    if inner.is_empty() {
      return;
    }

    let body = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    self.proc_view.fit(body.height as usize);
    // one blank cell between the last column and the right border
    let columns = fit_columns(inner.width.saturating_sub(1));
    let buf = f.buffer_mut();

    let header = columns.iter().map(|&(column, _)| {
      let color =
        if column.sort() == Some(self.proc_view.sort) { self.theme.title } else { self.theme.dim };
      Span::styled(column.header(), Style::new().fg(color).add_modifier(Modifier::BOLD))
    });
    draw_row(buf, Rect { height: 1, ..inner }, &columns, header);

    for (i, (selected, proc)) in self.proc_view.page_rows().enumerate() {
      let row = Rect { y: body.y + i as u16, height: 1, ..body };
      let cells = columns.iter().map(|&(column, _)| self.proc_cell(column, proc));
      draw_row(buf, row, &columns, cells);
      // reverse video in the default colors, so the row reads as one bar
      if selected {
        buf.set_style(row, self.theme.selected);
      }
    }
  }

  /// `proc 412` (`proc 12/412` with a filter), the filter and the sort key.
  fn proc_titles(&self) -> Titles<'static> {
    let view = &self.proc_view;
    let mut name = vec![self.heading("proc")];
    if let Some(procs) = view.procs() {
      let count = if view.filter().is_empty() {
        format!(" {}", procs.len())
      } else {
        format!(" {}/{}", view.row_count(), procs.len())
      };
      name.push(self.text(count));
    }

    let mut titles = Titles::new(name);
    if view.typing() || !view.filter().is_empty() {
      let mut filter = vec![self.heading("/"), self.text(view.filter().to_string())];
      if view.typing() {
        filter.push(Span::styled(FILTER_CURSOR, self.theme.title));
      }
      titles = titles.left(filter);
    }

    let arrow = if view.sort_desc { "↓" } else { "↑" };
    titles.right(self.text(format!("{} {arrow}", view.sort.label())))
  }

  /// Text of one table cell. Load values are colored by the gradient, zeros are dim and missing
  /// values show as a dim `-`.
  fn proc_cell(&self, column: Column, proc: &ProcInfo) -> Span<'static> {
    let load = |value: f64, ratio: f64, text: String| {
      if value > 0.0 { Span::styled(text, self.theme.gradient(ratio)) } else { self.dim(text) }
    };

    match column {
      Column::Pid => self.text(proc.pid.to_string()),
      Column::Name => self.text(proc.name.clone()),
      Column::User => self.text(proc.user.clone()),
      Column::Cpu => {
        let cpu = f64::from(proc.cpu_pct);
        load(cpu, cpu / 100.0, format!("{cpu:.1}"))
      }
      Column::Mem => {
        let mem = proc.mem_bytes as f64;
        load(mem, ratio(mem, self.mem.ram_total as f64), format_mem(proc.mem_bytes))
      }
      Column::Power => match proc.power_w {
        Some(watts) => {
          let watts = f64::from(watts);
          load(watts, watts / POWER_HOT_W, format!("{watts:.2}W"))
        }
        None => self.dim("-"),
      },
      Column::Gpu => {
        let gpu = f64::from(proc.gpu_pct);
        load(gpu, gpu / 100.0, format!("{gpu:.1}"))
      }
    }
  }

  /// Key hints of the process panel over its bottom border, `start` cells from its left edge.
  pub(super) fn render_proc_hints(&self, f: &mut Frame, area: Rect, start: u16) {
    let view = &self.proc_view;
    let mut hints = vec![];
    if view.typing() {
      hints.extend([("enter", "keep"), ("esc", "clear"), ("↑↓", "select")]);
    } else {
      hints.extend([("/", "filter"), ("s", "sort"), ("S", "reverse"), ("↑↓", "select")]);
      if view.selected_pid().is_some() || !view.filter().is_empty() {
        hints.push(("esc", "clear"));
      }
    }

    let hints: Vec<(&str, String)> = hints.into_iter().map(|(k, l)| (k, l.to_string())).collect();
    self.draw_hints(f, area, start, &hints);
  }
}

/// Draws one table row: `cells` in `columns`, numbers right-aligned, clipped to `area`.
fn draw_row<'a>(
  buf: &mut Buffer,
  area: Rect,
  columns: &[(Column, u16)],
  cells: impl Iterator<Item = Span<'a>>,
) {
  let mut x = area.x;
  for (&(column, width), cell) in columns.iter().zip(cells) {
    let room = area.right().saturating_sub(x);
    if room == 0 {
      break;
    }

    let cells = usize::from(width);
    let text = if column.right_aligned() {
      format!("{:>cells$}", cell.content)
    } else {
      cell.content.into_owned()
    };
    buf.set_stringn(x, area.y, text, cells.min(usize::from(room)), cell.style);
    x = x.saturating_add(width).saturating_add(1);
  }
}

#[cfg(test)]
mod tests {
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

  use super::{Column, ProcView, fit_columns, format_mem, scroll_offset};
  use crate::config::ProcSort;
  use crate::procs::ProcInfo;

  const MB: u64 = 1 << 20;

  fn proc(pid: i32, name: &str, cpu: f32, mem_mb: u64, power: Option<f32>, gpu: f32) -> ProcInfo {
    ProcInfo {
      pid,
      ppid: 1,
      name: name.to_string(),
      user: "user".to_string(),
      cpu_pct: cpu,
      mem_bytes: mem_mb * MB,
      power_w: power,
      gpu_pct: gpu,
    }
  }

  fn sample() -> Vec<ProcInfo> {
    vec![
      proc(1, "launchd", 0.5, 20, None, 0.0),
      proc(631, "WindowServer", 25.0, 300, Some(1.5), 40.0),
      proc(2301, "Safari", 12.0, 900, Some(0.8), 5.0),
      proc(4410, "cargo", 80.0, 150, Some(3.2), 0.0),
      proc(77, "safaribookmarksyncagent", 0.0, 10, Some(0.0), 0.0),
    ]
  }

  fn view() -> ProcView {
    let mut view = ProcView::default();
    view.set_procs(sample());
    view
  }

  fn pids(view: &ProcView) -> Vec<i32> {
    view.rows().map(|p| p.pid).collect()
  }

  fn press(view: &mut ProcView, code: KeyCode) -> bool {
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
  }

  fn type_str(view: &mut ProcView, text: &str) {
    for c in text.chars() {
      assert!(press(view, KeyCode::Char(c)), "{c:?} not used");
    }
  }

  #[test]
  fn sorts_by_each_key_and_direction() {
    let cases = [
      (ProcSort::Cpu, [4410, 631, 2301, 1, 77], [77, 1, 2301, 631, 4410]),
      (ProcSort::Mem, [2301, 631, 4410, 1, 77], [77, 1, 4410, 631, 2301]),
      // no power reading goes last in both directions
      (ProcSort::Power, [4410, 631, 2301, 77, 1], [77, 2301, 631, 4410, 1]),
      // ties by pid
      (ProcSort::Gpu, [631, 2301, 1, 77, 4410], [1, 77, 4410, 2301, 631]),
      (ProcSort::Pid, [4410, 2301, 631, 77, 1], [1, 77, 631, 2301, 4410]),
      // case-insensitive
      (ProcSort::Name, [631, 77, 2301, 1, 4410], [4410, 1, 2301, 77, 631]),
    ];

    for (sort, desc, asc) in cases {
      let mut view = ProcView::new(sort, true);
      view.set_procs(sample());
      assert_eq!(pids(&view), desc, "{sort:?} desc");

      let mut view = ProcView::new(sort, false);
      view.set_procs(sample());
      assert_eq!(pids(&view), asc, "{sort:?} asc");
    }
  }

  #[test]
  fn s_cycles_sort_and_shift_s_reverses() {
    use ProcSort::*;
    let mut view = view();
    let mut sorts = vec![view.sort];
    for _ in 0..6 {
      assert!(press(&mut view, KeyCode::Char('s')));
      sorts.push(view.sort);
    }
    assert_eq!(sorts, [Cpu, Mem, Power, Gpu, Pid, Name, Cpu]);
    assert!(view.sort_desc, "s keeps the direction");

    assert!(press(&mut view, KeyCode::Char('S')));
    assert!(!view.sort_desc);
    assert_eq!(pids(&view), [77, 1, 2301, 631, 4410]);

    // shift reported as a modifier
    assert!(view.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SHIFT)));
    assert!(view.sort_desc);
    assert_eq!(view.sort, Cpu);
  }

  #[test]
  fn filter_matches_name_or_pid_ignoring_case() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    assert!(view.typing());

    type_str(&mut view, "SAF");
    assert_eq!(view.filter(), "SAF");
    assert_eq!(pids(&view), [2301, 77]);
    assert_eq!(view.row_count(), 2);
    assert_eq!(view.procs().map(<[_]>::len), Some(5), "the full list is kept");

    // pid substring
    assert!(press(&mut view, KeyCode::Esc));
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "63");
    assert_eq!(pids(&view), [631]);

    // name or pid: "1" is in pids 4410, 631, 2301 and 1
    assert!(press(&mut view, KeyCode::Backspace));
    assert!(press(&mut view, KeyCode::Backspace));
    type_str(&mut view, "1");
    assert_eq!(pids(&view), [4410, 631, 2301, 1]);

    type_str(&mut view, "zz");
    assert_eq!(pids(&view), Vec::<i32>::new());
  }

  #[test]
  fn enter_keeps_filter_esc_clears_it() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "cargo");
    assert!(press(&mut view, KeyCode::Enter));
    assert!(!view.typing());
    assert_eq!(pids(&view), [4410]);

    // the filter survives new samples
    view.set_procs(sample());
    assert_eq!(pids(&view), [4410]);

    assert!(press(&mut view, KeyCode::Char('/')));
    assert_eq!(view.filter(), "cargo", "typing continues the kept filter");
    assert!(press(&mut view, KeyCode::Esc));
    assert!(!view.typing());
    assert_eq!(view.filter(), "");
    assert_eq!(pids(&view).len(), 5);
  }

  #[test]
  fn typing_takes_shortcut_keys_as_text() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "qcsS/1");
    assert_eq!(view.filter(), "qcsS/1");
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Cpu, true));

    // control / alt chords and other keys don't edit the filter, but are still used
    let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    let alt_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
    for key in [ctrl_u, alt_x, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)] {
      assert!(view.handle_key(key));
    }
    assert_eq!(view.filter(), "qcsS/1");

    // shifted letters are text
    assert!(view.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)));
    assert_eq!(view.filter(), "qcsS/1A");
  }

  #[test]
  fn normal_mode_leaves_other_keys_alone() {
    let mut view = view();
    for c in ['q', 'c', 'v', '1', '+'] {
      assert!(!press(&mut view, KeyCode::Char(c)), "{c:?}");
    }
    assert!(!press(&mut view, KeyCode::Esc), "nothing to clear");
    assert!(!press(&mut view, KeyCode::Enter));
    assert_eq!(view.filter(), "");
  }

  #[test]
  fn navigation_moves_and_clamps_selection() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    view.fit(2);
    assert_eq!(view.selected_pid(), None);

    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(4410), "down starts at the top");
    assert!(press(&mut view, KeyCode::Up));
    assert_eq!(view.selected_pid(), Some(4410), "up stops at the top");
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(631));
    assert!(press(&mut view, KeyCode::PageDown));
    assert_eq!(view.selected_pid(), Some(1));
    assert!(press(&mut view, KeyCode::PageDown));
    assert_eq!(view.selected_pid(), Some(77), "page down stops at the end");
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(77));
    assert!(press(&mut view, KeyCode::PageUp));
    assert_eq!(view.selected_pid(), Some(2301));
    assert!(press(&mut view, KeyCode::Home));
    assert_eq!(view.selected_pid(), Some(4410));
    assert!(press(&mut view, KeyCode::End));
    assert_eq!(view.selected_pid(), Some(77));

    // esc clears the selection first, then the filter
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!(view.selected_pid(), None);
    assert!(press(&mut view, KeyCode::PageDown));
    assert_eq!(view.selected_pid(), Some(631), "page down from no selection");
  }

  #[test]
  fn navigation_on_empty_list() {
    let mut view = ProcView::default();
    for code in [KeyCode::Down, KeyCode::End, KeyCode::PageDown] {
      assert!(press(&mut view, code));
      assert_eq!(view.selected_pid(), None);
    }

    view.set_procs(vec![]);
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), None);
  }

  #[test]
  fn esc_clears_filter_without_selection() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf");
    assert!(press(&mut view, KeyCode::Enter));
    assert!(press(&mut view, KeyCode::Down));

    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!((view.selected_pid(), view.filter()), (None, "saf"));
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!(view.filter(), "");
  }

  #[test]
  fn selection_follows_pid_after_resort_and_refresh() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(631));

    assert!(press(&mut view, KeyCode::Char('S'))); // [77, 1, 2301, 631, 4410]
    assert_eq!(view.selected_pid(), Some(631));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(4410), "moves from the new position");
    assert!(press(&mut view, KeyCode::Up));

    // a new sample reorders the list
    let mut procs = sample();
    procs[1].cpu_pct = 0.1; // 631: [77, 631, 1, 2301, 4410]
    view.set_procs(procs);
    assert_eq!(pids(&view), [77, 631, 1, 2301, 4410]);
    assert_eq!(view.selected_pid(), Some(631));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(1));

    // a filter that keeps the selected process
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "d");
    assert_eq!(pids(&view), [631, 1]);
    assert_eq!(view.selected_pid(), Some(1));
  }

  #[test]
  fn selection_clamps_when_list_shrinks() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::End));
    assert_eq!(view.selected_pid(), Some(77));

    // the selected process exits: the last row takes over
    let all = sample();
    view.set_procs(vec![all[1].clone(), all[2].clone(), all[3].clone()]);
    assert_eq!(pids(&view), [4410, 631, 2301]);
    assert_eq!(view.selected_pid(), Some(2301));

    // the selected process exits again: the row at the same position
    assert!(press(&mut view, KeyCode::Up)); // 631, index 1
    view.set_procs(vec![all[2].clone(), all[0].clone(), all[3].clone()]);
    assert_eq!(pids(&view), [4410, 2301, 1]);
    assert_eq!(view.selected_pid(), Some(2301));

    // nothing left to select
    view.set_procs(vec![]);
    assert_eq!(view.selected_pid(), None);
    view.set_procs(sample());
    assert_eq!(view.selected_pid(), None);
  }

  #[test]
  fn hidden_panel_clears_list_and_input() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "sa");

    view.clear();
    assert!(view.procs().is_none());
    assert_eq!(view.row_count(), 0);
    assert_eq!(view.selected_pid(), None);
    assert!(!view.typing());
    assert_eq!(view.filter(), "sa", "the filter stays");
  }

  #[test]
  fn scroll_keeps_selection_visible() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    let page = |view: &ProcView| view.page_rows().map(|(sel, p)| (sel, p.pid)).collect::<Vec<_>>();

    view.fit(2);
    assert_eq!(page(&view), [(false, 4410), (false, 631)]);

    for _ in 0..3 {
      assert!(press(&mut view, KeyCode::Down));
    }
    view.fit(2);
    assert_eq!(view.offset, 1);
    assert_eq!(page(&view), [(false, 631), (true, 2301)]);

    // moving up inside the page doesn't scroll
    assert!(press(&mut view, KeyCode::Up));
    view.fit(2);
    assert_eq!(page(&view), [(true, 631), (false, 2301)]);

    assert!(press(&mut view, KeyCode::End));
    view.fit(2);
    assert_eq!(page(&view), [(false, 1), (true, 77)]);

    assert!(press(&mut view, KeyCode::Home));
    view.fit(2);
    assert_eq!(view.offset, 0);

    // a taller window shows everything
    assert!(press(&mut view, KeyCode::End));
    view.fit(10);
    assert_eq!(view.offset, 0);
    assert_eq!(page(&view).len(), 5);
  }

  #[test]
  fn scroll_offset_cases() {
    // selection above / inside / below the page
    assert_eq!(scroll_offset(5, Some(2), 4, 20), 2);
    assert_eq!(scroll_offset(5, Some(7), 4, 20), 5);
    assert_eq!(scroll_offset(5, Some(12), 4, 20), 9);
    // no selection: back to the top
    assert_eq!(scroll_offset(5, None, 4, 20), 0);
    // shrunk list: no blank rows at the bottom
    assert_eq!(scroll_offset(10, Some(11), 4, 12), 8);
    assert_eq!(scroll_offset(3, Some(1), 10, 5), 0);
    // zero-height table doesn't panic
    assert_eq!(scroll_offset(0, Some(3), 0, 5), 4);
  }

  #[test]
  fn columns_drop_by_priority_at_narrow_widths() {
    use Column::*;
    let names = |width| fit_columns(width).into_iter().map(|(c, _)| c).collect::<Vec<_>>();

    let all = [(Pid, 5), (Name, 156), (User, 10), (Cpu, 6), (Mem, 6), (Power, 6), (Gpu, 5)];
    assert_eq!(fit_columns(200), all);
    // 5 + 8 + 10 + 6 + 6 + 6 + 5 + 6 gaps
    assert_eq!(names(52), [Pid, Name, User, Cpu, Mem, Power, Gpu]);
    assert_eq!(names(51), [Pid, Name, Cpu, Mem, Power, Gpu]);
    assert_eq!(names(41), [Pid, Name, Cpu, Mem, Power, Gpu]);
    assert_eq!(names(40), [Pid, Name, Cpu, Mem, Gpu]);
    assert_eq!(names(34), [Pid, Name, Cpu, Mem, Gpu]);
    assert_eq!(names(33), [Pid, Name, Cpu, Mem]);
    assert_eq!(names(28), [Pid, Name, Cpu, Mem]);
    assert_eq!(names(27), [Pid, Name, Cpu]);
    assert_eq!(names(21), [Pid, Name, Cpu]);
    assert_eq!(names(20), [Pid, Name]);

    // NAME shrinks below its minimum once nothing else can go, then disappears
    assert_eq!(fit_columns(14), [(Pid, 5), (Name, 8)]);
    assert_eq!(fit_columns(10), [(Pid, 5), (Name, 4)]);
    assert_eq!(fit_columns(6), [(Pid, 5)]);
    assert_eq!(fit_columns(0), [(Pid, 5)]);
  }

  #[test]
  fn columns_fill_the_width() {
    for width in 14..300 {
      let columns = fit_columns(width);
      let used: u16 = columns.iter().map(|(_, w)| w + 1).sum::<u16>() - 1;
      assert_eq!(used, width, "width {width}: {columns:?}");
    }
  }

  #[test]
  fn memory_sizes() {
    assert_eq!(format_mem(0), "0K");
    assert_eq!(format_mem(512 << 10), "512K");
    assert_eq!(format_mem(64 << 20), "64M");
    assert_eq!(format_mem(1536 << 20), "1.5G");
    assert_eq!(format_mem(40 << 30), "40.0G");
  }
}
