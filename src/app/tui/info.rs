//! The info overlay (`i`): the executable, working directory, open files and sockets of the
//! selected process, files grouped by directory, under a line counting the descriptors; `e`
//! reveals one.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;

use super::boxes::{draw_box, hint, render_bottom_border};
use super::kill::{ProcSys, failure};
use super::proc_view::{
  ELLIPSIS, Filter, cut_middle, filter_titles, scroll_offset, step, type_filter,
};
use super::theme::{self, faint, heading, text};
use crate::fds::{Kind, OpenFile};
use crate::procs::ProcInfo;

/// The kinds on the counts line, in its order, with their labels.
#[rustfmt::skip]
const COUNTS: [(Kind, &str); 9] = [
  (Kind::File, "files"), (Kind::Dir, "dirs"), (Kind::Dev, "dev"), (Kind::Tcp, "tcp"),
  (Kind::Udp, "udp"), (Kind::Unix, "unix"), (Kind::Pipe, "pipes"), (Kind::Kqueue, "kqueues"),
  (Kind::Other, "other"),
];
/// Fewest inner rows with the counts line and its divider; with fewer the list takes them.
const COUNTS_MIN_HEIGHT: u16 = 8;
/// The type and mode columns with their blank cells: ` file  rw  `.
const COLUMNS: usize = 11;

