//! Open files and sockets of the selected process (`i`) in an overlay over the process list; `o`
//! shows the selected file in Finder.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Clear;

use super::boxes::{draw_box, hint, render_bottom_border};
use super::kill::{KillSys, failure};
use super::proc_view::{
  Filter, cut_middle, cut_start, filter_titles, scroll_offset, step, type_filter,
};
use super::theme::{self, dim, heading, text};
use crate::fds::{Kind, OpenFile};
use crate::procs::ProcInfo;

/// The info overlay (`i`) of one process: its files or why they can't be read, the filter and the
/// selection. It goes through the system calls of the kill popup.
#[derive(Debug)]
pub(super) struct Info {
  proc: ProcInfo,
  files: Result<Vec<OpenFile>, String>,
  filter: String,
  pub(super) typing: bool,
  /// The selected row, and its fd (`None`: the cwd), which it follows when the rows change.
  index: usize,
  fd: Option<i32>,
  offset: usize,
  /// Rows on screen at the last render, the PgUp / PgDn step.
  page: usize,
}

impl Info {
  /// The overlay of `proc`, its first row selected.
  pub(super) fn new(proc: &ProcInfo, sys: &dyn KillSys) -> Self {
    let (proc, files, filter) = (proc.clone(), Ok(vec![]), String::new());
    let mut info =
      Self { proc, files, filter, typing: false, index: 0, fd: None, offset: 0, page: 0 };
    info.refresh(sys);
    info
  }

  /// Reads the files again. A pid reused since the sample (another start time) reads as exited, a
  /// `ps` row (no start time) as not permitted, as kill takes them.
  pub(super) fn refresh(&mut self, sys: &dyn KillSys) {
    let (pid, started) = (self.proc.pid, self.proc.started);
    let files = sys.open_files(pid).and_then(|files| {
      let started = started.ok_or(libc::EPERM)?;
      if sys.identity(pid) == Some(started) { Ok(files) } else { Err(libc::ESRCH) }
    });
    self.files = files.map_err(failure);
    self.follow();
  }

  /// The files passing the filter: its terms in the type or the path.
  fn rows(&self) -> Vec<&OpenFile> {
    let filter = Filter::parse(&self.filter);
    let hit =
      |f: &OpenFile, t: &str| f.kind.label().contains(t) || f.text.to_lowercase().contains(t);
    let files = self.files.as_deref().unwrap_or_default();
    files.iter().filter(|file| filter.matches(|term| hit(file, term))).collect()
  }

  /// Selects row `index`, clamped to the rows.
  fn select(&mut self, index: usize) {
    let rows = self.rows();
    let index = index.min(rows.len().saturating_sub(1));
    let fd = rows.get(index).map_or(self.fd, |file| file.fd);
    (self.index, self.fd) = (index, fd);
  }

  /// After the rows changed: the selection stays on its fd while it is listed.
  fn follow(&mut self) {
    let at = self.rows().iter().position(|file| file.fd == self.fd);
    self.select(at.unwrap_or(self.index));
  }

