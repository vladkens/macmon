//! Process panel: a process table with sorting, filtering and a selection that follows its pid,
//! driven by keys and the mouse.

use std::cmp::Ordering;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
  KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::App;
use super::boxes::{Titles, draw_box, hint, render_bottom_border, title_text_room};
use super::store::ratio;
use super::theme::{self, dim, gradient, heading, text};
use crate::config::{Config, ProcSort};
use crate::procs::ProcInfo;

/// Narrowest NAME column; other columns are dropped to keep it.
const NAME_MIN_WIDTH: u16 = 8;
/// Narrowest NAME column next to USER, which is dropped first to keep it.
const NAME_USER_WIDTH: u16 = 16;
/// Process power at the hot end of the load gradient.
const POWER_HOT_W: f64 = 10.0;
/// Cursor shown after the filter text while typing it.
const FILTER_CURSOR: &str = "█";
/// Stands for the cut part of a text too long for its cells.
pub(super) const ELLIPSIS: &str = "…";
/// Cells of the shortest filter title worth showing next to the count: `/…x█`.
const FILTER_MIN_WIDTH: u16 = 4;
/// Rows one wheel step moves the selection and scrolls the table.
const WHEEL_ROWS: usize = 3;

// MARK: Table

/// Columns of the process table in display order: one per sort key, so each header sorts by its
/// own column.
const COLUMNS: [ProcSort; 7] = [
  ProcSort::Pid,
  ProcSort::Name,
  ProcSort::User,
  ProcSort::Cpu,
  ProcSort::Mem,
  ProcSort::Power,
  ProcSort::Gpu,
];

/// Columns dropped (in this order) when the table is too narrow, each while NAME would get fewer
/// cells than the given width; PID, NAME and the sorted column always stay.
const DROP_ORDER: [(ProcSort, u16); 5] = [
  (ProcSort::User, NAME_USER_WIDTH),
  (ProcSort::Power, NAME_MIN_WIDTH),
  (ProcSort::Gpu, NAME_MIN_WIDTH),
  (ProcSort::Mem, NAME_MIN_WIDTH),
  (ProcSort::Cpu, NAME_MIN_WIDTH),
];

/// The table column of each sort key.
impl ProcSort {
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

  /// Column width; NAME's is its minimum, it takes the space other columns leave. Every column
  /// fits its header with the sort arrow (`POWER ↓`), so sorting doesn't move the columns.
  fn width(self) -> u16 {
    match self {
      Self::Pid => 5,
      Self::Name => NAME_MIN_WIDTH,
      Self::User => 10,
      Self::Cpu => 6,
      Self::Mem => 6,
      Self::Power => 7,
      Self::Gpu => 6,
    }
  }

  fn right_aligned(self) -> bool {
    !matches!(self, Self::Name | Self::User)
  }

  /// Direction of a column when it is chosen for sorting: numbers largest first, PIDs and text
  /// from the start.
  fn sorts_desc(self) -> bool {
    !matches!(self, Self::Pid | Self::Name | Self::User)
  }

  /// Next column in the `s` cycle: CPU → MEM → POWER → GPU → PID → NAME → USER → CPU.
  fn next(self) -> Self {
    match self {
      Self::Cpu => Self::Mem,
      Self::Mem => Self::Power,
      Self::Power => Self::Gpu,
      Self::Gpu => Self::Pid,
      Self::Pid => Self::Name,
      Self::Name => Self::User,
      Self::User => Self::Cpu,
    }
  }

  /// Header text, with the sort arrow when the table is sorted by this column: `MEM ↓`.
  fn header_text(self, sort: ProcSort, desc: bool) -> String {
    match (self == sort, desc) {
      (true, true) => format!("{} ↓", self.header()),
      (true, false) => format!("{} ↑", self.header()),
      (false, _) => self.header().to_string(),
    }
  }
}

/// Columns of a table `width` cells wide sorted by `sort`, with their widths, one blank cell
/// between columns. Columns are dropped as `DROP_ORDER` says, never the sorted one; NAME takes all
/// the space left and is dropped only when no cell is left for it.
fn fit_columns(width: u16, sort: ProcSort) -> Vec<(ProcSort, u16)> {
  // NAME's cells next to the other columns, each with its gap
  let name_width = |columns: &[ProcSort]| {
    let fixed = columns.iter().filter(|c| **c != ProcSort::Name).map(|c| u32::from(c.width()) + 1);
    u32::from(width).saturating_sub(fixed.sum())
  };

  let mut columns = COLUMNS.to_vec();
  for (column, min_name) in DROP_ORDER {
    if column != sort && name_width(&columns) < u32::from(min_name) {
      columns.retain(|c| *c != column);
    }
  }

  let name = name_width(&columns) as u16;
  columns
    .into_iter()
    .filter_map(|c| match c {
      ProcSort::Name => (name > 0).then_some((c, name)),
      c => Some((c, c.width())),
    })
    .collect()
}

/// Memory size in the most readable unit: `512K`, `64M`, `1.5G`, `128G`. The unit follows the
/// rounded value, so 1023.9 MiB reads `1.0G`, not `1024M`.
fn format_mem(bytes: u64) -> String {
  /// Bytes in a KiB, and each unit in the next one.
  const KIB: f64 = 1024.0;
  let kib = bytes as f64 / KIB;
  let mib = kib / KIB;
  let gib = mib / KIB;
  // no decimal from `100.0G` on
  if (gib * 10.0).round() >= 1000.0 {
    format!("{gib:.0}G")
  } else if mib.round() >= KIB {
    format!("{gib:.1}G")
  } else if kib.round() >= KIB {
    format!("{mib:.0}M")
  } else {
    format!("{kib:.0}K")
  }
}