/// What a row is, so the selection follows it: kind, text, mode and whether it's a directory row.
type Key = (Kind, String, &'static str, bool);

/// A listed row: the entries with one kind, text and mode, or the directory of a group of files.
#[derive(Debug, Clone, PartialEq)]
struct Row {
  kind: Kind,
  mode: &'static str,
  /// The path or the socket text; a directory row has the directory with a trailing `/`.
  text: String,
  /// Entries behind it, `×N` from 2; 0 for a directory row.
  count: usize,
  dir: bool,
  /// A file under its directory row, shown by name.
  member: bool,
}

impl Row {
  fn key(&self) -> Key {
    (self.kind, self.text.clone(), self.mode, self.dir)
  }
}

/// The directory and the name of `path`: `/a/b` → (`/a`, `b`), `/b` → (``, `b`).
fn split(path: &str) -> (&str, &str) {
  path.rsplit_once('/').unwrap_or(("", path))
}

/// Sort order: by kind, then paths by directory and name (so a directory's files stay together
/// around its subdirectories), sockets by text, then the mode.
fn order(file: &OpenFile) -> (Kind, &str, &str, &'static str) {
  let (dir, name) =
    if file.kind <= Kind::Dev { split(&file.text) } else { ("", file.text.as_str()) };
  (file.kind, dir, name, file.mode)
}

/// `path` cut to `max` cells by components: the first 2 (else 1) and the longest tail of whole
/// components that fits around `…`, a trailing `/` kept; `cut_middle` when none fits.
fn cut_path(path: &str, max: usize) -> String {
  if Span::raw(path).width() <= max {
    return path.to_string();
  }
  let (body, slash) = path.strip_suffix('/').map_or((path, ""), |body| (body, "/"));
  let parts: Vec<&str> = body.split('/').collect();
  let root = usize::from(parts[0].is_empty());
  for head in [2, 1] {
    for tail in (1..parts.len().saturating_sub(root + head)).rev() {
      let (head, tail) = (parts[..root + head].join("/"), parts[parts.len() - tail..].join("/"));
      let cut = format!("{head}/{ELLIPSIS}/{tail}{slash}");
      if Span::raw(&cut).width() <= max {
        return cut;
      }
    }
  }
  cut_middle(path, max)
}

/// The info overlay of one process: its files (or why they can't be read), filter and selection.
#[derive(Debug)]
pub(super) struct Info {
  proc: ProcInfo,
  files: Result<Vec<OpenFile>, String>,
  filter: String,
  pub(super) typing: bool,
  /// The selected row, and its key, which it follows when the rows change.
  index: usize,
  key: Option<Key>,
  offset: usize,
  /// Rows on screen at the last render, the PgUp / PgDn step.
  page: usize,
  /// `$HOME`, shown as `~`.
  home: String,
}

impl Info {
  /// The overlay of `proc`, its first row selected.
  pub(super) fn new(proc: &ProcInfo, sys: &dyn ProcSys) -> Self {
    let (proc, files, filter) = (proc.clone(), Ok(vec![]), String::new());
    let home = std::env::var("HOME").unwrap_or_default();
    let (index, key, offset, page) = (0, None, 0, 0);
    let mut info = Self { proc, files, filter, typing: false, index, key, offset, page, home };
    info.refresh(sys);
    info
  }

  /// Reads the files again. A pid reused since the sample (another start time) reads as exited, a
  /// `ps` row (no start time) as not permitted, as kill takes them.
  pub(super) fn refresh(&mut self, sys: &dyn ProcSys) {
    let (pid, started) = (self.proc.pid, self.proc.started);
    let files = sys.open_files(pid).and_then(|files| {
      let started = started.ok_or(libc::EPERM)?;
      if sys.identity(pid) == Some(started) { Ok(files) } else { Err(libc::ESRCH) }
    });
    self.files = files.map_err(failure);
    self.follow();
  }

  /// `path` with the home directory as `~`.
  fn tilde(&self, path: &str) -> String {
    let home = self.home.trim_end_matches('/');
    match path.strip_prefix(home) {
      Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
        format!("~{rest}")
      }
      _ => path.to_string(),
    }
  }

  /// The rows: the executable, then the files with a text, but pipes and kqueues, passing the
  /// filter (its terms in the type, the path or its `~` form), sorted, equal ones collapsed, and
  /// 2 or more files in one directory under a directory row.
  fn rows(&self) -> Vec<Row> {
    let Ok(files) = &self.files else { return vec![] };
    let filter = Filter::parse(&self.filter);
    let hit = |f: &OpenFile, t: &str| {
      let text = [f.text.to_lowercase(), self.tilde(&f.text).to_lowercase()];
      f.kind.label().contains(t) || text.iter().any(|text| text.contains(t))
    };
    let exe = OpenFile { fd: None, kind: Kind::Exe, text: self.proc.path.clone(), mode: "" };
    let mut entries: Vec<&OpenFile> = (std::iter::once(&exe).chain(files))
      .filter(|f| !f.text.is_empty() && !matches!(f.kind, Kind::Pipe | Kind::Kqueue))
      .filter(|f| filter.matches(|term| hit(f, term)))
      .collect();
    entries.sort_by_key(|f| order(f));

    let mut collapsed: Vec<(&OpenFile, usize)> = vec![];
    for f in entries {
      match collapsed.last_mut() {
        Some((last, n)) if (last.kind, &last.text, last.mode) == (f.kind, &f.text, f.mode) => {
          *n += 1
        }
        _ => collapsed.push((f, 1)),
      }
    }

    let mut rows = vec![];
    let files_in_one_dir = |(a, _): &(&OpenFile, usize), (b, _): &(&OpenFile, usize)| {
      a.kind == Kind::File && b.kind == Kind::File && split(&a.text).0 == split(&b.text).0
    };
    for run in collapsed.chunk_by(files_in_one_dir) {
      let member = run[0].0.text != run[run.len() - 1].0.text;
      if member {
        let text = format!("{}/", split(&run[0].0.text).0);
        rows.push(Row { kind: Kind::File, mode: "", text, count: 0, dir: true, member: false });
      }
      rows.extend(run.iter().map(|&(f, count)| {
        let (kind, mode, text) = (f.kind, f.mode, f.text.clone());
        Row { kind, mode, text, count, dir: false, member }
      }));
    }
    rows
  }

  /// Selects row `index`, clamped to the rows.
  fn select(&mut self, index: usize) {
    let rows = self.rows();
    self.index = index.min(rows.len().saturating_sub(1));
    if let Some(row) = rows.get(self.index) {
      self.key = Some(row.key());
    }
  }

  /// After the rows changed: the selection stays on its key while it is listed.
  fn follow(&mut self) {
    let at = self.rows().iter().position(|row| self.key.as_ref() == Some(&row.key()));
    self.select(at.unwrap_or(self.index));
  }

  /// A key: navigation ends filter input, the filter keys are those of the process list, `e`
  /// reveals the selected exe, cwd, directory or file, Esc clears the filter; `false` when Esc or
  /// `q` (no modifier, not typed into the filter) closes the overlay, as they close the other
  /// popups.
  pub(super) fn handle_key(&mut self, key: KeyEvent, sys: &dyn ProcSys) -> bool {
    if let Some(index) = step(key.code, self.index, self.page.max(1)) {
      self.typing = false;
      self.select(index);
      return true;
    }
    match key.code {
      _ if self.typing => {
        if type_filter(&mut self.filter, &mut self.typing, key) {
          self.follow();
        }
      }
      KeyCode::Char('/') => self.typing = true,
      KeyCode::Char('e') => {
        // exe, cwd, dir and file rows (directory rows are file rows), not devices and sockets
        if let Some(row) = self.rows().get(self.index).filter(|row| row.kind <= Kind::File) {
          sys.reveal(&row.text);
        }
      }
      KeyCode::Esc if !self.filter.is_empty() => {
        self.filter.clear();
        self.follow();
      }
      KeyCode::Esc => return false,
      KeyCode::Char('q') if key.modifiers.is_empty() => return false,
      _ => {}
    }
    true
  }

  /// The wheel moves the selection as ↑ / ↓ do; other mouse events do nothing.
  pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, sys: &dyn ProcSys) {
    let up = mouse.kind == MouseEventKind::ScrollUp;
    if up || mouse.kind == MouseEventKind::ScrollDown {
      self.handle_key(KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down }), sys);
    }
  }

  /// `116 fds  8 files  3 dev …` in `width` cells: the descriptors (`shown/total` with a filter,
  /// shown being those behind `rows`), then the count of each kind of them; pairs that don't fit
  /// drop from the end.
  fn counts(&self, rows: &[Row], width: usize) -> Line<'static> {
    let fds: Vec<&OpenFile> = self.files.iter().flatten().filter(|f| f.fd.is_some()).collect();
    let shown = rows.iter().filter(|row| row.kind > Kind::Cwd).map(|row| row.count).sum::<usize>();
    let total = match self.filter.is_empty() {
      true => fds.len().to_string(),
      false => format!("{shown}/{}", fds.len()),
    };
    let kinds =
      COUNTS.iter().map(|&(kind, label)| (fds.iter().filter(|f| f.kind == kind).count(), label));
    let kinds = kinds.filter(|&(n, _)| n > 0).map(|(n, label)| (n.to_string(), label));

    let (mut spans, mut used) = (vec![Span::raw(" ")], 2); // a blank cell at both borders
    for (i, (n, label)) in std::iter::once((total, "fds")).chain(kinds).enumerate() {
      let n = if i == 0 { n } else { format!("  {n}") };
      used += n.len() + 1 + label.len();
      if used > width {
        break;
      }
      spans.extend([text(n), faint(format!(" {label}"))]);
    }
    Line::from(spans)
  }

  /// Draws the overlay over `area` (the process box, never empty): `<name> <pid>`, the filter and
  /// `finder` (not while typing) on the top border, the counts line and a divider when there is
  /// room, the rows (paths cut by components) or why there are none, and the filter keys on the
  /// bottom border while typing.
  pub(super) fn render(&mut self, f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let name = vec![heading(self.proc.name.clone()), text(format!(" {}", self.proc.pid))];
    let titles = filter_titles(name, &self.filter, self.typing, area.width);
    let titles = if self.typing { titles } else { titles.right(hint("e", "finder")) };
    let mut list = draw_box(f, area, titles);
    let rows = self.rows();
    if let Err(error) = &self.files {
      f.render_widget(Line::from(faint(format!(" {error}"))), list);
    } else if list.height >= COUNTS_MIN_HEIGHT {
      f.render_widget(self.counts(&rows, usize::from(list.width)), list);
      let divider = format!("├{}┤", "─".repeat(usize::from(area.width.saturating_sub(2))));
      f.buffer_mut().set_string(area.x, list.y + 1, divider, theme::BORDER);
      (list.y, list.height) = (list.y + 2, list.height - 2);
    }
    self.page = usize::from(list.height);
    self.offset = scroll_offset(self.offset, Some(self.index), self.page, rows.len());

    let rows = rows.iter().enumerate().skip(self.offset);
    for (y, (i, row)) in (list.y..list.bottom()).zip(rows) {
      // the columns and `×N` faint unless selected, a blank cell at both borders
      let side = |s: String| if i == self.index { text(s) } else { faint(s) };
      let columns = match row.dir {
        true => " ".repeat(COLUMNS),
        false => format!(" {:5} {:2}  ", row.kind.label(), row.mode),
      };
      let count = if row.count > 1 { format!("  ×{}", row.count) } else { String::new() };
      let room = usize::from(list.width).saturating_sub(COLUMNS + 1 + Span::raw(&count).width());
      let shown = match row.kind {
        _ if row.member => format!("  {}", cut_path(split(&row.text).1, room.saturating_sub(2))),
        kind if kind > Kind::Dev => cut_middle(&self.tilde(&row.text), room), // sockets
        _ => cut_path(&self.tilde(&row.text), room),
      };
      let line = Rect { y, height: 1, ..list };
      f.render_widget(Line::from(vec![side(columns), text(shown), side(count)]), line);
      if i == self.index {
        f.buffer_mut().set_style(line, theme::SELECTED);
      }
    }

    if self.typing {
      let hints = vec![hint("Enter", "keep"), hint("Esc", "clear"), hint("↑↓", "select")];
      render_bottom_border(f, area, hints, |_| vec![]);
    }
  }
}