  /// A key: navigation ends filter input, the filter keys are those of the process list, `o`
  /// reveals the selected file or the cwd, Esc clears the filter; `false` when Esc closes the
  /// overlay.
  pub(super) fn handle_key(&mut self, key: KeyEvent, sys: &dyn KillSys) -> bool {
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
      KeyCode::Char('o') => {
        let file = self.rows().get(self.index).copied();
        if let Some(file) = file.filter(|f| matches!(f.kind, Kind::File | Kind::Cwd)) {
          sys.reveal(&file.text);
        }
      }
      KeyCode::Esc if !self.filter.is_empty() => {
        self.filter.clear();
        self.follow();
      }
      KeyCode::Esc => return false,
      _ => {}
    }
    true
  }

  /// The wheel moves the selection as ↑ / ↓ do; other mouse events do nothing.
  pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, sys: &dyn KillSys) {
    let up = mouse.kind == MouseEventKind::ScrollUp;
    if up || mouse.kind == MouseEventKind::ScrollDown {
      self.handle_key(KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down }), sys);
    }
  }

  /// Draws the overlay over `area` (the process box, never empty): `<name> <pid>` and the filter
  /// on the top border, the files (cut in the middle) or why there are none, the selected one in
  /// full and the key hints on the bottom border.
  pub(super) fn render(&mut self, f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let name = vec![heading(self.proc.name.clone()), text(format!(" {}", self.proc.pid))];
    let inner = draw_box(f, area, filter_titles(name, &self.filter, self.typing, area.width));
    self.page = usize::from(inner.height);
    self.offset = scroll_offset(self.offset, Some(self.index), self.page, self.rows().len());
    if let Err(error) = &self.files {
      f.render_widget(Line::from(dim(format!(" {error}"))), inner);
    }

    // a blank cell at both borders and after the type column, as wide as `kqueue`
    let room = usize::from(inner.width).saturating_sub(9);
    let rows = self.rows().into_iter().enumerate().skip(self.offset);
    for (y, (i, file)) in (inner.y..inner.bottom()).zip(rows) {
      let kind = dim(format!(" {:6} ", file.kind.label()));
      let row = Rect { y, height: 1, ..inner };
      f.render_widget(Line::from(vec![kind, text(cut_middle(&file.text, room))]), row);
      if i == self.index {
        f.buffer_mut().set_style(row, theme::SELECTED);
      }
    }

    let hints = match self.typing {
      true => vec![hint("Enter", "keep"), hint("Esc", "clear"), hint("↑↓", "select")],
      false => vec![hint("o", "open"), hint("Esc", "close")],
    };
    let selected = self.rows().get(self.index).map(|f| f.text.clone()).filter(|t| !t.is_empty());
    render_bottom_border(f, area, hints, |room| {
      selected.iter().map(|path| text(cut_start(path, room))).collect()
    });
  }
}

#[cfg(test)]
mod tests {
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
  use ratatui::style::Modifier;

  use super::Info;
  use crate::fds::{Kind, OpenFile};
  use crate::procs::{self, ProcInfo};
  use crate::tui::kill::tests::FakeSys;
  use crate::tui::kill::{KillSys, LibcSys};
  use crate::tui::theme;

  /// A file open on `fd` (negative: the cwd).
  fn file(fd: i32, kind: Kind, text: &str) -> OpenFile {
    OpenFile { fd: (fd >= 0).then_some(fd), kind, text: text.into() }
  }

  const LOG: &str = "/Users/vlad/Library/Application Support/Google/Chrome/Default/IndexedDB/\
                     https_web.whatsapp.com_0.indexeddb.leveldb/000003.log";