/// Order of `a` and `b` by the value of `sort`, ascending.
fn compare(a: &ProcInfo, b: &ProcInfo, sort: ProcSort) -> Ordering {
  fn lower(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
  }

  match sort {
    ProcSort::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
    ProcSort::Mem => a.mem_bytes.cmp(&b.mem_bytes),
    ProcSort::Power => a.power_w.unwrap_or(0.0).total_cmp(&b.power_w.unwrap_or(0.0)),
    ProcSort::Gpu => a.gpu_pct.total_cmp(&b.gpu_pct),
    ProcSort::Pid => a.pid.cmp(&b.pid),
    ProcSort::Name => lower(&a.name).cmp(lower(&b.name)),
    ProcSort::User => lower(&a.user).cmp(lower(&b.user)),
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
/// `offset`.
fn scroll_offset(offset: usize, selected: Option<usize>, height: usize, len: usize) -> usize {
  let offset = match selected {
    Some(selected) if selected < offset => selected,
    Some(selected) if selected >= offset + height => (selected + 1).saturating_sub(height),
    _ => offset,
  };

  // no blank rows below the last process when the list shrinks
  offset.min(len.saturating_sub(height))
}

/// `text` cut to `max` cells, ending with `…` when cut.
fn cut_end(text: &str, max: usize) -> String {
  match max {
    _ if Span::raw(text).width() <= max => text.to_string(),
    0 => String::new(),
    _ => format!("{}{ELLIPSIS}", head(text, max - 1)),
  }
}

/// `text` cut to its last `max` cells, starting with `…` when cut.
fn cut_start(text: &str, max: usize) -> String {
  match max {
    _ if Span::raw(text).width() <= max => text.to_string(),
    0 => String::new(),
    _ => format!("{ELLIPSIS}{}", tail(text, max - 1)),
  }
}

/// The longest start of `text` (whole characters) at most `max` cells wide.
pub(super) fn head(text: &str, max: usize) -> &str {
  let mut end = 0;
  for (i, c) in text.char_indices() {
    let next = i + c.len_utf8();
    if Span::raw(&text[..next]).width() > max {
      break;
    }
    end = next;
  }
  &text[..end]
}

/// The longest end of `text` (whole characters) at most `max` cells wide.
fn tail(text: &str, max: usize) -> &str {
  let mut start = text.len();
  for (i, _) in text.char_indices().rev() {
    if Span::raw(&text[i..]).width() > max {
      break;
    }
    start = i;
  }
  &text[start..]
}

// MARK: State

/// Selected process: its pid and its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
  pid: i32,
  index: usize,
}

/// Cells of the process box at the last render that react to the mouse. Empty until the box is
/// rendered, and again once it is hidden.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Targets {
  /// The whole box, borders included: the wheel works anywhere over it.
  area: Rect,
  /// Header cells of each column on screen.
  headers: Vec<(ProcSort, Rect)>,
  /// Table rows below the header, the first one showing row `offset`.
  body: Rect,
}

/// State of the process panel.
#[derive(Debug)]
pub(super) struct ProcView {
  pub(super) sort: ProcSort,
  pub(super) sort_desc: bool,
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
  targets: Targets,
}

/// The sort of the default settings.
impl Default for ProcView {
  fn default() -> Self {
    let cfg = Config::default();
    Self::new(cfg.proc_sort, cfg.proc_sort_desc)
  }
}