#[cfg(test)]
mod tests {
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
  use ratatui::style::Modifier;

  use super::{Info, cut_path};
  use crate::fds::{Kind, OpenFile};
  use crate::procs::{self, ProcInfo};
  use crate::tui::kill::tests::FakeSys;
  use crate::tui::kill::{LibcSys, ProcSys};
  use crate::tui::theme;

  /// A file open on `fd` (negative: the cwd) with `mode`.
  fn file(fd: i32, kind: Kind, mode: &'static str, text: &str) -> OpenFile {
    OpenFile { fd: (fd >= 0).then_some(fd), kind, text: text.into(), mode }
  }

  const EXE: &str = "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer";
  const LOG: &str = "/Users/vlad/Library/Application Support/Google/Chrome/Default/IndexedDB/\
                     https_web.whatsapp.com_0.indexeddb.leveldb/000003.log";

  fn files() -> Vec<OpenFile> {
    vec![
      file(-1, Kind::Cwd, "", "/Users/vlad/Code/macmon"),
      file(0, Kind::Dev, "rw", "/dev/ttys004"),
      file(3, Kind::File, "r", LOG),
      file(4, Kind::Tcp, "", "127.0.0.1:52344 → 140.82.112.4:443 (established)"),
      file(5, Kind::Tcp, "", "[::1]:8080 (listen)"),
      file(6, Kind::Udp, "", "0.0.0.0:5353"),
      file(7, Kind::Unix, "", "/var/run/mDNSResponder"),
      file(8, Kind::Pipe, "", ""),
      file(9, Kind::Kqueue, "", ""),
    ]
  }

