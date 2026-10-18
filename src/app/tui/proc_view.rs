//! Process panel: a process table with sorting, filtering and identity-aware selection,
//! driven by keys and the mouse.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
  KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::App;
use super::boxes::{
  Titles, draw_box, draw_scrollbar, hint, join, render_bottom_border, title_text_room,
};
use super::store::ratio;
use super::theme::{self, dim, faint, gradient, heading, text};
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
/// Fewest process rows left below the group totals; with fewer the totals are hidden.
const GROUPS_MIN_ROWS: u16 = 3;

// MARK: Table

/// Columns of the process table in display order: one per sort key, so each header sorts by its
/// own column.
const COLUMNS: [ProcSort; 8] = [
  ProcSort::Pid,
  ProcSort::Name,
  ProcSort::User,
  ProcSort::Time,
  ProcSort::Cpu,
  ProcSort::Mem,
  ProcSort::Gpu,
  ProcSort::Power,
];

/// Columns dropped (in this order) when the table is too narrow, each while NAME would get fewer
/// cells than the given width; PID, NAME and the sorted column always stay.
const DROP_ORDER: [(ProcSort, u16); 6] = [
  (ProcSort::Time, NAME_USER_WIDTH),
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
      Self::Time => "TIME",
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
      Self::Time => 8,
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

  /// Next column in the `s` cycle: the load columns as on screen, then TIME and the text columns:
  /// CPU → MEM → GPU → POWER → TIME → PID → NAME → USER → CPU.
  fn next(self) -> Self {
    match self {
      Self::Cpu => Self::Mem,
      Self::Mem => Self::Gpu,
      Self::Gpu => Self::Power,
      Self::Power => Self::Time,
      Self::Time => Self::Pid,
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

/// CPU time, cut and 8 cells up to 9999 days, as htop's TIME+: `M:SS.hh` (hundredths) under an
/// hour, `H:MM:SS` under a day, then days and hours (`2d03h`).
fn format_time(ns: u64) -> String {
  let (secs, hundredths) = (ns / 1_000_000_000, ns / 10_000_000 % 100);
  let (days, hours, mins, secs) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
  match (days, hours) {
    (0, 0) => format!("{mins}:{secs:02}.{hundredths:02}"),
    (0, _) => format!("{hours}:{mins:02}:{secs:02}"),
    _ => format!("{days}d{hours:02}h"),
  }
}

/// Order of `a` and `b` by the value of `sort`, ascending.
fn compare(a: &ProcInfo, b: &ProcInfo, sort: ProcSort) -> Ordering {
  fn lower(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
  }

  match sort {
    ProcSort::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
    ProcSort::Time => a.cpu_time_ns.cmp(&b.cpu_time_ns),
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

/// Sorts group totals like the processes: PID by the process count, NAME by the term. Groups
/// without a power reading go last when sorted by power; USER and ties keep the filter's order.
fn sort_groups(groups: &mut [Group], sort: ProcSort, desc: bool) {
  let missing = |g: &Group| sort == ProcSort::Power && g.power_w.is_none();
  groups.sort_by(|a, b| {
    let ord = match sort {
      ProcSort::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
      ProcSort::Time => a.cpu_time_ns.cmp(&b.cpu_time_ns),
      ProcSort::Mem => a.mem_bytes.cmp(&b.mem_bytes),
      ProcSort::Power => a.power_w.unwrap_or(0.0).total_cmp(&b.power_w.unwrap_or(0.0)),
      ProcSort::Gpu => a.gpu_pct.total_cmp(&b.gpu_pct),
      ProcSort::Pid => a.count.cmp(&b.count),
      ProcSort::Name => a.term.cmp(&b.term),
      ProcSort::User => Ordering::Equal,
    };
    let ord = if desc { ord.reverse() } else { ord };
    missing(a).cmp(&missing(b)).then(ord)
  });
}

/// Filter text split into terms: `safari, cargo, !helper` keeps rows matching any term and none of
/// the `!` ones. Each term is lowercase (a process matches it in its name or pid); empty terms are
/// ignored, so a filter still being typed (`safari,`) keeps its rows.
#[derive(Debug, Default)]
pub(super) struct Filter {
  any: Vec<String>,
  none: Vec<String>,
}

impl Filter {
  pub(super) fn parse(text: &str) -> Self {
    let mut filter = Self::default();
    for term in text.split(',').map(|t| t.trim().to_lowercase()) {
      match term.strip_prefix('!').map(str::trim) {
        Some(not) if !not.is_empty() => filter.none.push(not.to_string()),
        Some(_) => {}
        None if !term.is_empty() => filter.any.push(term),
        None => {}
      }
    }
    filter
  }

  /// Whether a row passes, `hit` telling whether a term is in it.
  pub(super) fn matches(&self, hit: impl Fn(&str) -> bool) -> bool {
    let hit = |term: &String| hit(term);
    (self.any.is_empty() || self.any.iter().any(hit)) && !self.none.iter().any(hit)
  }
}

/// Whether the lowercase `term` is in the name or pid of `proc`.
fn hit(proc: &ProcInfo, term: &str) -> bool {
  proc.name.to_lowercase().contains(term) || proc.pid.to_string().contains(term)
}

/// Totals of the listed processes matching one filter term: `chrome` sums Chrome and all its
/// helpers. A process matching two terms counts in both.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Group {
  term: String,
  count: usize,
  cpu_pct: f32,
  cpu_time_ns: u64,
  mem_bytes: u64,
  /// Sum of the known readings; `None` when no process has one.
  power_w: Option<f32>,
  /// Some processes have no power reading, so the sum is a lower bound.
  power_partial: bool,
  gpu_pct: f32,
}

impl Group {
  fn sum<'a>(term: &str, procs: impl Iterator<Item = &'a ProcInfo>) -> Self {
    let mut group = Self { term: term.to_string(), ..Self::default() };
    for proc in procs {
      group.count += 1;
      group.cpu_pct += proc.cpu_pct;
      group.cpu_time_ns += proc.cpu_time_ns;
      group.mem_bytes += proc.mem_bytes;
      group.gpu_pct += proc.gpu_pct;
      match proc.power_w {
        Some(watts) => *group.power_w.get_or_insert(0.0) += watts,
        None => group.power_partial = true,
      }
    }
    group
  }
}

/// Drops the last word of a filter and the commas and spaces after it: `claude, clan` →
/// `claude, `, then `claude, ` → ``.
fn delete_word(text: &mut String) {
  let separator = |c: char| c == ',' || c.is_whitespace();
  let kept = text.trim_end_matches(separator).trim_end_matches(|c| !separator(c)).len();
  text.truncate(kept);
}

/// Filter input on `filter`: characters and Backspace edit, Ctrl-W / Alt-Backspace delete the last
/// word, Ctrl-U (what Ghostty sends for Cmd-Backspace) and Cmd-Backspace delete the whole text,
/// Enter keeps the filter, Esc clears it; both end the input. Returns whether the filter changed.
pub(super) fn type_filter(filter: &mut String, typing: &mut bool, key: KeyEvent) -> bool {
  let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
  let alt = key.modifiers.contains(KeyModifiers::ALT);
  let cmd = key.modifiers.contains(KeyModifiers::SUPER);
  match key.code {
    KeyCode::Char('u') if ctrl => filter.clear(),
    KeyCode::Backspace if cmd => filter.clear(),
    KeyCode::Char('w') if ctrl => delete_word(filter),
    KeyCode::Backspace if alt => delete_word(filter),
    KeyCode::Char(c) if !(ctrl || alt || cmd) => filter.push(c),
    KeyCode::Backspace => {
      filter.pop();
    }
    KeyCode::Enter => {
      *typing = false;
      return false;
    }
    KeyCode::Esc => {
      *typing = false;
      filter.clear();
    }
    _ => return false,
  }
  true
}

/// `name`, then `/ filter`, or the filter once there is one or it is being typed. A filter too
/// long for the border shows its end (`/…ari█`); with no room for that next to `name`, the filter
/// takes its place.
pub(super) fn filter_titles(
  name: Vec<Span<'static>>,
  filter: &str,
  typing: bool,
  enabled: bool,
  width: u16,
) -> Titles<'static> {
  if !typing && filter.is_empty() {
    return Titles::new(name).left(hint("/", "filter"));
  }
  let beside = title_text_room(width, &[Line::from(name.clone())]);
  if beside >= FILTER_MIN_WIDTH {
    Titles::new(name).left(filter_title(filter, typing, enabled, beside))
  } else {
    Titles::new(filter_title(filter, typing, enabled, title_text_room(width, &[])))
  }
}

/// `/saf` (with a cursor while typing) in `room` cells: a filter too long keeps its end, after `…`.
pub(super) fn filter_title(
  filter: &str,
  typing: bool,
  enabled: bool,
  room: u16,
) -> Vec<Span<'static>> {
  // `/` and the cursor (one cell each) around the filter text
  let text_room = usize::from(room).saturating_sub(1 + usize::from(typing));

  let mut spans = vec![heading("/")];
  if Span::raw(filter).width() <= text_room {
    spans.push(text(filter.to_string()));
  } else {
    spans.push(dim(ELLIPSIS));
    spans.push(text(tail(filter, text_room.saturating_sub(1)).to_string()));
  }
  if typing {
    spans.push(Span::styled(FILTER_CURSOR, theme::TEXT));
  }
  if !enabled {
    for span in &mut spans {
      *span = faint(span.content.clone());
    }
  }
  spans
}

/// The row of `len` a navigation key (↑ / ↓, PgUp / PgDn, Home / End) moves the selection to from
/// row `at`, with `page` rows on screen: ↑ / ↓ wrap around at the ends, the others stop there;
/// `None` for other keys.
pub(super) fn step(code: KeyCode, at: usize, page: usize, len: usize) -> Option<usize> {
  let last = len.saturating_sub(1);
  Some(match code {
    KeyCode::Home => 0,
    KeyCode::End => last,
    KeyCode::Up if at == 0 => last,
    KeyCode::Up => at - 1,
    KeyCode::Down if at >= last => 0,
    KeyCode::Down => at + 1,
    KeyCode::PageUp => at.saturating_sub(page),
    KeyCode::PageDown => (at + page).min(last),
    _ => return None,
  })
}

/// First visible row so that row `at` is on screen, moving as little as possible from `offset`.
pub(super) fn scroll_offset(offset: usize, at: Option<usize>, height: usize, len: usize) -> usize {
  let offset = match at {
    Some(at) if at < offset => at,
    Some(at) if at >= offset + height => (at + 1).saturating_sub(height),
    _ => offset,
  };

  // no blank rows below the last process when the list shrinks
  offset.min(len.saturating_sub(height))
}

/// `text` cut to `max` cells, ending with `…` when cut.
pub(super) fn cut_end(text: &str, max: usize) -> String {
  match max {
    _ if Span::raw(text).width() <= max => text.to_string(),
    0 => String::new(),
    _ => format!("{}{ELLIPSIS}", head(text, max - 1)),
  }
}

/// `text` cut to its last `max` cells, starting with `…` when cut.
pub(super) fn cut_start(text: &str, max: usize) -> String {
  match max {
    _ if Span::raw(text).width() <= max => text.to_string(),
    0 => String::new(),
    _ => format!("{ELLIPSIS}{}", tail(text, max - 1)),
  }
}

/// `text` cut to `max` cells in the middle with `…`, keeping its start and the file name (from the
/// last `/`); from the start when even the name doesn't fit.
pub(super) fn cut_middle(text: &str, max: usize) -> String {
  if Span::raw(text).width() <= max {
    return text.to_string();
  }
  let name = &text[text.rfind('/').unwrap_or(0)..];
  match max.saturating_sub(Span::raw(name).width() + 1) {
    0 => cut_start(text, max),
    room => format!("{}{ELLIPSIS}{name}", head(text, room)),
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

type Identity = (i32, Option<(u64, u64)>);

fn identity(proc: &ProcInfo) -> Identity {
  // shortcut: ps rows only have PIDs; detect their reuse when the sampler supplies start times.
  (proc.pid, proc.started)
}

/// Selected process: its identity and its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
  pid: i32,
  started: Option<(u64, u64)>,
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
  pub(super) paused: bool,
  exited: HashSet<Identity>,
  pub(super) sort: ProcSort,
  pub(super) sort_desc: bool,
  filter: String,
  filter_enabled: bool,
  /// Filter input mode: keys edit the filter instead of acting as shortcuts.
  typing: bool,
  /// Latest sample, sorted; `None` until the first one arrives.
  procs: Option<Vec<ProcInfo>>,
  /// Indexes of `procs` that pass the filter, in display order.
  rows: Vec<usize>,
  /// Totals of `rows` for each filter term, sorted like the processes.
  groups: Vec<Group>,
  selected: Option<Selection>,
  /// First row on screen.
  offset: usize,
  /// Rows on screen at the last render, the PgUp / PgDn step.
  page: usize,
  targets: Targets,
  /// `c` copied the selected path, and no key was pressed since.
  pub(super) copied: bool,
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
      paused: false,
      exited: HashSet::new(),
      sort,
      sort_desc,
      filter: String::new(),
      filter_enabled: true,
      typing: false,
      procs: None,
      rows: vec![],
      groups: vec![],
      selected: None,
      offset: 0,
      page: 0,
      targets: Targets::default(),
      copied: false,
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

  pub(super) fn groups(&self) -> &[Group] {
    &self.groups
  }

  /// Rows the group totals and the line under them take in a table body `height` rows high: none
  /// without a filter term or a listed process, or when fewer than `GROUPS_MIN_ROWS` processes
  /// would be left on screen.
  pub(super) fn groups_height(&self, height: u16) -> u16 {
    let rows = self.groups.len() as u16 + 1;
    let shown = !self.groups.is_empty() && !self.rows.is_empty();
    if shown && height >= rows + GROUPS_MIN_ROWS { rows } else { 0 }
  }

  pub(super) fn typing(&self) -> bool {
    self.typing
  }

  /// Ends filter input as Enter does: the filter stays, keys act as shortcuts again.
  pub(super) fn end_typing(&mut self) {
    self.typing = false;
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

  pub(super) fn exited(&self, proc: &ProcInfo) -> bool {
    self.exited.contains(&identity(proc))
  }

  pub(super) fn toggle_pause(&mut self) {
    self.paused = !self.paused;
    if !self.paused {
      if let Some(procs) = &mut self.procs {
        procs.retain(|p| !self.exited.contains(&identity(p)));
      }
      self.exited.clear();
    }
    self.refresh(false);
  }

  /// Updates held processes by identity while paused, ignoring arrivals; the first sample
  /// initializes a sorted list even if Space was pressed during collection.
  pub(super) fn set_procs(&mut self, procs: Vec<ProcInfo>) {
    let first_sample = self.procs.is_none();
    if let (true, Some(held)) = (self.paused, &mut self.procs) {
      let indices: HashMap<_, _> = held.iter().enumerate().map(|(i, p)| (identity(p), i)).collect();
      self.exited = indices.keys().copied().collect();
      for proc in procs {
        let key = identity(&proc);
        self.exited.remove(&key);
        if let Some(&i) = indices.get(&key) {
          held[i] = proc;
        }
      }
    } else {
      self.procs = Some(procs);
    }
    self.refresh(first_sample);
  }

  /// Drops the list, ends filter input and forgets the mouse targets (the panel got hidden). The
  /// filter and sort stay.
  pub(super) fn clear(&mut self) {
    self.procs = None;
    self.paused = false;
    self.exited.clear();
    self.typing = false;
    self.targets = Targets::default();
    self.refresh(false);
  }

  /// Re-filters and sorts on live samples or explicit requests. Selection follows identity,
  /// and is dropped when that process is gone or filtered out.
  fn refresh(&mut self, sort_requested: bool) {
    let sort_now = !self.paused || sort_requested;
    let procs = self.procs.as_deref_mut().unwrap_or_default();
    if sort_now {
      sort_procs(procs, self.sort, self.sort_desc);
    }

    let filter = Filter::parse(if self.filter_enabled { &self.filter } else { "" });
    self.rows = (0..procs.len()).filter(|&i| filter.matches(|t| hit(&procs[i], t))).collect();
    let groups = filter
      .any
      .iter()
      .map(|term| Group::sum(term, self.rows().filter(|p| !self.exited(p) && hit(p, term))));
    let mut groups: Vec<_> = groups.collect();
    if !sort_now {
      groups.sort_by_key(|g| {
        self.groups.iter().position(|old| old.term == g.term).unwrap_or(usize::MAX)
      });
    }
    self.groups = groups;
    if sort_now {
      sort_groups(&mut self.groups, self.sort, self.sort_desc);
    }

    let procs = self.procs.as_deref().unwrap_or_default();
    let rows = &self.rows;
    self.selected = self.selected.and_then(|sel| {
      let index = rows.iter().position(|&i| identity(&procs[i]) == (sel.pid, sel.started))?;
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
    self.selected = self.rows.get(index).map(|&i| Selection {
      pid: procs[i].pid,
      started: procs[i].started,
      index,
    });
  }

  /// Scrolls the table to show row `offset` first, without blank rows below the last process.
  fn scroll_to(&mut self, offset: usize) {
    self.offset = offset.min(self.rows.len().saturating_sub(self.page));
  }

  /// Moves the selection for a navigation key (↑ / ↓, PgUp / PgDn, Home / End), as `step` does.
  /// Without a selection ↑ / ↓ select the top row on screen and the others scroll the table.
  /// Returns `false` for other keys.
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

    let Some(index) = step(code, selected, page, self.rows.len()) else { return false };
    self.select(index);
    true
  }

  /// Applies a key press. Returns `false` for keys the panel doesn't use, so they can act as
  /// global shortcuts; while typing a filter every key is used. A navigation key ends filter input
  /// as Enter does.
  pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
    if self.navigate(key.code) {
      self.end_typing();
      return true;
    }

    match key.code {
      _ if self.typing => {
        if type_filter(&mut self.filter, &mut self.typing, key) {
          self.refresh(false);
        }
      }
      KeyCode::Char(' ') => self.toggle_pause(),
      KeyCode::Char('/') => {
        self.filter_enabled = true;
        self.typing = true;
        self.refresh(false);
      }
      KeyCode::Char('f') if !self.filter.is_empty() => {
        self.filter_enabled = !self.filter_enabled;
        self.refresh(false);
      }
      KeyCode::Char('f') => {}
      KeyCode::Char('S') => self.reverse_sort(),
      KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::SHIFT) => self.reverse_sort(),
      KeyCode::Char('s') => self.next_sort(),
      KeyCode::Esc if self.selected.is_some() || !self.filter.is_empty() => {
        self.selected = None;
        self.filter.clear();
        self.offset = 0;
        self.refresh(false);
      }
      _ => return false,
    }

    true
  }

  fn reverse_sort(&mut self) {
    self.sort_desc = !self.sort_desc;
    self.refresh(true);
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
    self.refresh(true);
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

  /// Applies a mouse event to the cells of the last render: any click ends filter input, a click
  /// on a column header sorts by it (again: reverses), on a process selects it (on the selected
  /// one clears the selection); the wheel over the box moves the selection `WHEEL_ROWS` rows, or
  /// scrolls without one. Anything else is ignored.
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
    self.end_typing();
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

  /// Titles of a process box `width` cells wide: `proc 412` (`proc  12/412` with a filter or while
  /// one is typed, the count padded to the digits of the total), then the filter as
  /// `filter_titles` shows it, then `sort`, and `info | finder | copy | kill` on the
  /// right with a process selected (neither while typing: keys are text then).
  fn titles(&self, width: u16) -> Titles<'_> {
    let mut name = vec![heading("proc")];
    if let Some(procs) = self.procs() {
      let total = procs.len().to_string();
      // padded to the digits of the total, so the filter after it stays put while it is typed
      let count = if (self.filter.is_empty() || !self.filter_enabled) && !self.typing {
        format!(" {total}")
      } else {
        format!(" {:>width$}/{total}", self.row_count(), width = total.len())
      };
      name.push(text(count));
    }

    let titles = filter_titles(name, &self.filter, self.typing, self.filter_enabled, width);
    let actions =
      || join([hint("i", "info"), hint("e", "finder"), hint("c", "copy"), hint("k", "kill")]);
    match self.typing {
      true => titles,
      false if self.selected().is_some_and(|p| self.exited(p)) => {
        titles.left(hint("s", "sort")).right(join([hint("e", "finder"), hint("c", "copy")]))
      }
      false if self.selected().is_some() => titles.left(hint("s", "sort")).right(actions()),
      false => titles.left(hint("s", "sort")),
    }
  }

  /// Left side of the bottom border of the process box: a faint `copied` after `c`, else a dim
  /// note that POWER is known for own processes only, while the POWER column shows processes
  /// without it next to some with it.
  fn border_text(&self, power_shown: bool) -> Vec<Span<'static>> {
    let procs = self.procs().unwrap_or_default();
    let power = |known: bool| procs.iter().any(|p| p.power_w.is_some() == known);
    if self.copied {
      vec![faint("copied")]
    } else if power_shown && power(true) && power(false) {
      vec![dim("POWER: own processes only")]
    } else {
      vec![]
    }
  }
}

// MARK: Render

impl App {
  /// Process panel: count, filter and sort hint in the title, a header row with the sort arrow,
  /// the process rows with a scrollbar when they don't fit, and the POWER note and the key hints
  /// on the bottom border. Keeps the cells that react to the mouse for `ProcView::handle_mouse`.
  pub(super) fn render_proc_box(&mut self, f: &mut Frame, area: Rect) {
    let inner = draw_box(f, area, self.proc_view.titles(area.width));
    let (headers, body) = self.render_proc_table(f, inner);
    let power_shown = headers.iter().any(|&(column, _)| column == ProcSort::Power);
    self.proc_view.targets = Targets { area, headers, body };

    let view = &self.proc_view;
    draw_scrollbar(f, area, body, view.row_count(), view.offset);
    let mut hints = self.footer_hints();
    if view.paused {
      hints.insert(0, vec![theme::paused()]);
    }
    render_bottom_border(f, area, hints, |_| view.border_text(power_shown));
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

    // the group totals between the header and the processes
    let below_header = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    let groups_height = self.proc_view.groups_height(below_header.height);
    let body = Rect {
      y: below_header.y + groups_height,
      height: below_header.height - groups_height,
      ..inner
    };
    self.proc_view.fit(body.height as usize);
    // one blank cell between the columns and each border, as in the metrics box
    let table = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let columns = fit_columns(table.width, self.proc_view.sort);

    let filter = self.proc_view.filter();
    if self.proc_view.row_count() == 0 && self.proc_view.filter_enabled && !filter.is_empty() {
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

    if groups_height > 0 {
      let bold = Style::new().add_modifier(Modifier::BOLD);
      for (i, group) in self.proc_view.groups().iter().enumerate() {
        let y = below_header.y + i as u16;
        let cells =
          columns.iter().map(|&(column, _)| self.group_cell(column, group).patch_style(bold));
        draw_row(buf, Rect { y, height: 1, ..table }, &columns, cells);
      }
      let line = "─".repeat(usize::from(table.width));
      buf.set_string(table.x, body.y - 1, line, Style::new().fg(theme::BORDER));
    }

    for (i, (selected, proc)) in self.proc_view.page_rows().enumerate() {
      let y = body.y + i as u16;
      let cells = columns.iter().map(|&(column, _)| self.proc_cell(column, proc));
      draw_row(buf, Rect { y, height: 1, ..table }, &columns, cells);
      if self.proc_view.exited(proc) {
        buf.set_style(Rect { y, height: 1, ..inner }, Style::new().add_modifier(Modifier::DIM));
      }
      // from border to border, so the row reads as one bar
      if selected {
        buf.set_style(
          Rect { y, height: 1, ..inner },
          theme::SELECTED.remove_modifier(Modifier::DIM),
        );
        if self.proc_view.exited(proc)
          && let Some((_, pid)) = column_areas(Rect { y, height: 1, ..table }, &columns)
            .into_iter()
            .find(|(column, _)| *column == ProcSort::Pid)
        {
          buf.set_style(pid, theme::SELECTED_EXITED_PID);
        }
      }
    }

    (column_areas(header_row, &columns), body)
  }

  /// Text of one table cell. Load values are colored by the gradient, zeros are dim and missing
  /// values show as a dim `-`.
  fn proc_cell(&self, column: ProcSort, proc: &ProcInfo) -> Span<'static> {
    if self.proc_view.exited(proc)
      && !matches!(column, ProcSort::Pid | ProcSort::Name | ProcSort::User)
    {
      return dim("-");
    }
    match column {
      ProcSort::Pid => text(proc.pid.to_string()),
      ProcSort::Name => text(proc.name.clone()),
      ProcSort::User => text(proc.user.clone()),
      ProcSort::Cpu => pct_cell(proc.cpu_pct),
      ProcSort::Time => time_cell(proc.cpu_time_ns),
      ProcSort::Mem => self.mem_cell(proc.mem_bytes),
      ProcSort::Power => power_cell(proc.power_w, false),
      ProcSort::Gpu => pct_cell(proc.gpu_pct),
    }
  }

  /// Text of one cell of a group row: the process count in PID (`×12`), the term in NAME, the
  /// totals in the load columns; POWER starts with `≥` when some processes have no reading.
  fn group_cell(&self, column: ProcSort, group: &Group) -> Span<'static> {
    match column {
      ProcSort::Pid => text(format!("×{}", group.count)),
      ProcSort::Name => text(group.term.clone()),
      ProcSort::User => text(""),
      ProcSort::Cpu => pct_cell(group.cpu_pct),
      ProcSort::Time => time_cell(group.cpu_time_ns),
      ProcSort::Mem => self.mem_cell(group.mem_bytes),
      ProcSort::Power => power_cell(group.power_w, group.power_partial),
      ProcSort::Gpu => pct_cell(group.gpu_pct),
    }
  }

  fn mem_cell(&self, bytes: u64) -> Span<'static> {
    let mem = bytes as f64;
    load_cell(mem, ratio(mem, self.mem.ram_total as f64), format_mem(bytes))
  }
}