impl ProcView {
  pub(super) fn new(sort: ProcSort, sort_desc: bool) -> Self {
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
      targets: Targets::default(),
    }
  }

  pub(super) fn procs(&self) -> Option<&[ProcInfo]> {
    self.procs.as_deref()
  }

  /// Processes that pass the filter, in display order.
  pub(super) fn rows(&self) -> impl Iterator<Item = &ProcInfo> {
    let procs = self.procs.as_deref().unwrap_or_default();
    self.rows.iter().map(move |&i| &procs[i])
  }

  pub(super) fn row_count(&self) -> usize {
    self.rows.len()
  }

  pub(super) fn filter(&self) -> &str {
    &self.filter
  }

  pub(super) fn typing(&self) -> bool {
    self.typing
  }

  #[cfg(test)]
  pub(super) fn selected_pid(&self) -> Option<i32> {
    self.selected.map(|s| s.pid)
  }

  /// The selected process.
  pub(super) fn selected(&self) -> Option<&ProcInfo> {
    self.rows().nth(self.selected?.index)
  }

  /// Columns on screen at the last render; all of them before the first one.
  fn visible_columns(&self) -> Vec<ProcSort> {
    match self.targets.headers.as_slice() {
      [] => COLUMNS.to_vec(),
      headers => headers.iter().map(|&(column, _)| column).collect(),
    }
  }

  /// Replaces the process list; the selection follows its pid.
  pub(super) fn set_procs(&mut self, procs: Vec<ProcInfo>) {
    self.procs = Some(procs);
    self.refresh();
  }

  /// Drops the list, ends filter input and forgets the mouse targets (the panel got hidden). The
  /// filter and sort stay.
  pub(super) fn clear(&mut self) {
    self.procs = None;
    self.typing = false;
    self.targets = Targets::default();
    self.refresh();
  }

  /// Re-sorts and re-filters the list. The selection stays on its pid, and is dropped when that
  /// process is gone or filtered out.
  fn refresh(&mut self) {
    let procs = self.procs.as_deref_mut().unwrap_or_default();
    sort_procs(procs, self.sort, self.sort_desc);

    let filter = self.filter.to_lowercase();
    self.rows = (0..procs.len()).filter(|&i| matches(&procs[i], &filter)).collect();

    let procs = self.procs.as_deref().unwrap_or_default();
    let rows = &self.rows;
    self.selected = self.selected.and_then(|sel| {
      let index = rows.iter().position(|&i| procs[i].pid == sel.pid)?;
      Some(Selection { index, ..sel })
    });
  }

  /// Fits the scroll position to a table body `height` rows high, keeping the selection visible
  /// and no blank rows below the last process.
  pub(super) fn fit(&mut self, height: usize) {
    self.page = height;
    self.offset =
      scroll_offset(self.offset, self.selected.map(|s| s.index), height, self.rows.len());
  }

  /// Rows on screen after `fit`, with a flag for the selected one.
  pub(super) fn page_rows(&self) -> impl Iterator<Item = (bool, &ProcInfo)> {
    let selected = self.selected.map(|s| s.index);
    let rows = self.rows().enumerate().skip(self.offset).take(self.page);
    rows.map(move |(i, proc)| (Some(i) == selected, proc))
  }

  fn select(&mut self, index: usize) {
    let procs = self.procs.as_deref().unwrap_or_default();
    self.selected = self.rows.get(index).map(|&i| Selection { pid: procs[i].pid, index });
  }

  /// Scrolls the table to show row `offset` first, without blank rows below the last process.
  fn scroll_to(&mut self, offset: usize) {
    self.offset = offset.min(self.rows.len().saturating_sub(self.page));
  }

  /// Moves the selection for a navigation key (↑ / ↓, PgUp / PgDn, Home / End). Without a
  /// selection ↑ / ↓ select the top row on screen and the others scroll the table. Returns
  /// `false` for other keys.
  fn navigate(&mut self, code: KeyCode) -> bool {
    let page = self.page.max(1);
    let Some(selected) = self.selected.map(|s| s.index) else {
      match code {
        KeyCode::Up | KeyCode::Down => self.select(self.offset),
        KeyCode::PageUp => self.scroll_to(self.offset.saturating_sub(page)),
        KeyCode::PageDown => self.scroll_to(self.offset.saturating_add(page)),
        KeyCode::Home => self.scroll_to(0),
        KeyCode::End => self.scroll_to(usize::MAX),
        _ => return false,
      }
      return true;
    };

    let index = match code {
      KeyCode::Home => 0,
      KeyCode::End => usize::MAX,
      KeyCode::Up => selected.saturating_sub(1),
      KeyCode::Down => selected + 1,
      KeyCode::PageUp => selected.saturating_sub(page),
      KeyCode::PageDown => selected + page,
      _ => return false,
    };

    if let Some(last) = self.rows.len().checked_sub(1) {
      self.select(index.min(last));
    }
    true
  }

  /// Applies a key press. Returns `false` for keys the panel doesn't use, so they can act as
  /// global shortcuts; while typing a filter every key is used.
  pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
    if self.navigate(key.code) {
      return true;
    }

    match key.code {
      _ if self.typing => self.type_key(key),
      KeyCode::Char('/') => self.typing = true,
      KeyCode::Char('S') => self.reverse_sort(),
      KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::SHIFT) => self.reverse_sort(),
      KeyCode::Char('s') => self.next_sort(),
      KeyCode::Esc if self.selected.is_some() || !self.filter.is_empty() => {
        self.selected = None;
        self.filter.clear();
        self.offset = 0;
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

  /// Sorts by `sort` in its own direction (`ProcSort::sorts_desc`); the current sort key reverses
  /// instead.
  fn sort_by(&mut self, sort: ProcSort) {
    if self.sort == sort {
      self.sort_desc = !self.sort_desc;
    } else {
      self.sort = sort;
      self.sort_desc = sort.sorts_desc();
    }
    self.refresh();
  }

  /// `s`: the next column on screen in the `ProcSort::next` cycle.
  fn next_sort(&mut self) {
    let visible = self.visible_columns();
    let mut next = self.sort.next();
    while !visible.contains(&next) && next != self.sort {
      next = next.next();
    }
    if next != self.sort {
      self.sort_by(next);
    }
  }

  /// Applies a mouse event to the cells of the last render: a click on a column header sorts by
  /// it (again: reverses), on a process selects it (on the selected one clears the selection);
  /// the wheel over the box moves the selection `WHEEL_ROWS` rows, or scrolls without one.
  /// Anything else is ignored.
  pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
    let at = Position::new(mouse.column, mouse.row);
    let over_box = self.targets.area.contains(at);
    match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => self.click(at),
      MouseEventKind::ScrollUp if over_box => self.wheel(-(WHEEL_ROWS as isize)),
      MouseEventKind::ScrollDown if over_box => self.wheel(WHEEL_ROWS as isize),
      _ => {}
    }
  }

  fn click(&mut self, at: Position) {
    let targets = &self.targets;
    if let Some(&(column, _)) = targets.headers.iter().find(|(_, cells)| cells.contains(at)) {
      self.sort_by(column);
    } else if targets.body.contains(at) {
      // blank rows below the last process select nothing
      let index = self.offset + usize::from(at.y - targets.body.y);
      if self.selected.is_some_and(|s| s.index == index) {
        self.selected = None;
      } else if index < self.rows.len() {
        self.select(index);
      }
    }
  }

  /// Moves the selection `rows` rows (negative: up) and scrolls the table as much, so the
  /// selected row keeps its place on screen until the table hits its top or end. Without a
  /// selection it only scrolls.
  fn wheel(&mut self, rows: isize) {
    let Some(from) = self.selected.map(|s| s.index) else {
      self.scroll_to(self.offset.saturating_add_signed(rows));
      return;
    };
    let Some(last) = self.rows.len().checked_sub(1) else { return };
    let index = from.saturating_add_signed(rows).min(last);
    self.offset = (self.offset + index).saturating_sub(from);
    self.select(index);
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

  /// Titles of a process box `width` cells wide: `proc 412` (`proc 12/412` with a filter), then
  /// `/ filter`, or the filter once there is one or it is being typed, then `s sort` (not while
  /// typing: `s` is text then). A filter too long for the border shows its end (`/…ari█`) and
  /// leaves out the sort hint; with no room for that next to the count, the filter takes the
  /// count's place.
  fn titles(&self, width: u16) -> Titles<'static> {
    let mut name = vec![heading("proc")];
    if let Some(procs) = self.procs() {
      let count = if self.filter.is_empty() {
        format!(" {}", procs.len())
      } else {
        format!(" {}/{}", self.row_count(), procs.len())
      };
      name.push(text(count));
    }

    let sort = (!self.typing).then(|| hint("s", "sort"));
    let with_sort = |titles: Titles<'static>| match sort {
      Some(sort) => titles.left(sort),
      None => titles,
    };
    if !self.typing && self.filter.is_empty() {
      return with_sort(Titles::new(name).left(hint("/", "filter")));
    }

    // the filter's text after the count, or alone on the border
    let beside = title_text_room(width, &[Line::from(name.clone())]);
    if beside >= FILTER_MIN_WIDTH {
      with_sort(Titles::new(name).left(self.filter_title(beside)))
    } else {
      with_sort(Titles::new(self.filter_title(title_text_room(width, &[]))))
    }
  }

  /// `/saf` (with a cursor while typing) in `room` cells: a filter too long keeps its end, after
  /// `…`.
  fn filter_title(&self, room: u16) -> Vec<Span<'static>> {
    // `/` and the cursor (one cell each) around the filter text
    let text_room = usize::from(room).saturating_sub(1 + usize::from(self.typing));

    let mut spans = vec![heading("/")];
    if Span::raw(&self.filter).width() <= text_room {
      spans.push(text(self.filter.clone()));
    } else {
      spans.push(dim(ELLIPSIS));
      spans.push(text(tail(&self.filter, text_room.saturating_sub(1)).to_string()));
    }
    if self.typing {
      spans.push(Span::styled(FILTER_CURSOR, theme::TEXT));
    }
    spans
  }

  /// Left side of the bottom border of the process box in `room` cells: the selected process
  /// (`631 /System/…/WindowServer`, its path cut from the left; the name when the path isn't
  /// readable). Without a selection, a dim note that POWER is known for own processes only, while
  /// the POWER column shows processes without it next to some with it (all have it as root).
  fn border_text(&self, room: usize, power_shown: bool) -> Vec<Span<'static>> {
    if let Some(proc) = self.selected() {
      let pid = proc.pid.to_string();
      let path = if proc.path.is_empty() { &proc.name } else { &proc.path };
      let path = cut_start(path, room.saturating_sub(pid.len() + 1));
      return vec![heading(pid), text(" "), text(path)];
    }

    let procs = self.procs().unwrap_or_default();
    let power = |known: bool| procs.iter().any(|p| p.power_w.is_some() == known);
    if power_shown && power(true) && power(false) {
      vec![dim("POWER: own processes only")]
    } else {
      vec![]
    }
  }
}