  /// The rows of `files()`, in order.
  const ROWS: [&str; 8] = [
    EXE,
    "/Users/vlad/Code/macmon",
    LOG,
    "/dev/ttys004",
    "127.0.0.1:52344 → 140.82.112.4:443 (established)",
    "[::1]:8080 (listen)",
    "0.0.0.0:5353",
    "/var/run/mDNSResponder",
  ];

  const CHROME: &str = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome \
                        Framework.framework/Versions/131.0.6778.86";

  /// A Chrome renderer: the resources two files share, a file open twice, /dev/null three times.
  fn chrome_files() -> Vec<OpenFile> {
    let rules = "/Users/vlad/Library/Application Support/Google/Chrome/Subresource Filter/\
                 Indexed Rules/37/9.57.0/Ruleset Data";
    let mut files = vec![
      file(-1, Kind::Cwd, "", "/"),
      file(0, Kind::Dev, "rw", "/dev/null"),
      file(1, Kind::Dev, "rw", "/dev/null"),
      file(2, Kind::Dev, "rw", "/dev/null"),
      file(3, Kind::File, "r", &format!("{CHROME}/Resources/icudtl.dat")),
      file(4, Kind::File, "r", rules),
      file(5, Kind::Kqueue, "", ""),
      file(6, Kind::File, "r", &format!("{CHROME}/Resources/chrome_100_percent.pak")),
      file(7, Kind::Unix, "", "/var/run/mDNSResponder"),
      file(8, Kind::File, "r", rules),
      file(
        9,
        Kind::File,
        "rw",
        "/Users/vlad/Library/Application Support/Google/Chrome/Default/\
                                 Shared Dictionary/db",
      ),
      file(10, Kind::Tcp, "", "192.168.1.20:50122 → 140.82.112.4:443 (established)"),
      file(11, Kind::Unix, "", ""),
    ];
    files.extend((12..16).map(|fd| file(fd, Kind::Pipe, "", "")));
    files
  }

  /// A fake with process 631, started at second 631 as `FakeSys::kill` has it, and its `files`.
  fn fake(files: Result<Vec<OpenFile>, i32>) -> FakeSys {
    let (_, fake) = FakeSys::kill(&[631]);
    fake.0.borrow_mut().files.insert(631, files);
    fake
  }

  /// WindowServer 631 as sampled.
  fn proc() -> ProcInfo {
    ProcInfo {
      pid: 631,
      name: "WindowServer".into(),
      path: EXE.into(),
      user: "vlad".into(),
      cpu_pct: 0.0,
      mem_bytes: 0,
      power_w: None,
      gpu_pct: 0.0,
      started: Some((631, 0)),
    }
  }

  /// The overlay of `proc` with `files` over a fake, for user vlad, drawn once 80x8 (6 rows on
  /// screen, no counts line).
  fn overlay(proc: ProcInfo, files: Result<Vec<OpenFile>, i32>) -> (Info, FakeSys) {
    let fake = fake(files);
    let mut info = Info::new(&proc, &fake);
    info.home = "/Users/vlad".into();
    render(&mut info, 80, 8);
    (info, fake)
  }

  fn info(files: Result<Vec<OpenFile>, i32>) -> (Info, FakeSys) {
    overlay(proc(), files)
  }

  /// The Chrome renderer's overlay.
  fn chrome() -> (Info, FakeSys) {
    let path = format!(
      "{CHROME}/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/\
                        Google Chrome Helper (Renderer)"
    );
    let name = "Google Chrome Helper (Renderer)".into();
    overlay(ProcInfo { name, path, ..proc() }, Ok(chrome_files()))
  }

  fn press(info: &mut Info, fake: &FakeSys, code: KeyCode) -> bool {
    info.handle_key(KeyEvent::new(code, KeyModifiers::NONE), fake)
  }