  fn files() -> Vec<OpenFile> {
    vec![
      file(-1, Kind::Cwd, "/Users/vlad/Code/macmon"),
      file(0, Kind::File, "/dev/ttys004"),
      file(3, Kind::File, LOG),
      file(4, Kind::Tcp, "127.0.0.1:52344 → 140.82.112.4:443 (established)"),
      file(5, Kind::Tcp, "[::1]:8080 (listen)"),
      file(6, Kind::Udp, "0.0.0.0:5353"),
      file(7, Kind::Unix, "/var/run/mDNSResponder"),
      file(8, Kind::Pipe, ""),
      file(9, Kind::Kqueue, ""),
    ]
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
      path: "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer".into(),
      user: "vlad".into(),
      cpu_pct: 0.0,
      mem_bytes: 0,
      power_w: None,
      gpu_pct: 0.0,
      started: Some((631, 0)),
    }
  }

  /// The overlay of `proc` over `fake` with `files`, drawn once 80x8 (6 rows on screen).
  fn info(files: Result<Vec<OpenFile>, i32>) -> (Info, FakeSys) {
    let fake = fake(files);
    let mut info = Info::new(&proc(), &fake);
    render(&mut info, 80, 8);
    (info, fake)
  }

  fn press(info: &mut Info, fake: &FakeSys, code: KeyCode) -> bool {
    info.handle_key(KeyEvent::new(code, KeyModifiers::NONE), fake)
  }

  fn type_str(info: &mut Info, fake: &FakeSys, text: &str) {
    text.chars().for_each(|c| assert!(press(info, fake, KeyCode::Char(c))));
  }

  /// The fd of the selected row (`None`: the cwd) and of all the rows.
  fn shown(info: &Info) -> (Option<i32>, Vec<Option<i32>>) {
    let fds: Vec<_> = info.rows().iter().map(|file| file.fd).collect();
    (fds[info.index], fds)
  }

  fn fds(info: &Info) -> Vec<Option<i32>> {
    info.rows().iter().map(|file| file.fd).collect()
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
    let wheel = |info: &mut Info, kind| {
      let mouse = MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE };
      info.handle_mouse(mouse, &fake);
      shown(info).0
    };
    // the first row, the cwd, selected; a page is the 6 rows on screen
    assert_eq!(shown(&info).0, None);
    let steps = [
      (Up, None),
      (Down, Some(0)),
      (PageDown, Some(8)),
      (PageDown, Some(9)),
      (Down, Some(9)),
      (PageUp, Some(3)),
      (End, Some(9)),
      (Home, None),
    ];
    for (code, fd) in steps {
      assert!(press(&mut info, &fake, code));
      assert_eq!(shown(&info).0, fd, "{code:?}");
    }
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollDown), Some(0));
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollDown), Some(3));
    assert_eq!(wheel(&mut info, MouseEventKind::ScrollUp), Some(0));
    assert_eq!(wheel(&mut info, MouseEventKind::Moved), Some(0));

    // the list scrolls to keep the selection on screen
    assert!(press(&mut info, &fake, End));
    let shown = rows(&render(&mut info, 80, 8));
    assert!(shown[1].starts_with("│ tcp    127.0.0.1:52344 ") && shown[6].starts_with("│ kqueue"));
  }

  #[test]
  fn the_filter_matches_the_type_and_the_path_and_ends_with_navigation() {
    let (mut info, fake) = info(Ok(files()));
    let filter = |text: &str| {
      let (mut info, fake) = self::info(Ok(files()));
      assert!(press(&mut info, &fake, KeyCode::Char('/')));
      type_str(&mut info, &fake, text);
      fds(&info)
    };
    assert_eq!(filter("TCP, /var"), [Some(4), Some(5), Some(7)]);
    assert_eq!(filter("!/, !tcp"), [Some(6), Some(8), Some(9)]);
    assert_eq!(filter("cwd"), [None]);
    assert_eq!(filter("nothing"), []);

    // the selection, filtered out, goes to the row in its place; ↓ ends the input, `o` and Esc
    // act again
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp, unix");
    assert_eq!((info.typing, shown(&info)), (true, (Some(4), vec![Some(4), Some(5), Some(7)])));
    let buf = render(&mut info, 80, 8);
    assert!(rows(&buf)[0].starts_with("╭─ WindowServer 631 ─ /tcp, unix█ ──"), "{}", rows(&buf)[0]);
    assert!(
      rows(&buf)[7].ends_with("─ Enter keep | Esc clear | ↑↓ select ─╯"),
      "{}",
      rows(&buf)[7]
    );
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert_eq!((info.typing, info.filter.as_str(), shown(&info).0), (false, "tcp, unix", Some(5)));
    let buf = render(&mut info, 80, 8);
    assert!(rows(&buf)[0].starts_with("╭─ WindowServer 631 ─ /tcp, unix ──"), "{}", rows(&buf)[0]);
    assert!(rows(&buf)[7].ends_with("─ open | Esc close ─╯"), "{}", rows(&buf)[7]);
  }

  #[test]
  fn o_reveals_the_selected_file_or_the_cwd_only() {
    let (mut info, fake) = info(Ok(files()));
    // `o` on every row: the cwd and the files, not the sockets, the pipe and the kqueue
    for _ in 0..files().len() {
      assert!(press(&mut info, &fake, KeyCode::Char('o')));
      assert!(press(&mut info, &fake, KeyCode::Down));
    }
    assert_eq!(revealed(&fake), ["/Users/vlad/Code/macmon", "/dev/ttys004", LOG]);

    // typed, it is text
    assert!(press(&mut info, &fake, KeyCode::Home));
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "o");
    assert_eq!((info.filter.as_str(), revealed(&fake).len()), ("o", 3));
  }

  #[test]
  fn esc_ends_input_clearing_the_filter_then_clears_it_then_closes() {
    let (mut info, fake) = info(Ok(files()));
    let typed = |info: &Info| (info.typing, info.filter.clone(), shown(info).1.len());
    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp");
    assert!(press(&mut info, &fake, KeyCode::Esc));
    assert_eq!(typed(&info), (false, String::new(), 9));

    assert!(press(&mut info, &fake, KeyCode::Char('/')));
    type_str(&mut info, &fake, "tcp");
    assert!(press(&mut info, &fake, KeyCode::Enter));
    assert_eq!(typed(&info), (false, "tcp".into(), 2));
    assert!(press(&mut info, &fake, KeyCode::Esc));
    assert_eq!(typed(&info), (false, String::new(), 9));
    assert!(!press(&mut info, &fake, KeyCode::Esc), "closes");
  }

  #[test]
  fn a_refresh_keeps_the_selection_on_its_fd() {
    let (mut info, fake) = info(Ok(files()));
    let set = |files: Vec<OpenFile>| fake.0.borrow_mut().files.insert(631, Ok(files));
    for _ in 0..3 {
      assert!(press(&mut info, &fake, KeyCode::Down));
    }
    assert_eq!(shown(&info).0, Some(4));

    // fds 0 and 3 closed: still on fd 4, now the second row
    let mut fewer = files();
    fewer.drain(1..3);
    set(fewer.clone());
    info.refresh(&fake);
    assert_eq!((shown(&info).0, info.index), (Some(4), 1));
    // fd 4 closed: the row in its place
    fewer.remove(1);
    set(fewer);
    info.refresh(&fake);
    assert_eq!((shown(&info).0, info.index), (Some(5), 1));
    // the cwd too has its own key
    assert!(press(&mut info, &fake, KeyCode::Home));
    set(files());
    info.refresh(&fake);
    assert_eq!(shown(&info).0, None);
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
      let shown = rows(&render(&mut info, 40, 5));
      assert_eq!(shown[1], format!("│{text:38}│"));
      assert_eq!(shown[4], format!("╰{}─ open | Esc close ─╯", "─".repeat(18)));
      // nothing to select or reveal
      for code in [KeyCode::Down, KeyCode::End, KeyCode::Char('o')] {
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
  fn the_overlay_draws_in_any_window() {
    let (mut info, fake) = info(Ok(files()));
    for (width, height) in (1..70).flat_map(|w| (1..12).map(move |h| (w, h))) {
      render(&mut info, width, height);
    }
    // the long path cut in the middle, keeping its file name; the selected one in full on the
    // bottom border, cut from the start
    assert!(press(&mut info, &fake, KeyCode::Down));
    assert!(press(&mut info, &fake, KeyCode::Down));
    let buf = render(&mut info, 60, 8);
    let overlay = [
      "╭─ WindowServer 631 ─ / filter ────────────────────────────╮",
      "│ cwd    /Users/vlad/Code/macmon                           │",
      "│ file   /dev/ttys004                                      │",
      "│ file   /Users/vlad/Library/Application Suppo…/000003.log │",
      "│ tcp    127.0.0.1:52344 → 140.82.112.4:443 (established)  │",
      "│ tcp    [::1]:8080 (listen)                               │",
      "│ udp    0.0.0.0:5353                                      │",
      "╰─ …com_0.indexeddb.leveldb/000003.log ─ open | Esc close ─╯",
    ];
    let shown = rows(&buf);
    assert_eq!(shown, overlay);
    // the type dim, the path in the default color, the selected row reversed from border to
    // border, the keys of the hints in the default color and the rest dim
    let cell = |x: u16, y: u16| (buf[(x, y)].fg, buf[(x, y)].modifier);
    let (none, reversed) = (Modifier::empty(), Modifier::REVERSED);
    assert_eq!([cell(2, 1), cell(9, 1)], [(theme::DIM, none), (theme::TEXT, none)]);
    assert_eq!([cell(1, 3), cell(2, 3), cell(58, 3)], [(theme::TEXT, reversed); 3]);
    let open = shown[7].find("open").unwrap();
    let open = shown[7][..open].chars().count() as u16;
    assert_eq!([cell(open, 7).0, cell(open + 1, 7).0], [theme::TEXT, theme::DIM]);
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
    assert_eq!(info.files.unwrap()[0], OpenFile { fd: None, kind: Kind::Cwd, text: cwd });
  }
}