// MARK: Render

impl App {
  /// Process panel: count, filter and sort hint in the title, a header row with the sort arrow,
  /// the process rows, and the selected process and the key hints on the bottom border. Keeps the
  /// cells that react to the mouse for `ProcView::handle_mouse`.
  pub(super) fn render_proc_box(&mut self, f: &mut Frame, area: Rect) {
    let inner = draw_box(f, area, self.proc_view.titles(area.width));
    let (headers, body) = self.render_proc_table(f, inner);
    let power_shown = headers.iter().any(|&(column, _)| column == ProcSort::Power);
    self.proc_view.targets = Targets { area, headers, body };

    let view = &self.proc_view;
    render_bottom_border(f, area, self.footer_hints(), |room| view.border_text(room, power_shown));
  }

  /// Header row and process rows in `inner`, or "collecting…" until the first sample. Returns the
  /// header cells of the columns and the area of the process rows.
  fn render_proc_table(&mut self, f: &mut Frame, inner: Rect) -> (Vec<(ProcSort, Rect)>, Rect) {
    if self.proc_view.procs().is_none() {
      let row = inner.centered_vertically(Constraint::Length(1));
      f.render_widget(Line::from(dim("collecting…")).centered(), row);
      return Default::default();
    }

    if inner.is_empty() {
      return Default::default();
    }

    let body = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    self.proc_view.fit(body.height as usize);
    // one blank cell between the columns and each border, as in the metrics box
    let table = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let columns = fit_columns(table.width, self.proc_view.sort);

    let filter = self.proc_view.filter();
    if self.proc_view.row_count() == 0 && !filter.is_empty() {
      let row = body.centered_vertically(Constraint::Length(1));
      let message = format!("no process matches \"{filter}\"");
      f.render_widget(Line::from(dim(message)).centered(), row);
    }
    let buf = f.buffer_mut();

    let (sort, desc) = (self.proc_view.sort, self.proc_view.sort_desc);
    let header = columns.iter().map(|&(column, _)| {
      let color = if column == sort { theme::TEXT } else { theme::DIM };
      let style = Style::new().fg(color).add_modifier(Modifier::BOLD);
      Span::styled(column.header_text(sort, desc), style)
    });
    let header_row = Rect { height: 1, ..table };
    draw_row(buf, header_row, &columns, header);

    for (i, (selected, proc)) in self.proc_view.page_rows().enumerate() {
      let y = body.y + i as u16;
      let cells = columns.iter().map(|&(column, _)| self.proc_cell(column, proc));
      draw_row(buf, Rect { y, height: 1, ..table }, &columns, cells);
      // from border to border, so the row reads as one bar
      if selected {
        buf.set_style(Rect { y, height: 1, ..inner }, theme::SELECTED);
      }
    }

    (column_areas(header_row, &columns), body)
  }

  /// Text of one table cell. Load values are colored by the gradient, zeros are dim and missing
  /// values show as a dim `-`.
  fn proc_cell(&self, column: ProcSort, proc: &ProcInfo) -> Span<'static> {
    let load = |value: f64, ratio: f64, text: String| {
      if value > 0.0 { Span::styled(text, gradient(ratio)) } else { dim(text) }
    };