  fn type_str(info: &mut Info, fake: &FakeSys, text: &str) {
    text.chars().for_each(|c| assert!(press(info, fake, KeyCode::Char(c))));
  }

  fn texts(info: &Info) -> Vec<String> {
    info.rows().into_iter().map(|row| row.text).collect()
  }

  /// The text of the selected row.
  fn selected(info: &Info) -> String {
    texts(info).swap_remove(info.index)
  }

  fn render(info: &mut Info, width: u16, height: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| info.render(f, f.area())).unwrap();
    term.backend().buffer().clone()
  }

  fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect()).collect()
  }

  fn revealed(fake: &FakeSys) -> Vec<String> {
    fake.0.borrow().revealed.clone()
  }

  #[test]
  fn keys_and_the_wheel_move_a_selection_that_stays_in_the_list() {
    use KeyCode::{Down, End, Home, PageDown, PageUp, Up};
    let (mut info, fake) = info(Ok(files()));
    assert_eq!(texts(&info), ROWS);
    let wheel = |info: &mut Info, kind| {
      let mouse = MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE };
      info.handle_mouse(mouse, &fake);
      info.index
    };
    // the first row, the exe, selected; a page is the 6 rows on screen
    assert_eq!(selected(&info), EXE);
    let steps = [(Up, 0), (Down, 1), (PageDown, 7), (PageDown, 7), (Down, 7), (PageUp, 1)];
    for (code, index) in steps.into_iter().chain([(End, 7), (Home, 0)]) {
      assert!(press(&mut info, &fake, code));
      assert_eq!(info.index, index, "{code:?}");
    }
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollDown), 1);
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollDown), 2);
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollUp), 1);
    assert_eq!(wheel(&mut info, MouseEventKind::Moved), 1);

    // the list scrolls to keep the selection on screen
    assert!(press(&mut info, &fake, End));
    let shown = rows(&render(&mut info, 80, 8));
    assert!(shown[1].starts_with("│ file  r   ~/Library/") && shown[6].starts_with("│ unix "));
  }

  #[test]
  fn the_filter_matches_the_type_the_path_and_its_home_form_and_ends_with_navigation() {
    let (mut info, fake) = info(Ok(files()));
    let filter = |text: &str| {
      let (mut info, fake) = self::info(Ok(files()));
      assert!(press(&mut info, &fake, KeyCode::Char('/')));
      type_str(&mut info, &fake, text);
      texts(&info)
    };
    assert_eq!(filter("TCP, /var"), [ROWS[4], ROWS[5], ROWS[7]]);
    // pipes and kqueues match but aren't listed
    assert_eq!(filter("!/, !tcp"), [ROWS[6]]);
    assert_eq!(filter("cwd"), [ROWS[1]]);
    // `exe` is in `indexeddb` too
    assert_eq!(filter("exe, dev"), [ROWS[0], ROWS[2], ROWS[3]]);
    assert_eq!(filter("~/library"), [LOG]);
    assert_eq!(filter("nothing"), Vec::<String>::new());

    // the selection, filtered out, goes to the row in its place; while typing no `finder` on top,
    // the filter keys on the bottom border; ↓ ends the input, `e` and Esc act again
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp, unix");
    assert_eq!((info.typing, selected(&info)), (true, ROWS[4].into()));
    let buf = render(&mut info, 80, 8);
    assert!(rows(&buf)[0].starts_with("╭─ WindowServer 631 ─ /tcp, unix█ ──"), "{}", rows(&buf)[0]);
    assert!(rows(&buf)[0].ends_with("──╮"), "{}", rows(&buf)[0]);
    assert!(
      rows(&buf)[7].ends_with("─ Enter keep | Esc clear | ↑↓ select ─╯"),
      "{}",
      rows(&buf)[7]
    );
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert_eq!(
      (info.typing, info.filter.as_str(), selected(&info)),
      (false, "tcp, unix", ROWS[5].into())
    );
    let buf = render(&mut info, 80, 8);
    assert!(rows(&buf)[0].starts_with("╭─ WindowServer 631 ─ /tcp, unix ──"), "{}", rows(&buf)[0]);
    assert!(rows(&buf)[0].ends_with("── finder ─╮"), "{}", rows(&buf)[0]);
    assert_eq!(rows(&buf)[7], format!("╰{}╯", "─".repeat(78)));
  }

  #[test]
  fn e_reveals_the_exe_the_cwd_directories_and_files_only() {
    // a file the kernel gives no path for isn't listed
    let mut files = files();
    files.push(file(10, Kind::File, "r", ""));
    files.push(file(11, Kind::Dir, "r", "/Users/vlad/Downloads"));
    let (mut info, fake) = info(Ok(files.clone()));
    // `e` on every row: not the device and the sockets; `f` nothing
    for _ in 0..files.len() {
      assert!(press(&mut info, &fake, KeyCode::Char('f')));
      assert!(press(&mut info, &fake, KeyCode::Char('e')));
      assert!(press(&mut info, &fake, KeyCode::Down));
    }
    assert_eq!(revealed(&fake), [EXE, "/Users/vlad/Code/macmon", "/Users/vlad/Downloads", LOG]);

    // typed, it is text
    assert!(press(&mut info, &fake, KeyCode::Home));
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "e");
    assert_eq!((info.filter.as_str(), revealed(&fake).len()), ("e", 4));

    // a directory row reveals the directory
    let (mut info, fake) = chrome();
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert!(press(&mut info, &fake, KeyCode::Char('e')));
    assert_eq!(revealed(&fake), [format!("{CHROME}/Resources/")]);
  }

  #[test]
  fn esc_ends_input_clearing_the_filter_then_clears_it_then_closes() {
    let (mut info, fake) = info(Ok(files()));
    let typed = |info: &Info| (info.typing, info.filter.clone(), info.rows().len());
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp");
    assert!(press(&mut info, &fake, KeyCode::Esc));
    assert_eq!(typed(&info), (false, String::new(), 8));

    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp");
    assert!(press(&mut info, &fake, KeyCode::Enter));
    assert_eq!(typed(&info), (false, "tcp".into(), 2));
    assert!(press(&mut info, &fake, KeyCode::Esc));
    assert_eq!(typed(&info), (false, String::new(), 8));
    assert!(!press(&mut info, &fake, KeyCode::Esc), "closes");
  }

  #[test]
  fn q_closes_unless_typed_into_the_filter() {
    let (mut info, fake) = info(Ok(files()));
    // with a modifier or in upper case it does nothing
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::SUPER] {
      assert!(info.handle_key(KeyEvent::new(KeyCode::Char('q'), modifiers), &fake));
    }
    assert!(press(&mut info, &fake, KeyCode::Char('Q')));
    // typed, it is text; then it closes, the filter set or not
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "q");
    assert_eq!((info.typing, info.filter.as_str()), (true, "q"));
    assert!(press(&mut info, &fake, KeyCode::Enter));
    assert!(!press(&mut info, &fake, KeyCode::Char('q')), "closes");
    let (mut info, fake) = self::info(Ok(files()));
    assert!(!press(&mut info, &fake, KeyCode::Char('q')), "closes");
  }

  #[test]
  fn a_refresh_keeps_the_selection_on_its_row() {
    let (mut info, fake) = chrome();
    let set = |files: Vec<OpenFile>| fake.0.borrow_mut().files.insert(631, Ok(files));
    let at = |info: &mut Info, text: &str| {
      let index = texts(info).iter().position(|t| t.ends_with(text)).unwrap();
      info.select(index);
    };
    // on the file open twice: one fd closed, still on it, now once
    at(&mut info, "Ruleset Data");
    let mut files = chrome_files();
    files.retain(|file| file.fd != Some(8));
    set(files.clone());
    info.refresh(&fake);
    assert!(selected(&info).ends_with("Ruleset Data") && info.rows()[info.index].count == 1);

    // on the directory row: its key is the directory, not the fds
    at(&mut info, "Resources/");
    let index = info.index;
    files.retain(|file| file.fd != Some(3));
    files.push(file(16, Kind::File, "r", &format!("{CHROME}/Resources/v8_context_snapshot.bin")));
    set(files.clone());
    info.refresh(&fake);
    assert_eq!((selected(&info), info.index), (format!("{CHROME}/Resources/"), index));
    // the group gone, the row in its place: the first file, its full path back
    files.retain(|file| file.fd != Some(16));
    set(files);
    info.refresh(&fake);
    let pak = format!("{CHROME}/Resources/chrome_100_percent.pak");
    assert_eq!((selected(&info), info.index), (pak, index));
    assert!(!info.rows()[index].member);

    // the exe too has its own key
    assert!(press(&mut info, &fake, KeyCode::Home));
    set(chrome_files());
    info.refresh(&fake);
    assert_eq!(info.rows()[info.index].kind, Kind::Exe);
  }

  #[test]
  fn processes_that_cant_be_read_show_why() {
    let (left, reused) = (Some((631, 0)), Some((2000, 0)));
    // the files, the start time sampled, the text
    let cases = [
      (Err(libc::EPERM), left, " Not permitted"),
      (Err(libc::ESRCH), left, " Already exited"),
      // a pid reused since the sample, a `ps` row
      (Ok(files()), reused, " Already exited"),
      (Ok(files()), None, " Not permitted"),
    ];
    for (files, started, text) in cases {
      let fake = fake(files);
      let mut info = Info::new(&ProcInfo { started, ..proc() }, &fake);
      let buf = render(&mut info, 40, 12);
      let shown = rows(&buf);
      assert_eq!(shown[1], format!("│{text:38}│"));
      assert_eq!((buf[(2, 1)].fg, buf[(2, 1)].modifier), (theme::TEXT, Modifier::DIM), "{text}");
      assert_eq!(shown[2], format!("│{:38}│", ""), "no counts line");
      assert_eq!(shown[11], format!("╰{}╯", "─".repeat(38)));
      // nothing to select or reveal
      for code in [KeyCode::Down, KeyCode::End, KeyCode::Char('e')] {
        assert!(press(&mut info, &fake, code));
      }
      assert_eq!((info.index, revealed(&fake)), (0, Vec::<String>::new()), "{text}");
    }

    // exited while open
    let (mut info, fake) = info(Ok(files()));
    fake.0.borrow_mut().files.clear();
    info.refresh(&fake);
    assert_eq!(rows(&render(&mut info, 40, 5))[1], format!("│{:38}│", " Already exited"));
  }

  #[test]
  fn files_group_under_their_directory_and_duplicates_collapse() {
    let (mut info, _fake) = chrome();
    let buf = render(&mut info, 100, 16);
    let overlay = [
      "╭─ Google Chrome Helper (Renderer) 631 ─ / filter ──────────────────────────────────────── finder ─╮",
      "│ 16 fds  5 files  3 dev  1 tcp  2 unix  4 pipes  1 kqueues                                        │",
      "├──────────────────────────────────────────────────────────────────────────────────────────────────┤",
      "│ exe       /Applications/Google Chrome.app/…/Contents/MacOS/Google Chrome Helper (Renderer)       │",
      "│ cwd       /                                                                                      │",
      "│           /Applications/Google Chrome.app/…/Versions/131.0.6778.86/Resources/                    │",
      "│ file  r     chrome_100_percent.pak                                                               │",
      "│ file  r     icudtl.dat                                                                           │",
      "│ file  rw  ~/Library/Application Support/Google/Chrome/Default/Shared Dictionary/db               │",
      "│ file  r   ~/Library/…/Google/Chrome/Subresource Filter/Indexed Rules/37/9.57.0/Ruleset Data  ×2  │",
      "│ dev   rw  /dev/null  ×3                                                                          │",
      "│ tcp       192.168.1.20:50122 → 140.82.112.4:443 (established)                                    │",
      "│ unix      /var/run/mDNSResponder                                                                 │",
      "│                                                                                                  │",
      "│                                                                                                  │",
      "╰──────────────────────────────────────────────────────────────────────────────────────────────────╯",
    ];
    assert_eq!(rows(&buf), overlay);
    // `×N` faint like the type and the mode, the paths and the directory plain, the exe selected
    let cell = |x: u16, y: u16| (buf[(x, y)].fg, buf[(x, y)].modifier);
    let (plain, faint) = ((theme::TEXT, Modifier::empty()), (theme::TEXT, Modifier::DIM));
    let x = overlay[9].chars().position(|c| c == '×').unwrap() as u16;
    assert_eq!([cell(2, 9), cell(8, 9), cell(12, 9), cell(x, 9)], [faint, faint, plain, faint]);
    assert_eq!([cell(12, 5), cell(14, 6)], [plain, plain]);
    assert_eq!(cell(2, 3), (theme::TEXT, Modifier::REVERSED));
  }

  #[test]
  fn a_file_alone_in_its_directory_and_one_left_by_the_filter_show_their_full_path() {
    let (mut info, fake) = chrome();
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "icudtl");
    let rows = info.rows();
    assert_eq!(texts(&info), [format!("{CHROME}/Resources/icudtl.dat")]);
    assert!(!rows[0].member && !rows[0].dir);
    let shown = self::rows(&render(&mut info, 100, 16));
    assert!(
      shown[3].starts_with(
        "│ file  r   /Applications/Google Chrome.app/…/Versions/131.0.6778.86/Resources/icudtl.dat "
      ),
      "{}",
      shown[3]
    );
    // 1 of the 16 descriptors listed; the kinds stay totals
    assert!(shown[1].starts_with("│ 1/16 fds  5 files  3 dev "), "{}", shown[1]);
  }

  #[test]
  fn paths_are_cut_by_components() {
    let path = "/Applications/Google Chrome.app/Contents/Frameworks/Resources/icudtl.dat";
    let cuts = [
      (80, path),
      (65, "/Applications/Google Chrome.app/…/Frameworks/Resources/icudtl.dat"),
      (64, "/Applications/Google Chrome.app/…/Resources/icudtl.dat"),
      (50, "/Applications/Google Chrome.app/…/icudtl.dat"),
      // then 1 head component, then cut in the middle
      (30, "/Applications/…/icudtl.dat"),
      (20, "/Applica…/icudtl.dat"),
    ];
    for (max, cut) in cuts {
      assert_eq!(cut_path(path, max), cut, "{max}");
    }
    // the home form, a trailing `/`, a relative name
    assert_eq!(cut_path("~/Library/Application Support/Google/", 30), "~/Library/…/Google/");
    assert_eq!(cut_path("~/a/b/c", 6), "~/…/c");
    assert_eq!(cut_path("chrome_100_percent.pak", 10), "…rcent.pak");
  }

  #[test]
  fn the_counts_line_shows_with_room_and_drops_pairs_that_dont_fit() {
    let (mut info, _fake) = chrome();
    // below 8 inner rows the list takes its place
    assert!(rows(&render(&mut info, 100, 9))[1].starts_with("│ exe "));
    let shown = rows(&render(&mut info, 100, 10));
    assert!(shown[1].starts_with("│ 16 fds  5 files ") && shown[2].starts_with("├───"));
    assert!(shown[3].starts_with("│ exe "));
    let shown = rows(&render(&mut info, 41, 10));
    assert_eq!(shown[1], "│ 16 fds  5 files  3 dev  1 tcp  2 unix │");
    let shown = rows(&render(&mut info, 40, 10));
    assert_eq!(shown[1], "│ 16 fds  5 files  3 dev  1 tcp        │");
    // the numbers plain, the labels faint, the divider in the border color
    let buf = render(&mut info, 40, 10);
    let cell = |x: u16, y: u16| (buf[(x, y)].fg, buf[(x, y)].modifier);
    let (plain, faint) = ((theme::TEXT, Modifier::empty()), (theme::TEXT, Modifier::DIM));
    assert_eq!([cell(2, 1), cell(5, 1), cell(10, 1), cell(12, 1)], [plain, faint, plain, faint]);
    assert_eq!([cell(0, 2).0, cell(20, 2).0, cell(39, 2).0], [theme::BORDER; 3]);
  }

  #[test]
  fn the_overlay_draws_in_any_window() {
    let (mut chrome, _) = chrome();
    for (width, height) in (1..110).flat_map(|w| (1..14).map(move |h| (w, h))) {
      render(&mut chrome, width, height);
    }
    let (mut info, fake) = info(Ok(files()));
    for (width, height) in (1..70).flat_map(|w| (1..12).map(move |h| (w, h))) {
      render(&mut info, width, height);
    }
    // `finder` on the top border, nothing on the bottom one
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert!(press(&mut info, &fake, KeyCode::Down));
    let buf = render(&mut info, 60, 8);
    let overlay = [
      "╭─ WindowServer 631 ─ / filter ─────────────────── finder ─╮",
      "│ exe       /System/Library/…/Resources/WindowServer       │",
      "│ cwd       ~/Code/macmon                                  │",
      "│ file  r   ~/Library/…/000003.log                         │",
      "│ dev   rw  /dev/ttys004                                   │",
      "│ tcp       ….0.0.1:52344 → 140.82.112.4:443 (established) │",
      "│ tcp       [::1]:8080 (listen)                            │",
      "╰──────────────────────────────────────────────────────────╯",
    ];
    let shown = rows(&buf);
    assert_eq!(shown, overlay);
    // the type and mode faint, the path in the default color, the selected row plain text
    // reversed from border to border, the key of the hint cyan, its label faint
    let cell = |x: u16, y: u16| (buf[(x, y)].fg, buf[(x, y)].modifier);
    let (plain, faint) = ((theme::TEXT, Modifier::empty()), (theme::TEXT, Modifier::DIM));
    assert_eq!([cell(2, 4), cell(8, 4), cell(12, 4)], [faint, faint, plain]);
    assert_eq!([cell(1, 3), cell(2, 3), cell(58, 3)], [(theme::TEXT, Modifier::REVERSED); 3]);
    let x = |text| shown[0][..shown[0].find(text).unwrap()].chars().count() as u16;
    let key = (theme::KEY, Modifier::empty());
    let finder = x("finder");
    assert_eq!([0, 3, 4, 5].map(|dx| cell(finder + dx, 0)), [faint, faint, key, faint]);
  }

  /// The only test with the real calls: the test's own process, and no reveal.
  #[test]
  fn lists_the_own_process_with_the_real_calls() {
    let pid = std::process::id() as i32;
    let info = procs::bsd_info(pid).unwrap();
    let started = Some((info.pbi_start_tvsec, info.pbi_start_tvusec));
    assert_eq!(LibcSys.identity(pid), started);
    let info = Info::new(&ProcInfo { pid, started, ..proc() }, &LibcSys);
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    assert_eq!(info.files.unwrap()[0], OpenFile { fd: None, kind: Kind::Cwd, text: cwd, mode: "" });
  }
}