/// A load value colored by the gradient at `ratio`; zero is dim.
fn load_cell(value: f64, ratio: f64, text: String) -> Span<'static> {
  if value > 0.0 { Span::styled(text, gradient(ratio)) } else { dim(text) }
}

/// CPU% or GPU%: 100 is one full core or the whole GPU.
fn pct_cell(pct: f32) -> Span<'static> {
  let pct = f64::from(pct);
  load_cell(pct, pct / 100.0, format!("{pct:.1}"))
}

/// CPU time; zero is dim.
fn time_cell(ns: u64) -> Span<'static> {
  if ns == 0 { dim(format_time(ns)) } else { text(format_time(ns)) }
}

/// Power in watts, `≥` before a lower bound; a dim `-` without a reading.
fn power_cell(watts: Option<f32>, lower_bound: bool) -> Span<'static> {
  let Some(watts) = watts.map(f64::from) else { return dim("-") };
  let at_least = if lower_bound { "≥" } else { "" };
  load_cell(watts, watts / POWER_HOT_W, format!("{at_least}{watts:.2}W"))
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
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
  use ratatui::layout::Rect;

  use super::{ProcView, column_areas, cut_end, cut_middle, cut_start, fit_columns, format_time};
  use crate::config::ProcSort;
  use crate::procs::ProcInfo;
  use crate::tui::boxes::draw_box;

  const MIB: u64 = 1 << 20;

  fn proc(pid: i32, name: &str, cpu: f32, mem_mb: u64, power: Option<f32>, gpu: f32) -> ProcInfo {
    ProcInfo {
      pid,
      name: name.to_string(),
      path: format!("/usr/bin/{name}"),
      user: "user".to_string(),
      cpu_pct: cpu,
      // an order of its own: the last two digits of the pid in seconds
      cpu_time_ns: (pid % 100) as u64 * 1_000_000_000,
      mem_bytes: mem_mb * MIB,
      power_w: power,
      gpu_pct: gpu,
      started: None,
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
  fn paused_startup_uses_sorted_first_sample() {
    let mut view = ProcView::default();
    press(&mut view, KeyCode::Char(' '));
    view.set_procs(sample());
    assert_eq!(pids(&view), [4410, 631, 2301, 1, 77]);
    assert!(view.paused && view.exited.is_empty());
  }

  #[test]
  fn paused_explicit_sort_reorders_once_and_keeps_selection() {
    let mut view = view();
    press(&mut view, KeyCode::Down);
    let selected = view.selected_pid();
    press(&mut view, KeyCode::Char(' '));
    press(&mut view, KeyCode::Char('S'));
    assert_eq!(pids(&view), [77, 1, 2301, 631, 4410]);
    assert!(view.paused);
    assert_eq!(view.selected_pid(), selected);
    let mut next = sample();
    next[0].cpu_pct = 100.0;
    view.set_procs(next);
    assert_eq!(pids(&view), [77, 1, 2301, 631, 4410]);
  }

  #[test]
  fn pause_keeps_slots_live_values_and_selection_until_resume() {
    let mut view = view();
    view.fit(3);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Down);
    let selected = view.selected().unwrap().pid;
    let before = pids(&view);
    let offset = view.offset;
    press(&mut view, KeyCode::Char(' '));
    let mut next = sample();
    next.retain(|p| p.pid != selected);
    next[0].cpu_pct = 99.0;
    next.push(proc(9000, "added", 100.0, 10, None, 0.0));
    view.set_procs(next);
    assert_eq!(pids(&view), before);
    assert_eq!(view.selected_pid(), Some(selected));
    assert!(view.exited(view.selected().unwrap()));
    assert_eq!(view.offset, offset);
    assert_eq!(view.rows().find(|p| p.pid == 1).unwrap().cpu_pct, 99.0);
    view.set_procs(sample());
    press(&mut view, KeyCode::Char(' '));
    assert!(!view.paused && view.exited.is_empty());
    assert_eq!(pids(&view), [4410, 631, 2301, 1, 77]);
    assert_eq!(view.selected_pid(), Some(selected));
  }

  #[test]
  fn pause_distinguishes_reused_pids_and_excludes_exited_totals() {
    let mut view = ProcView::default();
    let mut old = proc(123, "worker", 25.0, 10, None, 0.0);
    old.started = Some((1, 0));
    view.set_procs(vec![old.clone()]);
    type_str(&mut view, "/worker");
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Down);
    press(&mut view, KeyCode::Char(' '));
    let mut new = old;
    new.started = Some((2, 0));
    new.cpu_pct = 50.0;
    view.set_procs(vec![new.clone()]);
    assert_eq!(pids(&view), [123]);
    assert_eq!(view.selected().unwrap().started, Some((1, 0)));
    assert!(view.exited(view.selected().unwrap()));
    assert_eq!((view.groups[0].count, view.groups[0].cpu_pct), (0, 0.0));
    press(&mut view, KeyCode::Char(' '));
    assert_eq!(view.selected(), None, "PID reuse must not steal selection");
    assert_eq!(view.procs(), Some([].as_slice()));
    view.set_procs(vec![new.clone()]);
    assert_eq!(view.procs(), Some([new].as_slice()));
    press(&mut view, KeyCode::Char(' '));
    view.clear();
    assert!(!view.paused && view.procs().is_none());
  }

  #[test]
  fn pause_before_first_sample_and_group_order_are_stable() {
    let mut view = ProcView::default();
    press(&mut view, KeyCode::Char(' '));
    view.set_procs(sample());
    assert_eq!(pids(&view), [4410, 631, 2301, 1, 77]);
    press(&mut view, KeyCode::Char(' '));
    type_str(&mut view, "/safari, cargo");
    press(&mut view, KeyCode::Enter);
    assert_eq!(
      view.groups.iter().map(|g| g.term.as_str()).collect::<Vec<_>>(),
      ["cargo", "safari"]
    );
    press(&mut view, KeyCode::Char(' '));
    let mut next = sample();
    next[2].cpu_pct = 100.0;
    view.set_procs(next);
    assert_eq!(
      view.groups.iter().map(|g| g.term.as_str()).collect::<Vec<_>>(),
      ["cargo", "safari"]
    );
    assert_eq!(view.groups[1].cpu_pct, 100.0);
    press(&mut view, KeyCode::Char('S'));
    assert!(view.paused);
    assert_eq!(view.groups[0].term, "cargo");
    press(&mut view, KeyCode::Char('S'));
    assert_eq!(view.groups[0].term, "safari");
    let mut next = sample();
    next[3].cpu_pct = 200.0;
    view.set_procs(next);
    assert_eq!(view.groups[0].term, "safari");
    press(&mut view, KeyCode::Char(' '));
    assert_eq!(view.groups[0].term, "cargo");
  }

  #[test]
  fn paused_empty_sample_stays_empty_and_resume_accepts_arrivals() {
    let mut view = ProcView::default();
    view.set_procs(vec![]);
    view.toggle_pause();
    view.set_procs(sample());
    assert_eq!(view.row_count(), 0);
    view.toggle_pause();
    assert_eq!(view.row_count(), 0);
    view.set_procs(sample());
    assert_eq!(view.row_count(), 5);
  }

  #[test]
  fn paused_header_click_sorts_held_rows_and_preserves_selection() {
    let mut view = view();
    view.select(1);
    let selected = view.selected_pid();
    view.toggle_pause();
    view.targets.headers = column_areas(Rect::new(2, 1, 150, 1), &fit_columns(150, view.sort));
    for column in super::COLUMNS {
      let (_, rect) = view.targets.headers.iter().find(|(sort, _)| *sort == column).unwrap();
      view.handle_mouse(super::MouseEvent {
        kind: super::MouseEventKind::Down(super::MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
      });
      let mut expected = sample();
      super::sort_procs(&mut expected, column, view.sort_desc);
      assert_eq!(pids(&view), expected.iter().map(|p| p.pid).collect::<Vec<_>>());
      assert!(view.paused);
      assert_eq!(view.selected_pid(), selected);
    }
  }

  #[test]
  fn sorts_by_each_key_and_direction() {
    let cases = [
      (ProcSort::Cpu, [4410, 631, 2301, 1, 77], [77, 1, 2301, 631, 4410]),
      // 77, 31, 10, 1 and 1 seconds
      (ProcSort::Time, [77, 631, 4410, 1, 2301], [1, 2301, 4410, 631, 77]),
      (ProcSort::Mem, [2301, 631, 4410, 1, 77], [77, 1, 4410, 631, 2301]),
      // no power reading goes last in both directions
      (ProcSort::Power, [4410, 631, 2301, 77, 1], [77, 2301, 631, 4410, 1]),
      // ties by pid
      (ProcSort::Gpu, [631, 2301, 1, 77, 4410], [1, 77, 4410, 2301, 631]),
      (ProcSort::Pid, [4410, 2301, 631, 77, 1], [1, 77, 631, 2301, 4410]),
      // case-insensitive
      (ProcSort::Name, [631, 77, 2301, 1, 4410], [4410, 1, 2301, 77, 631]),
      (ProcSort::User, [1, 77, 631, 2301, 4410], [1, 77, 631, 2301, 4410]),
    ];

    for (sort, desc, asc) in cases {
      let mut view = ProcView::new(sort, true);
      view.set_procs(sample());
      assert_eq!(pids(&view), desc, "{sort:?} desc");
      view.select(0);
      let selected = view.selected_pid();
      view.toggle_pause();
      view.reverse_sort();
      assert_eq!(pids(&view), asc, "{sort:?} paused asc");
      view.reverse_sort();
      assert_eq!(pids(&view), desc, "{sort:?} paused desc");
      assert!(view.paused);
      assert_eq!(view.selected_pid(), selected);

      let mut view = ProcView::new(sort, false);
      view.set_procs(sample());
      assert_eq!(pids(&view), asc, "{sort:?} asc");
    }
  }

  #[test]
  fn s_cycles_the_columns_on_screen_and_shift_s_reverses() {
    use ProcSort::*;
    // before the first render every column, each in its own direction: numbers largest first,
    // PIDs and text from the start
    let mut view = view();
    view.toggle_pause();
    let mut sorts = vec![];
    for _ in 0..8 {
      assert!(press(&mut view, KeyCode::Char('s')));
      sorts.push((view.sort, view.sort_desc));
    }
    let expected = [
      (Mem, true),
      (Gpu, true),
      (Power, true),
      (Time, true),
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
    assert_eq!((view.sort, view.sort_desc), (Cpu, true));

    // [PID, NAME, CPU%, MEM, GPU%] on screen: USER, TIME and POWER are left out
    view.targets.headers = column_areas(Rect::new(2, 1, 36, 1), &fit_columns(36, Cpu));
    let mut sorts = vec![];
    for _ in 0..5 {
      assert!(press(&mut view, KeyCode::Char('s')));
      sorts.push(view.sort);
    }
    assert_eq!(sorts, [Mem, Gpu, Pid, Name, Cpu]);
  }

  #[test]
  fn filter_matches_name_or_pid_ignoring_case() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "SAF");
    assert_eq!(pids(&view), [2301, 77]);
    assert_eq!(view.procs().map(<[_]>::len), Some(5), "the full list is kept");

    // "1" is in pids 4410, 631, 2301 and 1
    for _ in 0..3 {
      assert!(press(&mut view, KeyCode::Backspace));
    }
    type_str(&mut view, "1");
    assert_eq!(pids(&view), [4410, 631, 2301, 1]);
  }

  #[test]
  fn toggling_a_saved_filter_restores_rows_totals_and_selection_rules() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('f')));
    assert!(view.filter_enabled);
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf, cargo, !bookmark");
    assert!(press(&mut view, KeyCode::Enter));
    view.select(1);
    assert_eq!(view.selected_pid(), Some(2301));
    assert_eq!(pids(&view), [4410, 2301]);
    assert_eq!(view.groups().len(), 2);

    assert!(press(&mut view, KeyCode::Char('f')));
    assert_eq!(view.filter(), "saf, cargo, !bookmark");
    assert_eq!(pids(&view), [4410, 631, 2301, 1, 77]);
    assert!(view.groups().is_empty());
    assert_eq!(view.selected_pid(), Some(2301));
    view.set_procs(sample());
    assert_eq!(view.row_count(), 5, "new samples keep the filter disabled");
    assert!(press(&mut view, KeyCode::Char('f')));
    assert_eq!(pids(&view), [4410, 2301]);
    assert_eq!(view.groups().len(), 2);
    assert_eq!(view.selected_pid(), Some(2301));

    assert!(press(&mut view, KeyCode::Char('f')));
    view.select(1); // WindowServer is outside the saved filter
    assert!(press(&mut view, KeyCode::Char('/')));
    assert!(view.filter_enabled && view.typing());
    assert_eq!(view.selected_pid(), None);
    type_str(&mut view, "f");
    assert_eq!(view.filter(), "saf, cargo, !bookmarkf");
    assert!(view.filter_enabled, "f is text while editing");
  }

  #[test]
  fn disabled_filter_titles_keep_the_text_faint_and_the_count_unfiltered() {
    use ratatui::style::Modifier;

    let mut view = view();
    press(&mut view, KeyCode::Char('/'));
    type_str(&mut view, "a-long-filter-with-no-matches");
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Char('f'));
    for width in [12, 80] {
      let mut term = Terminal::new(TestBackend::new(width, 3)).unwrap();
      term.draw(|f| _ = draw_box(f, f.area(), view.titles(width))).unwrap();
      let buf = term.backend().buffer();
      let line: String = (0..width).map(|x| buf[(x, 0)].symbol()).collect();
      assert!(line.contains(if width == 12 { "ches" } else { "matches" }), "{line}");
      assert!(line.contains("/…") || line.contains("/a-long"), "{line}");
      if width == 80 {
        assert!(line.contains("proc 5 ─"), "{line}");
        assert!(!line.contains("5/5"));
      }
      let slash = (0..width).find(|&x| buf[(x, 0)].symbol() == "/").unwrap();
      let cells = if width == 12 { 6 } else { 1 + view.filter.len() as u16 };
      for x in slash..slash + cells {
        assert!(buf[(x, 0)].modifier.contains(Modifier::DIM));
      }
    }
  }

  #[test]
  fn filter_terms_match_any_and_exclude_with_bang() {
    // [4410, 631, 2301, 1, 77] by CPU
    let cases: [(&str, &[i32]); 7] = [
      ("cargo, window", &[4410, 631]),
      (" Cargo ,WINDOW ", &[4410, 631]),
      // a term still being typed changes nothing
      ("cargo,", &[4410]),
      ("cargo, !", &[4410]),
      // exclusions alone keep the rest
      ("!saf", &[4410, 631, 1]),
      ("saf, !bookmark", &[2301]),
      ("saf, !77", &[2301]),
    ];

    for (filter, expected) in cases {
      let mut view = view();
      assert!(press(&mut view, KeyCode::Char('/')));
      type_str(&mut view, filter);
      assert_eq!(pids(&view), expected, "{filter:?}");
    }
  }

  #[test]
  fn groups_total_each_term_over_the_listed_processes() {
    let summary = |view: &ProcView| {
      let group = |g: &super::Group| {
        (
          g.term.clone(),
          g.count,
          g.cpu_pct,
          g.mem_bytes / MIB,
          g.power_w,
          g.power_partial,
          g.gpu_pct,
        )
      };
      view.groups().iter().map(group).collect::<Vec<_>>()
    };

    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    // `d`: launchd without a power reading and WindowServer; `saf` without the excluded agent
    type_str(&mut view, "d, saf, !bookmark");
    let expected = [
      ("d".to_string(), 2, 25.5, 320, Some(1.5), true, 40.0),
      ("saf".to_string(), 1, 12.0, 900, Some(0.8), false, 5.0),
    ];
    assert_eq!(summary(&view), expected);

    // totals and their order follow new samples: without WindowServer no power is known for `d`
    let mut procs = sample();
    procs.remove(1);
    view.set_procs(procs);
    let expected = [
      ("saf".to_string(), 1, 12.0, 900, Some(0.8), false, 5.0),
      ("d".to_string(), 1, 0.5, 20, None, true, 0.0),
    ];
    assert_eq!(summary(&view), expected);
  }

  #[test]
  fn groups_sort_like_the_processes() {
    let terms = |view: &ProcView| view.groups().iter().map(|g| g.term.clone()).collect::<Vec<_>>();

    // CPU: cargo 80, window 25, saf 12 + 0
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf, cargo, window");
    assert!(press(&mut view, KeyCode::Enter));
    assert_eq!(terms(&view), ["cargo", "window", "saf"]);
    view.toggle_pause();
    assert!(press(&mut view, KeyCode::Char('S')));
    assert_eq!(terms(&view), ["saf", "window", "cargo"]);

    // PID by the process count, ties in the filter's order
    view.sort_by(ProcSort::Pid);
    assert_eq!(terms(&view), ["cargo", "window", "saf"]);
    view.sort_by(ProcSort::Name);
    assert_eq!(terms(&view), ["cargo", "saf", "window"]);
  }

  #[test]
  fn groups_show_with_room_for_processes_below() {
    let mut view = view();
    assert_eq!(view.groups_height(20), 0, "no filter");

    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf, cargo, !77");
    // two totals and the line under them, while 3 processes still fit
    assert_eq!((view.groups_height(6), view.groups_height(5)), (3, 0));

    assert!(press(&mut view, KeyCode::Esc));
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "zzz");
    assert_eq!(view.groups_height(20), 0, "nothing listed");
  }

  #[test]
  fn typing_takes_shortcut_keys_as_text() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "qcsS/1");
    assert_eq!(view.filter(), "qcsS/1");
    assert_eq!((view.sort, view.sort_desc), (ProcSort::Cpu, true));

    // other control / alt / cmd chords don't edit the filter, but are still used
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::SUPER] {
      assert!(view.handle_key(KeyEvent::new(KeyCode::Char('x'), modifiers)));
    }
    assert_eq!(view.filter(), "qcsS/1");
  }

  #[test]
  fn navigation_ends_filter_input_keeping_the_filter() {
    // Safari selected, then filtered to [2301, 77]
    // ↑ wraps around to the last row
    for (code, pid) in
      [(KeyCode::Down, 77), (KeyCode::PageDown, 77), (KeyCode::End, 77), (KeyCode::Up, 77)]
    {
      let mut view = view(); // [4410, 631, 2301, 1, 77]
      for _ in 0..3 {
        assert!(press(&mut view, KeyCode::Down));
      }
      assert!(press(&mut view, KeyCode::Char('/')));
      type_str(&mut view, "saf");
      assert!(press(&mut view, code));
      assert_eq!((view.filter(), view.typing(), view.selected_pid()), ("saf", false, Some(pid)));

      // keys are shortcuts again
      assert!(press(&mut view, KeyCode::Char('s')));
      assert_eq!((view.filter(), view.sort), ("saf", ProcSort::Mem), "{code:?}");
    }
  }

  #[test]
  fn filter_input_deletes_words_and_the_whole_text() {
    let key = |code, modifiers| KeyEvent::new(code, modifiers);
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));

    // Ctrl-W and Alt-Backspace: the last word with the separators after it
    for delete_word in
      [key(KeyCode::Char('w'), KeyModifiers::CONTROL), key(KeyCode::Backspace, KeyModifiers::ALT)]
    {
      type_str(&mut view, "claude, !clan");
      assert!(view.handle_key(delete_word));
      assert_eq!(view.filter(), "claude, ");
      assert!(view.handle_key(delete_word));
      assert_eq!(view.filter(), "");
    }

    // Ctrl-U (Cmd-Backspace in Ghostty) and Cmd-Backspace: the whole text, still typing
    for delete_all in
      [key(KeyCode::Char('u'), KeyModifiers::CONTROL), key(KeyCode::Backspace, KeyModifiers::SUPER)]
    {
      type_str(&mut view, "saf, cargo");
      assert!(view.handle_key(delete_all));
      assert_eq!((view.filter(), view.typing()), ("", true));
      assert_eq!(pids(&view).len(), 5);
    }
  }

  #[test]
  fn selection_follows_its_pid() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(631));

    // through a reversed sort
    assert!(press(&mut view, KeyCode::Char('S'))); // [77, 1, 2301, 631, 4410]
    assert_eq!(view.selected_pid(), Some(631));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(4410), "moves from the new position");
    assert!(press(&mut view, KeyCode::Up));

    // and a new sample that reorders the list
    let mut procs = sample();
    procs[1].cpu_pct = 0.1;
    view.set_procs(procs);
    assert_eq!(pids(&view), [77, 631, 1, 2301, 4410]);
    assert_eq!(view.selected_pid(), Some(631));
    assert!(press(&mut view, KeyCode::Down));
    assert_eq!(view.selected_pid(), Some(1));
  }

  #[test]
  fn selection_is_dropped_when_its_process_exits_or_is_filtered_out() {
    let all = sample();
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));

    // other processes come and go: the selection stays on its process
    view.set_procs(vec![all[1].clone(), all[3].clone()]);
    assert_eq!((pids(&view), view.selected_pid()), (vec![4410, 631], Some(631)));
    // it exits: no neighbour takes over, and it isn't back with its pid
    view.set_procs(vec![all[2].clone(), all[3].clone()]);
    assert_eq!(view.selected_pid(), None);
    view.set_procs(sample());
    assert_eq!(view.selected_pid(), None);

    // `s` keeps WindowServer ([631, 2301, 77]), `sa` doesn't, and it isn't back with the row
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "s");
    assert_eq!(view.selected_pid(), Some(631));
    type_str(&mut view, "a");
    assert_eq!((pids(&view), view.selected_pid()), (vec![2301, 77], None));
    assert!(press(&mut view, KeyCode::Backspace));
    assert_eq!(view.selected_pid(), None);
  }

  #[test]
  fn esc_clears_selection_and_filter() {
    let mut view = view();
    assert!(press(&mut view, KeyCode::Char('/')));
    type_str(&mut view, "saf");
    assert!(press(&mut view, KeyCode::Enter));
    assert!(press(&mut view, KeyCode::Down));
    assert!(press(&mut view, KeyCode::End));
    view.fit(1);
    assert_eq!((view.selected_pid(), view.offset), (Some(77), 1));

    // both at once, and the table back at its top
    assert!(press(&mut view, KeyCode::Esc));
    assert_eq!((view.selected_pid(), view.filter(), view.offset), (None, "", 0));
    assert_eq!(pids(&view).len(), 5);
    assert!(!press(&mut view, KeyCode::Esc), "nothing left to clear: a global key");
  }

  #[test]
  fn up_and_down_wrap_around_the_other_keys_and_the_wheel_stop_at_the_ends() {
    use KeyCode::{Down, End, Home, PageDown, PageUp, Up};
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    let page = |view: &mut ProcView| {
      view.fit(2);
      view.page_rows().map(|(_, p)| p.pid).collect::<Vec<_>>()
    };
    // without a selection ↑ selects the top row on screen, as ↓ does
    assert!(press(&mut view, Up));
    assert_eq!(view.selected_pid(), Some(4410));
    let keys = [(Up, 77), (PageDown, 77), (End, 77), (Down, 4410), (PageUp, 4410), (Home, 4410)];
    for (code, pid) in keys {
      assert!(press(&mut view, code));
      assert_eq!(view.selected_pid(), Some(pid), "{code:?}");
    }
    // the table follows a wrap to either end
    assert!(press(&mut view, Up));
    assert_eq!(page(&mut view), [1, 77]);
    assert!(press(&mut view, Down));
    assert_eq!(page(&mut view), [4410, 631]);

    view.wheel(-3);
    assert_eq!(view.selected_pid(), Some(4410));
    assert!(press(&mut view, End));
    view.wheel(3);
    assert_eq!(view.selected_pid(), Some(77));
  }

  #[test]
  fn scroll_keeps_selection_visible() {
    let mut view = view(); // [4410, 631, 2301, 1, 77]
    let page = |view: &mut ProcView| {
      view.fit(2);
      view.page_rows().map(|(sel, p)| (sel, p.pid)).collect::<Vec<_>>()
    };

    for _ in 0..3 {
      assert!(press(&mut view, KeyCode::Down));
    }
    assert_eq!(page(&mut view), [(false, 631), (true, 2301)]);
    // moving up inside the page doesn't scroll
    assert!(press(&mut view, KeyCode::Up));
    assert_eq!(page(&mut view), [(true, 631), (false, 2301)]);
    assert!(press(&mut view, KeyCode::End));
    assert_eq!(page(&mut view), [(false, 1), (true, 77)]);

    // a taller window shows everything
    view.fit(10);
    assert_eq!((view.offset, view.page_rows().count()), (0, 5));
  }

  #[test]
  fn columns_drop_at_narrow_widths_keeping_the_sorted_one() {
    use ProcSort::*;
    let names =
      |width, sort| fit_columns(width, sort).into_iter().map(|(c, _)| c).collect::<Vec<ProcSort>>();

    // NAME takes the room the others leave
    let all =
      [(Pid, 5), (Name, 145), (User, 10), (Time, 8), (Cpu, 6), (Mem, 6), (Gpu, 6), (Power, 7)];
    assert_eq!(fit_columns(200, Pid), all);
    // all of them in an 80 cells window (76 inside the box)
    assert_eq!(fit_columns(76, Pid)[1], (Name, 21));
    // TIME, then USER go before NAME gets fewer than 16 cells, the others (from the right) before
    // it gets fewer than 8
    assert_eq!(names(71, Pid), [Pid, Name, User, Time, Cpu, Mem, Gpu, Power]);
    assert_eq!(names(70, Pid), [Pid, Name, User, Cpu, Mem, Gpu, Power]);
    assert_eq!(names(62, Pid), [Pid, Name, User, Cpu, Mem, Gpu, Power]);
    assert_eq!(names(61, Pid), [Pid, Name, Cpu, Mem, Gpu, Power]);
    assert_eq!(names(42, Pid), [Pid, Name, Cpu, Mem, Gpu]);
    assert_eq!(names(34, Pid), [Pid, Name, Cpu, Mem]);
    assert_eq!(names(27, Pid), [Pid, Name, Cpu]);
    assert_eq!(names(20, Pid), [Pid, Name]);

    // the sorted column stays, another one goes in its place
    assert_eq!(names(40, User), [Pid, Name, User, Cpu, Mem]);
    assert_eq!(names(30, Power), [Pid, Name, Cpu, Power]);
    assert_eq!(names(14, Mem), [Pid, Name, Mem]);
    assert_eq!(names(30, Time), [Pid, Name, Time, Cpu]);
  }

  #[test]
  fn the_filter_keeps_its_place_in_the_title_as_the_count_changes() {
    // 1 `one`, 29 `two` and 294 `three` of 324 processes
    let names = [("one", 1), ("two", 29), ("three", 294)];
    let procs = names.iter().flat_map(|&(name, n)| (0..n).map(move |_| name));
    let procs = procs.enumerate().map(|(i, name)| proc(i as i32 + 1, name, 0.0, 1, None, 0.0));
    let mut view = ProcView::default();
    view.set_procs(procs.collect());

    // the top border of a box 80 cells wide, and the cell where the filter starts
    let title = |view: &ProcView| {
      let mut term = Terminal::new(TestBackend::new(80, 3)).unwrap();
      term.draw(|f| _ = draw_box(f, f.area(), view.titles(80))).unwrap();
      let buf = term.backend().buffer();
      let row: String = (0..80).map(|x| buf[(x, 0)].symbol()).collect();
      let at = row.find("─ /").map(|i| row[..i].chars().count());
      (row, at)
    };

    assert!(title(&view).0.starts_with("╭─ proc 324 ─ / filter ─ sort ─"));
    assert!(press(&mut view, KeyCode::Char('/')));
    let (row, open) = title(&view);
    assert!(row.starts_with("╭─ proc 324/324 ─ /█ ─"), "{row}");
    for (name, n) in names {
      type_str(&mut view, name);
      let (row, at) = title(&view);
      assert!(row.starts_with(&format!("╭─ proc {n:>3}/324 ─ /{name}█ ─")), "{row}");
      assert_eq!(at, open, "{row}");
      for _ in name.chars() {
        assert!(press(&mut view, KeyCode::Backspace));
      }
    }
  }

  #[test]
  fn cpu_time_reads_like_top() {
    let sec = |s: u64| s * 1_000_000_000;
    let cases = [
      (0, "0:00.00"),
      (9_999_999, "0:00.00"), // cut to hundredths
      (sec(1) + 970_000_000, "0:01.97"),
      (sec(59), "0:59.00"),
      (sec(3600) - 1, "59:59.99"),
      (sec(3600), "1:00:00"),
      (sec(86_399), "23:59:59"),
      (sec(86_400), "1d00h"),
      (sec(2 * 86_400 + 3 * 3600 + 59), "2d03h"),
      (u64::MAX, "213503d23h"), // no overflow
    ];
    for (ns, shown) in cases {
      assert_eq!(format_time(ns), shown, "{ns}");
    }
  }

  #[test]
  fn cut_text_ends_with_an_ellipsis() {
    assert_eq!(cut_end("WindowServer", 12), "WindowServer");
    assert_eq!(cut_end("WindowServer", 9), "WindowSe…");
    assert_eq!(cut_end("WindowServer", 1), "…");
    assert_eq!(cut_end("WindowServer", 0), "");
    // wide characters take two cells and are never split
    assert_eq!(cut_end("漢字テキスト", 5), "漢字…");

    assert_eq!(cut_start("/usr/libexec/foo", 16), "/usr/libexec/foo");
    assert_eq!(cut_start("/usr/libexec/foo", 8), "…xec/foo");
    assert_eq!(cut_start("/usr/libexec/foo", 0), "");

    // the start and the file name; from the start when the name doesn't fit
    let path = "/Users/vlad/Library/Caches/com.apple.Safari/Cache.db";
    assert_eq!(cut_middle(path, 52), path);
    assert_eq!(cut_middle(path, 30), "/Users/vlad/Library/…/Cache.db");
    assert_eq!(cut_middle(path, 11), "/…/Cache.db");
    assert_eq!(cut_middle(path, 10), "…/Cache.db");
    assert_eq!(cut_middle(path, 9), "…Cache.db");
    assert_eq!(cut_middle("127.0.0.1:8080 (listen)", 10), "… (listen)");
    assert_eq!(cut_middle(path, 0), "");
  }
}