    match column {
      ProcSort::Pid => text(proc.pid.to_string()),
      ProcSort::Name => text(proc.name.clone()),
      ProcSort::User => text(proc.user.clone()),
      ProcSort::Cpu => {
        let cpu = f64::from(proc.cpu_pct);
        load(cpu, cpu / 100.0, format!("{cpu:.1}"))
      }
      ProcSort::Mem => {
        let mem = proc.mem_bytes as f64;
        load(mem, ratio(mem, self.mem.ram_total as f64), format_mem(proc.mem_bytes))
      }
      ProcSort::Power => match proc.power_w {
        Some(watts) => {
          let watts = f64::from(watts);
          load(watts, watts / POWER_HOT_W, format!("{watts:.2}W"))
        }
        None => dim("-"),
      },
      ProcSort::Gpu => {
        let gpu = f64::from(proc.gpu_pct);
        load(gpu, gpu / 100.0, format!("{gpu:.1}"))
      }
    }
  }
}

/// Cells of each column in the one-row `area`, one blank cell between columns; columns are cut
/// at the edge of `area`, those past it are left out.
fn column_areas(area: Rect, columns: &[(ProcSort, u16)]) -> Vec<(ProcSort, Rect)> {
  let mut x = area.x;
  let mut areas = vec![];
  for &(column, width) in columns {
    let width = width.min(area.right().saturating_sub(x));
    if width == 0 {
      break;
    }

    areas.push((column, Rect { x, width, ..area }));
    x = x.saturating_add(width).saturating_add(1);
  }
  areas
}

/// Draws one table row: `cells` in `columns`, numbers right-aligned, text cut with `…`, clipped
/// to `area`.
fn draw_row<'a>(
  buf: &mut Buffer,
  area: Rect,
  columns: &[(ProcSort, u16)],
  cells: impl Iterator<Item = Span<'a>>,
) {
  let areas = column_areas(area, columns);
  for ((&(column, width), (_, cell_area)), cell) in columns.iter().zip(areas).zip(cells) {
    let width = usize::from(width);
    let text = if column.right_aligned() {
      format!("{:>width$}", cell.content)
    } else {
      cut_end(&cell.content, usize::from(cell_area.width))
    };
    buf.set_stringn(cell_area.x, cell_area.y, text, usize::from(cell_area.width), cell.style);
  }
}

#[cfg(test)]
mod tests {
  use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
  };
  use ratatui::layout::Rect;

  use super::{
    COLUMNS, ProcView, Targets, column_areas, cut_end, cut_start, fit_columns, format_mem,
    scroll_offset, tail,
  };
  use crate::config::ProcSort;
  use crate::procs::ProcInfo;

  const MIB: u64 = 1 << 20;

  fn proc(pid: i32, name: &str, cpu: f32, mem_mb: u64, power: Option<f32>, gpu: f32) -> ProcInfo {
    ProcInfo {
      pid,
      name: name.to_string(),
      path: format!("/usr/bin/{name}"),
      user: "user".to_string(),
      cpu_pct: cpu,
      mem_bytes: mem_mb * MIB,
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
  fn sorts_by_user_ignoring_case() {
    let mut procs = sample();
    for (proc, user) in procs.iter_mut().zip(["root", "Vlad", "_spotlight", "vlad", "root"]) {
      proc.user = user.to_string();
    }

    // `_` before letters; ties by pid in both directions
    let mut view = ProcView::new(ProcSort::User, false);
    view.set_procs(procs.clone());
    assert_eq!(pids(&view), [2301, 1, 77, 631, 4410]);
    let mut view = ProcView::new(ProcSort::User, true);
    view.set_procs(procs);
    assert_eq!(pids(&view), [631, 4410, 1, 77, 2301]);
  }

  #[test]
  fn s_cycles_sort_and_shift_s_reverses() {
    use ProcSort::*;
    let mut view = view();
    let mut sorts = vec![(view.sort, view.sort_desc)];
    for _ in 0..7 {
      assert!(press(&mut view, KeyCode::Char('s')));
      sorts.push((view.sort, view.sort_desc));
    }
    // each column in its own direction: numbers largest first, PIDs and text from the start
    let expected = [
      (Cpu, true),
      (Mem, true),
      (Power, true),
      (Gpu, true),
      (Pid, false),
      (Name, false),
      (User, false),
      (Cpu, true),
    ];
    assert_eq!(sorts, expected);

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

    // without a selection the paging keys scroll, ↑ / ↓ select the top row on screen
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!(view.selected_pid(), None);
    for (code, offset) in
      [(KeyCode::PageDown, 2), (KeyCode::PageUp, 0), (KeyCode::End, 3), (KeyCode::Home, 0)]
    {
      assert!(press(&mut view, code));
      assert_eq!((view.selected_pid(), view.offset), (None, offset), "{code:?}");
    }
    assert!(press(&mut view, KeyCode::PageDown));
    assert!(press(&mut view, KeyCode::Up));
    assert_eq!((view.selected_pid(), view.offset), (Some(2301), 2), "the top row on screen");
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
  fn esc_clears_selection_and_filter() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf");
    assert!(press(&mut view, KeyCode::Enter));
    assert!(press(&mut view, KeyCode::Down));

    // both at once, and the table back at its top
    view.fit(1);
    assert!(press(&mut view, KeyCode::End));
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!((view.selected_pid(), view.filter(), view.offset), (None, "", 0));
    assert_eq!(pids(&view).len(), 5);
    assert!(!press(&mut view, KeyCode::Esc), "nothing left to clear");

    // either one alone
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!(view.selected_pid(), None);
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "x");
    assert!(press(&mut view, KeyCode::Enter));
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
  fn selection_is_dropped_when_its_process_exits() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(631));

    // other processes come and go: the selection stays on its process, at its new row
    let all = sample();
    view.set_procs(vec![all[1].clone(), all[3].clone()]);
    assert_eq!((pids(&view), view.selected_pid()), (vec![4410, 631], Some(631)));
    assert_eq!(view.selected().map(|p| p.name.as_str()), Some("WindowServer"));

    // the selected process exits: no neighbour takes over, and it isn't back with its pid
    view.set_procs(vec![all[2].clone(), all[3].clone()]);
    assert_eq!((view.selected_pid(), view.selected()), (None, None));
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
    // no selection: where it is, without blank rows at the bottom
    assert_eq!(scroll_offset(5, None, 4, 20), 5);
    assert_eq!(scroll_offset(18, None, 4, 20), 16);
    // shrunk list: no blank rows at the bottom
    assert_eq!(scroll_offset(10, Some(11), 4, 12), 8);
    assert_eq!(scroll_offset(3, Some(1), 10, 5), 0);
    // zero-height table doesn't panic
    assert_eq!(scroll_offset(0, Some(3), 0, 5), 4);
  }

  #[test]
  fn columns_drop_by_priority_at_narrow_widths() {
    use ProcSort::*;
    // sorted by PID, which always stays anyway
    let names = |width| fit_columns(width, Pid).into_iter().map(|(c, _)| c).collect::<Vec<_>>();

    let all = [(Pid, 5), (Name, 154), (User, 10), (Cpu, 6), (Mem, 6), (Power, 7), (Gpu, 6)];
    assert_eq!(fit_columns(200, Pid), all);
    // USER goes before NAME gets fewer than 16 cells: 5 + 16 + 10 + 6 + 6 + 7 + 6 + 6 gaps
    assert_eq!(names(62), [Pid, Name, User, Cpu, Mem, Power, Gpu]);
    assert_eq!(names(61), [Pid, Name, Cpu, Mem, Power, Gpu]);
    // the others before it gets fewer than 8
    assert_eq!(names(43), [Pid, Name, Cpu, Mem, Power, Gpu]);
    assert_eq!(names(42), [Pid, Name, Cpu, Mem, Gpu]);
    assert_eq!(names(35), [Pid, Name, Cpu, Mem, Gpu]);
    assert_eq!(names(34), [Pid, Name, Cpu, Mem]);
    assert_eq!(names(28), [Pid, Name, Cpu, Mem]);
    assert_eq!(names(27), [Pid, Name, Cpu]);
    assert_eq!(names(21), [Pid, Name, Cpu]);
    assert_eq!(names(20), [Pid, Name]);

    // NAME shrinks below its minimum once nothing else can go, then disappears
    assert_eq!(fit_columns(14, Pid), [(Pid, 5), (Name, 8)]);
    assert_eq!(fit_columns(10, Pid), [(Pid, 5), (Name, 4)]);
    assert_eq!(fit_columns(6, Pid), [(Pid, 5)]);
    assert_eq!(fit_columns(0, Pid), [(Pid, 5)]);
  }

  #[test]
  fn sorted_column_is_never_dropped() {
    use ProcSort::*;
    // another column goes in its place
    assert_eq!(fit_columns(40, User), [(Pid, 5), (Name, 9), (User, 10), (Cpu, 6), (Mem, 6)]);
    assert_eq!(fit_columns(30, Power), [(Pid, 5), (Name, 9), (Cpu, 6), (Power, 7)]);
    assert_eq!(fit_columns(30, Gpu), [(Pid, 5), (Name, 10), (Cpu, 6), (Gpu, 6)]);
    // even when NAME is left with a cell or none
    assert_eq!(fit_columns(20, Cpu), [(Pid, 5), (Name, 7), (Cpu, 6)]);
    assert_eq!(fit_columns(14, Mem), [(Pid, 5), (Name, 1), (Mem, 6)]);
    assert_eq!(fit_columns(13, Cpu), [(Pid, 5), (Cpu, 6)]);
    // once NAME gets a cell next to PID
    for sort in COLUMNS {
      for width in 7..120 {
        assert!(fit_columns(width, sort).iter().any(|(c, _)| *c == sort), "{sort:?} at {width}");
      }
    }
  }

  #[test]
  fn sort_arrow_follows_the_sorted_column_and_fits_it() {
    assert_eq!(ProcSort::Mem.header_text(ProcSort::Mem, true), "MEM ↓");
    assert_eq!(ProcSort::Mem.header_text(ProcSort::Mem, false), "MEM ↑");
    assert_eq!(ProcSort::Mem.header_text(ProcSort::Cpu, true), "MEM");

    // every column fits its header with the arrow
    for column in COLUMNS {
      let text = column.header_text(column, true);
      assert!(text.chars().count() <= usize::from(column.width()), "{text}");
    }
    // and every sort key has its column
    let mut sort = ProcSort::Cpu;
    for _ in 0..COLUMNS.len() {
      assert!(COLUMNS.contains(&sort), "{sort:?}");
      sort = sort.next();
    }
  }

  #[test]
  fn sort_cycle_wraps() {
    use ProcSort::*;
    let mut sorts = vec![Cpu];
    for _ in 0..7 {
      sorts.push(sorts.last().unwrap().next());
    }
    assert_eq!(sorts, [Cpu, Mem, Power, Gpu, Pid, Name, User, Cpu]);
  }

  #[test]
  fn column_areas_follow_the_columns_and_stop_at_the_edge() {
    use ProcSort::*;
    let columns = [(Pid, 5), (Name, 10), (Cpu, 6), (Mem, 6)];
    let cells = |width: u16| {
      let areas = column_areas(Rect::new(2, 5, width, 1), &columns);
      assert!(areas.iter().all(|(_, r)| r.y == 5 && r.height == 1));
      areas.into_iter().map(|(c, r)| (c, r.x, r.width)).collect::<Vec<_>>()
    };

    // one blank cell between columns
    assert_eq!(cells(30), [(Pid, 2, 5), (Name, 8, 10), (Cpu, 19, 6), (Mem, 26, 6)]);
    // cut at the edge, left out past it
    assert_eq!(cells(29), [(Pid, 2, 5), (Name, 8, 10), (Cpu, 19, 6), (Mem, 26, 5)]);
    assert_eq!(cells(24), [(Pid, 2, 5), (Name, 8, 10), (Cpu, 19, 6)]);
    assert_eq!(cells(20), [(Pid, 2, 5), (Name, 8, 10), (Cpu, 19, 3)]);
    assert!(cells(0).is_empty());
  }

  fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
  }

  /// `view()` as if rendered in a box at (0, 0), 40x6: the header on row 1 and 3 process rows
  /// below it.
  fn rendered_view() -> ProcView {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    view.fit(3);
    view.targets = Targets {
      area: Rect::new(0, 0, 40, 6),
      headers: column_areas(Rect::new(2, 1, 36, 1), &fit_columns(36, ProcSort::Cpu)),
      body: Rect::new(1, 2, 38, 3),
    };
    view
  }

  #[test]
  fn mouse_acts_on_the_rendered_targets() {
    let left = MouseEventKind::Down(MouseButton::Left);

    // header: PID at 2..7, then MEM at 25..31; a new key sorts in its own direction, the same
    // key reverses it
    let mut view = rendered_view();
    view.handle_mouse(mouse(left, 6, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Pid, false));
    view.handle_mouse(mouse(left, 2, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Pid, true));
    view.handle_mouse(mouse(left, 25, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Mem, true));
    view.handle_mouse(mouse(left, 6, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Pid, false));
    // the gap after PID
    view.handle_mouse(mouse(left, 7, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Pid, false));

    // the top border (`/ filter`) is no click target
    view.handle_mouse(mouse(left, 19, 0));
    assert!(!view.typing());

    // rows, the padding cells at the borders too
    let mut view = rendered_view();
    view.handle_mouse(mouse(left, 1, 3));
    assert_eq!(view.selected_pid(), Some(631));
    view.handle_mouse(mouse(left, 38, 4));
    assert_eq!(view.selected_pid(), Some(2301));

    // the selected row again: no selection
    view.handle_mouse(mouse(left, 20, 4));
    assert_eq!(view.selected_pid(), None);
    view.handle_mouse(mouse(left, 20, 4));
    assert_eq!(view.selected_pid(), Some(2301));

    // a blank row below the last process selects nothing
    view.set_procs(sample()[..2].to_vec()); // [631, 1]
    view.fit(3);
    view.handle_mouse(mouse(left, 20, 2));
    assert_eq!(view.selected_pid(), Some(631));
    view.handle_mouse(mouse(left, 20, 4));
    assert_eq!(view.selected_pid(), Some(631), "no process on that row");
  }

  #[test]
  fn mouse_works_while_typing() {
    let left = MouseEventKind::Down(MouseButton::Left);
    let mut view = rendered_view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "ar"); // [4410, 2301, 77]

    // a header click sorts the filtered rows, a row click selects one of them
    view.handle_mouse(mouse(left, 6, 1));
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Pid, false));
    assert_eq!(pids(&view), [77, 2301, 4410]);
    view.handle_mouse(mouse(left, 20, 3));
    assert_eq!(view.selected_pid(), Some(2301));
    view.handle_mouse(mouse(MouseEventKind::ScrollDown, 20, 3));
    assert_eq!(view.selected_pid(), Some(4410));

    // and the filter is still being typed; cargo no longer matches, so nothing is selected
    assert!(view.typing());
    type_str(&mut view, "i");
    assert_eq!((view.filter(), pids(&view)), ("ari", vec![77, 2301]));
    assert_eq!(view.selected_pid(), None);
  }

  #[test]
  fn wheel_moves_selection_and_offset_together() {
    let mut view = rendered_view(); // [4410, 631, 2301, 1, 77], 3 rows on screen
    let page = |view: &ProcView| view.page_rows().map(|(sel, p)| (sel, p.pid)).collect::<Vec<_>>();
    let wheel = |view: &mut ProcView, kind| {
      view.handle_mouse(mouse(kind, 20, 3));
      view.fit(3);
    };
    use MouseEventKind::{ScrollDown, ScrollUp};

    // without a selection it only scrolls, up to the end of the table
    wheel(&mut view, ScrollDown);
    assert_eq!(page(&view), [(false, 2301), (false, 1), (false, 77)]);
    wheel(&mut view, ScrollUp);
    assert_eq!(page(&view), [(false, 4410), (false, 631), (false, 2301)]);
    assert_eq!(view.selected_pid(), None);

    // with one, the selection moves along
    assert!(press(&mut view, KeyCode::Down));
    wheel(&mut view, ScrollDown);
    assert_eq!(page(&view), [(false, 2301), (true, 1), (false, 77)], "the end of the table");
    wheel(&mut view, ScrollDown);
    assert_eq!(page(&view), [(false, 2301), (false, 1), (true, 77)]);
    // the top of the table: the selection moves up on screen
    wheel(&mut view, ScrollUp);
    assert_eq!(page(&view), [(false, 4410), (true, 631), (false, 2301)]);
    wheel(&mut view, ScrollUp);
    assert_eq!(page(&view), [(true, 4410), (false, 631), (false, 2301)]);

    // outside the box
    view.handle_mouse(mouse(ScrollDown, 20, 6));
    view.fit(3);
    assert_eq!(view.selected_pid(), Some(4410));

    // an empty list
    let mut view = rendered_view();
    view.set_procs(vec![]);
    wheel(&mut view, ScrollDown);
    assert_eq!(view.selected_pid(), None);
  }

  #[test]
  fn hidden_panel_forgets_mouse_targets() {
    let mut view = rendered_view();
    view.clear();
    view.set_procs(sample());
    for (kind, x, y) in [
      (MouseEventKind::Down(MouseButton::Left), 6, 1),
      (MouseEventKind::Down(MouseButton::Left), 19, 0),
      (MouseEventKind::Down(MouseButton::Left), 20, 3),
      (MouseEventKind::ScrollDown, 20, 3),
    ] {
      view.handle_mouse(mouse(kind, x, y));
    }
    assert_eq!((view.sort, view.typing(), view.selected_pid()), (ProcSort::Cpu, false, None));
  }

  #[test]
  fn columns_fill_the_width() {
    // from PID, the widest sorted column (USER) and a cell for NAME
    for sort in COLUMNS {
      for width in 18..300 {
        let columns = fit_columns(width, sort);
        let used: u16 = columns.iter().map(|(_, w)| w + 1).sum::<u16>() - 1;
        assert_eq!(used, width, "width {width}: {columns:?}");
      }
    }
  }

  #[test]
  fn cut_text_ends_with_an_ellipsis() {
    assert_eq!(cut_end("WindowServer", 20), "WindowServer");
    assert_eq!(cut_end("WindowServer", 12), "WindowServer");
    assert_eq!(cut_end("WindowServer", 9), "WindowSe…");
    assert_eq!(cut_end("WindowServer", 1), "…");
    assert_eq!(cut_end("WindowServer", 0), "");
    // wide characters take two cells and are never split
    assert_eq!(cut_end("漢字テキスト", 6), "漢字…");
    assert_eq!(cut_end("漢字テキスト", 5), "漢字…");

    assert_eq!(cut_start("/usr/libexec/foo", 20), "/usr/libexec/foo");
    assert_eq!(cut_start("/usr/libexec/foo", 8), "…xec/foo");
    assert_eq!(cut_start("/usr/libexec/foo", 1), "…");
    assert_eq!(cut_start("/usr/libexec/foo", 0), "");
  }

  #[test]
  fn s_skips_columns_not_on_screen_and_arrows_leave_the_sort() {
    use ProcSort::*;
    // [PID, NAME, CPU%, MEM, GPU%] on screen: USER and POWER are left out
    let mut view = rendered_view();
    let mut sorts = vec![];
    for _ in 0..5 {
      assert!(press(&mut view, KeyCode::Char('s')));
      sorts.push((view.sort, view.sort_desc));
    }
    assert_eq!(sorts, [(Mem, true), (Gpu, true), (Pid, false), (Name, false), (Cpu, true)]);

    // ← / → aren't keys of the panel, and type nothing into a filter
    for code in [KeyCode::Left, KeyCode::Right] {
      assert!(!press(&mut view, code), "{code:?}");
      assert_eq!((view.sort, view.sort_desc), (Cpu, true), "{code:?}");
    }
    assert!(press(&mut view, KeyCode::Char('/')));
    for code in [KeyCode::Left, KeyCode::Right] {
      assert!(press(&mut view, code), "{code:?} while typing");
    }
    assert_eq!((view.sort, view.filter()), (Cpu, ""));
  }

  #[test]
  fn memory_sizes() {
    assert_eq!(format_mem(0), "0K");
    assert_eq!(format_mem(512 << 10), "512K");
    assert_eq!(format_mem(64 << 20), "64M");
    assert_eq!(format_mem(1536 << 20), "1.5G");
    assert_eq!(format_mem(40 << 30), "40.0G");

    // the unit follows the rounded value: no `1024K` or `1024M`
    const KIB: f64 = 1024.0;
    let bytes = |value: f64| value.round() as u64;
    assert_eq!(format_mem(bytes(1023.4 * KIB)), "1023K");
    assert_eq!(format_mem(bytes(1023.6 * KIB)), "1M");
    assert_eq!(format_mem(1 << 20), "1M");
    assert_eq!(format_mem(bytes(1023.4 * KIB * KIB)), "1023M");
    assert_eq!(format_mem(bytes(1023.9 * KIB * KIB)), "1.0G");
    assert_eq!(format_mem(1 << 30), "1.0G");
    assert_eq!(format_mem(bytes(99.9 * KIB * KIB * KIB)), "99.9G");
    assert_eq!(format_mem(bytes(99.96 * KIB * KIB * KIB)), "100G");
    assert_eq!(format_mem(512 << 30), "512G");
    // every size up to 16 TiB fits the MEM column
    for shift in 0..45 {
      for bytes in [(1u64 << shift) - 1, 1 << shift, (1 << shift) * 3 / 2] {
        assert!(format_mem(bytes).len() <= usize::from(ProcSort::Mem.width()), "{bytes}");
      }
    }
  }

  #[test]
  fn tail_keeps_whole_characters_from_the_end() {
    assert_eq!(tail("safari", 10), "safari");
    assert_eq!(tail("safari", 6), "safari");
    assert_eq!(tail("safari", 3), "ari");
    assert_eq!(tail("safari", 0), "");
    assert_eq!(tail("", 3), "");
    // wide characters take two cells
    assert_eq!(tail("ab漢字", 4), "漢字");
    assert_eq!(tail("ab漢字", 3), "字");
    assert_eq!(tail("ab漢字", 1), "");
  }

  #[test]
  fn navigation_keys_work_while_typing() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    view.fit(2);
    assert!(press(&mut view, KeyCode::Char('/')));
    // [4410 cargo, 2301 Safari, 77 safaribookmarksyncagent]
    type_str(&mut view, "ar");
    for (code, pid) in [
      (KeyCode::Down, 4410),
      (KeyCode::Down, 2301),
      (KeyCode::PageDown, 77),
      (KeyCode::Up, 2301),
      (KeyCode::PageUp, 4410),
      (KeyCode::End, 77),
      (KeyCode::Home, 4410),
    ] {
      assert!(press(&mut view, code), "{code:?}");
      assert_eq!(view.selected_pid(), Some(pid), "{code:?}");
    }
    assert!(view.typing());
    assert_eq!(view.filter(), "ar", "navigation keys don't edit the filter");
  }

  #[test]
  fn filter_hiding_the_selected_process_drops_the_selection() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(631));

    // `s` keeps WindowServer ([631, 2301, 77]), `sa` doesn't: no other row takes the selection
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "s");
    assert_eq!((pids(&view), view.selected_pid()), (vec![631, 2301, 77], Some(631)));
    type_str(&mut view, "af");
    assert_eq!(pids(&view), [2301, 77]);
    assert_eq!(view.selected_pid(), None);

    // and it doesn't come back with the rows
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!(pids(&view).len(), 5);
    assert_eq!(view.selected_pid(), None);
  }
}
