//! Killing the selected process (`k`) from a popup: Terminate (SIGTERM), Force kill (SIGKILL) or
//! Cancel. Only the sampled process gets a signal: a `ps` row (no sampled start time) gets none,
//! and the start time is read again right before `kill()`; macOS has no identity-bound kill.

use std::{fmt, io, process};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Margin, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Clear;

use super::boxes::{Titles, cells, draw_box, hint, render_bottom_border};
use super::proc_view::{cut_end, cut_start};
use super::theme::{self, dim, heading, text};
use crate::procs::{self, ProcInfo};

/// A button: its label and the signal it sends, pressed by the first letter of the label (in lower
/// case, underlined); one without a signal closes the popup. An error has only OK.
type Button = (&'static str, Option<i32>);
const CHOICES: [Button; 3] =
  [("Terminate", Some(libc::SIGTERM)), ("Force kill", Some(libc::SIGKILL)), ("Cancel", None)];
const OK: [Button; 1] = [("OK", None)];

/// System calls of `Kill`, so tests can use a fake.
pub(super) trait KillSys: fmt::Debug {
  /// `kill(pid, sig)`; the errno when it fails.
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32>;
  /// Start time of `pid` (seconds, microseconds), as `ProcInfo::started` has it; `None` when it
  /// is gone, a zombie or not readable for this user.
  fn identity(&self, pid: i32) -> Option<(u64, u64)>;
}

/// The real system calls.
#[derive(Debug)]
struct LibcSys;

impl KillSys for LibcSys {
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32> {
    let failed = unsafe { libc::kill(pid, sig) } != 0;
    if failed { Err(io::Error::last_os_error().raw_os_error().unwrap_or(0)) } else { Ok(()) }
  }

  fn identity(&self, pid: i32) -> Option<(u64, u64)> {
    let info = procs::bsd_info(pid)?;
    (info.pbi_status != libc::SZOMB).then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
  }
}

/// What the popup says about a failed `kill()`: `exited`, `Not permitted`, or the `strerror`.
fn failure(errno: i32) -> String {
  let text = io::Error::from_raw_os_error(errno).to_string();
  match errno {
    libc::ESRCH => "exited".into(),
    libc::EPERM => "Not permitted".into(),
    _ => text.replace(&format!(" (os error {errno})"), ""),
  }
}

/// The popup for one process: its start time or why it can't be killed, and its buttons' state.
#[derive(Debug)]
struct Popup {
  proc: ProcInfo,
  state: Result<(u64, u64), String>,
  selected: usize,
  rects: Vec<Rect>,
}

/// The kill popup (`k`) with the system calls it goes through.
#[derive(Debug)]
pub(super) struct Kill {
  sys: Box<dyn KillSys>,
  popup: Option<Popup>,
}

/// The real system calls; in tests a fake without processes, so no test signals a real one.
impl Default for Kill {
  fn default() -> Self {
    #[cfg(not(test))]
    let sys = LibcSys;
    #[cfg(test)]
    let sys = tests::FakeSys::default();
    Self { sys: Box::new(sys), popup: None }
  }
}

impl Kill {
  /// Whether the popup is open: every key goes to `handle_key` then.
  pub(super) fn is_open(&self) -> bool {
    self.popup.is_some()
  }

  /// Opens the popup for `proc`, Terminate selected.
  pub(super) fn open(&mut self, proc: &ProcInfo) {
    let state = self.check(proc.pid, proc.started);
    self.popup = Some(Popup { proc: proc.clone(), state, selected: 0, rects: vec![] });
  }

  /// `started`, when `pid` may be signalled and is still the process sampled with it.
  fn check(&self, pid: i32, started: Option<(u64, u64)>) -> Result<(u64, u64), String> {
    match pid {
      ..=0 => return Err(format!("Won't kill pid {pid}")),
      1 => return Err("Won't kill launchd".into()),
      _ if pid as u32 == process::id() => return Err("Won't kill macmon itself".into()),
      _ => {}
    }
    self.sys.signal(pid, 0).map_err(failure)?;
    let started = started.ok_or_else(|| failure(libc::EPERM))?;
    // gone since the sample, or a zombie (unreadable)
    if self.sys.identity(pid) != Some(started) {
      return Err(failure(libc::ESRCH));
    }
    Ok(started)
  }

  pub(super) fn close(&mut self) {
    self.popup = None;
  }

  /// A key while the popup is open: `←` `→` and Tab select, Enter presses the selected button, `t`
  /// / `f` (no modifier) theirs, Esc the last one (Cancel, OK). Other keys do nothing.
  pub(super) fn handle_key(&mut self, key: KeyEvent) {
    let Some(popup) = self.popup.as_mut() else { return };
    let (buttons, selected) =
      (if popup.state.is_ok() { &CHOICES[..] } else { &OK }, popup.selected);
    let shortcut =
      |c: char| buttons.iter().position(|b| b.1.is_some() && b.0.to_lowercase().starts_with(c));
    match key.code {
      KeyCode::Left | KeyCode::BackTab => popup.selected = selected.saturating_sub(1),
      KeyCode::Right | KeyCode::Tab => popup.selected = (selected + 1).min(buttons.len() - 1),
      KeyCode::Enter => self.press(Some(selected)),
      KeyCode::Esc => self.press(Some(buttons.len() - 1)),
      KeyCode::Char(c) if key.modifiers.is_empty() => self.press(shortcut(c)),
      _ => {}
    }
  }

  /// A left click on a button presses it; other mouse events do nothing.
  pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
    let at = Position::new(mouse.column, mouse.row);
    let hit = self.popup.as_ref().and_then(|p| p.rects.iter().position(|r| r.contains(at)));
    self.press(hit.filter(|_| mouse.kind == MouseEventKind::Down(MouseButton::Left)));
  }

  /// Presses button `i` (of `CHOICES` without an error), if any: a signal closes the popup or shows
  /// why it failed, over OK; Cancel and OK close it.
  fn press(&mut self, i: Option<usize>) {
    let (Some(popup), Some(i)) = (self.popup.as_mut(), i) else { return };
    let (&Ok(started), Some(sig)) = (&popup.state, CHOICES[i].1) else { return self.close() };
    // read again right before `kill()`: a pid reused since `k` gets no signal
    let same = self.sys.identity(popup.proc.pid) == Some(started);
    let sent = if same { self.sys.signal(popup.proc.pid, sig) } else { Err(libc::ESRCH) };
    match sent {
      Ok(()) => self.close(),
      Err(errno) => (popup.state, popup.selected, popup.rects) = (Err(failure(errno)), 0, vec![]),
    }
  }

  /// Draws the popup over `area`: the name, `pid · user · path` or the error, the buttons to click.
  pub(super) fn render(&mut self, f: &mut Frame, area: Rect) {
    let Some(popup) = self.popup.as_mut().filter(|_| !area.is_empty()) else { return };
    let (proc, ok) = (&popup.proc, popup.state.is_ok());
    let buttons = if ok { &CHOICES[..] } else { &OK };
    let owner = if ok { format!("{} · {} · ", proc.pid, proc.user) } else { String::new() };
    let path = if proc.path.is_empty() { &proc.name } else { &proc.path };
    let about = popup.state.as_ref().err().unwrap_or(path);
    let width = |text: &str| Span::raw(text).width();
    let row = |buttons: &[Button]| buttons.iter().map(|b| b.0.len() + 7).sum::<usize>() - 3;
    // two blank columns on both sides of the text (cut at 56 cells) or the buttons, the borders
    let text_width = width(&proc.name).max(width(&owner) + width(about)).min(56);
    let size = Constraint::Length(cells(text_width.max(row(&CHOICES)) + 6));
    let rect = area.centered(size, Constraint::Length(8));
    f.render_widget(Clear, rect);
    let inner = draw_box(f, rect, Titles::new(heading("Kill process")));
    let hints = vec![hint("←→", "select"), hint("↵", "ok"), hint("Esc", "close")];
    render_bottom_border(f, rect, if ok { hints } else { vec![hint("↵", "ok")] }, |_| vec![]);
    let room = usize::from(inner.width.saturating_sub(4));
    let about = format!("{owner}{}", cut_start(about, room.saturating_sub(width(&owner))));
    let lines = [heading(cut_end(&proc.name, room)), if ok { dim(about) } else { text(about) }];
    // a blank row above, between and below when there is room
    let gap = u16::from(inner.height >= 6);
    f.render_widget(Text::from_iter(lines), inner.inner(Margin::new(2, gap)));
    // right-aligned, two blank columns before the border; cut, or none, when they don't fit
    let y = inner.y + 2 + 2 * gap;
    let mut x = inner.right().saturating_sub(cells(row(buttons) + 2)).max(inner.x);
    popup.rects.clear();
    for (i, &(label, sig)) in buttons.iter().enumerate() {
      let underline = if sig.is_some() { Modifier::UNDERLINED } else { Modifier::empty() };
      let first = Span::styled(&label[..1], Style::new().fg(theme::TEXT).add_modifier(underline));
      let line = Line::from(vec![text("[ "), first, text(&label[1..]), text(" ]")]);
      let rect = Rect::new(x, y, cells(label.len() + 4), 1).intersection(inner);
      let style = if i == popup.selected { theme::SELECTED } else { Style::new() };
      f.render_widget(line.style(style), rect);
      popup.rects.push(rect);
      x = x.saturating_add(cells(label.len() + 7));
    }
  }
}

#[cfg(test)]
pub(super) mod tests {
  use std::cell::RefCell;
  use std::collections::HashMap;
  use std::os::unix::process::ExitStatusExt;
  use std::process::{Child, Command};
  use std::rc::Rc;

  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
  };
  use ratatui::style::Modifier;

  use super::{Kill, KillSys, LibcSys};
  use crate::procs::{self, ProcInfo};

  /// A fake process: its start second (`None`: unreadable, as a zombie) and the errno of signals.
  type FakeProc = (Option<u64>, Option<i32>);
  /// The fake processes and `(pid, signal)` of every `signal` call.
  type FakeState = (HashMap<i32, FakeProc>, Vec<(i32, i32)>);

  /// Fake system calls: only the processes it is given exist, and every `signal` call is logged.
  /// Clones share all that, so a test keeps one while the app owns another. No test may use the
  /// real ones (but the one with its own child): the test pids are real pids on the dev machine.
  #[derive(Debug, Default, Clone)]
  pub(crate) struct FakeSys(Rc<RefCell<FakeState>>);

  impl FakeSys {
    /// `Kill` through a fake with the processes `pids`, each started at the second of its pid.
    pub(crate) fn kill(pids: &[i32]) -> (Kill, Self) {
      let fake = Self::default();
      pids.iter().for_each(|&pid| fake.set(pid, Some((Some(pid as u64), None))));
      (Kill { sys: Box::new(fake.clone()), popup: None }, fake)
    }

    /// `pid` becomes `proc`, or exits with `None`.
    fn set(&self, pid: i32, proc: Option<FakeProc>) {
      let procs = &mut self.0.borrow_mut().0;
      match proc {
        Some(proc) => procs.insert(pid, proc),
        None => procs.remove(&pid),
      };
    }

    /// `(pid, signal)` of every `signal` call, the checks with signal 0 too.
    pub(crate) fn calls(&self) -> Vec<(i32, i32)> {
      self.0.borrow().1.clone()
    }
  }

  impl KillSys for FakeSys {
    fn signal(&self, pid: i32, sig: i32) -> Result<(), i32> {
      let (procs, calls) = &mut *self.0.borrow_mut();
      calls.push((pid, sig));
      procs.get(&pid).map_or(Err(libc::ESRCH), |&(_, errno)| errno.map_or(Ok(()), Err))
    }

    fn identity(&self, pid: i32) -> Option<(u64, u64)> {
      Some((self.0.borrow().0.get(&pid)?.0?, 0))
    }
  }

  fn code(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
  }

  fn key(c: char) -> KeyEvent {
    code(KeyCode::Char(c))
  }

  /// WindowServer sampled with `pid`, started at the second of its pid as in `FakeSys::kill`.
  fn window_server(pid: i32) -> ProcInfo {
    ProcInfo {
      pid,
      name: "WindowServer".into(),
      path: "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer".into(),
      user: "_windowserver".into(),
      cpu_pct: 0.0,
      mem_bytes: 0,
      power_w: None,
      gpu_pct: 0.0,
      started: Some((pid as u64, 0)),
    }
  }

  /// The popup's error (`Ok` with the three buttons) and its selected button; `None` when closed.
  fn shown(kill: &Kill) -> Option<(Result<(), String>, usize)> {
    kill.popup.as_ref().map(|popup| (popup.state.clone().map(|_| ()), popup.selected))
  }

  #[test]
  fn k_on_a_process_that_cant_be_killed_shows_why() {
    let own = std::process::id() as i32;
    let sampled = Some((631, 0));
    // the pid, 631 at `k` (`None`: exited), its sampled start time, the error
    let cases = [
      (0, None, sampled, "Won't kill pid 0"),
      (-1, None, sampled, "Won't kill pid -1"),
      (1, None, sampled, "Won't kill launchd"),
      (own, None, sampled, "Won't kill macmon itself"),
      (631, Some((Some(631), Some(libc::EPERM))), sampled, "Not permitted"),
      (631, Some((Some(631), Some(libc::EINVAL))), sampled, "Invalid argument"),
      (631, None, sampled, "exited"),
      // a `ps` row: alive, but the process with its pid now may be another one
      (631, Some((Some(631), None)), None, "Not permitted"),
      // restarted since the sample, or a zombie
      (631, Some((Some(2000), None)), sampled, "exited"),
      (631, Some((None, None)), sampled, "exited"),
    ];
    for (pid, proc, started, error) in cases {
      let (mut kill, fake) = FakeSys::kill(&[1, own]);
      fake.set(631, proc);
      kill.open(&ProcInfo { started, ..window_server(pid) });
      // only OK: the keys of the other buttons do nothing, Enter closes
      for key in [key('t'), key('f'), code(KeyCode::Right), code(KeyCode::Tab)] {
        kill.handle_key(key);
        assert_eq!(shown(&kill), Some((Err(error.into()), 0)), "{error}");
      }
      kill.handle_key(code(KeyCode::Enter));
      // refusals make no system call at all, the rest only checks
      let checked = if pid == 631 { vec![(631, 0)] } else { vec![] };
      assert_eq!((shown(&kill), fake.calls()), (None, checked), "{error}");
    }
  }

  #[test]
  fn keys_select_and_press_buttons_and_other_keys_do_nothing() {
    use KeyCode::{BackTab, Down, Enter, Esc, Left, Right, Tab};
    let chord = |c, modifiers| KeyEvent::new(KeyCode::Char(c), modifiers);
    let (term, force) = (vec![(631, libc::SIGTERM)], vec![(631, libc::SIGKILL)]);
    // the keys, the button selected after them (`None`: closed), the signals sent
    let cases = [
      (vec![code(Enter)], None, term.clone()),
      (vec![key('t')], None, term.clone()),
      (vec![key('f')], None, force.clone()),
      (vec![code(Right), code(Enter)], None, force.clone()),
      (vec![code(Tab), code(Tab), code(BackTab), code(Enter)], None, force),
      (vec![code(Right), code(Left), code(Enter)], None, term.clone()),
      // a shortcut presses its button whatever is selected
      (vec![code(Right), code(Right), key('t')], None, term),
      // no wrap around, Cancel and Esc only close
      (vec![code(Right); 4], Some(2), vec![]),
      (vec![code(Left), code(BackTab)], Some(0), vec![]),
      (vec![code(Right), code(Right), code(Enter)], None, vec![]),
      (vec![code(Right), code(Esc)], None, vec![]),
      // other keys and the shortcuts with a modifier do nothing
      (vec![key('q'), key('y'), key('k'), key('n'), key('T'), code(Down)], Some(0), vec![]),
      (vec![chord('t', KeyModifiers::CONTROL), chord('f', KeyModifiers::ALT)], Some(0), vec![]),
      (vec![chord('t', KeyModifiers::SUPER)], Some(0), vec![]),
    ];
    for (keys, selected, sent) in cases {
      let (mut kill, fake) = FakeSys::kill(&[631]);
      kill.open(&window_server(631));
      keys.iter().for_each(|&key| kill.handle_key(key));
      assert_eq!(shown(&kill), selected.map(|i| (Ok(()), i)), "{keys:?}");
      assert_eq!(fake.calls(), [vec![(631, 0)], sent].concat(), "{keys:?}");
    }
  }

  #[test]
  fn a_signal_that_fails_shows_why_over_ok() {
    let failing = Some((Some(631), Some(libc::EINVAL)));
    // 631 at the key (`None`: exited), the key, the error shown, the signals sent
    let cases = [
      (None, key('t'), "exited", vec![]),
      (Some((Some(2000), None)), key('t'), "exited", vec![]),
      (Some((None, None)), key('f'), "exited", vec![]),
      (failing, code(KeyCode::Enter), "Invalid argument", vec![(631, libc::SIGTERM)]),
    ];
    for (proc, press, error, sent) in cases {
      let (mut kill, fake) = FakeSys::kill(&[631]);
      kill.open(&window_server(631));
      fake.set(631, proc);
      // OK selected, the keys of the other buttons do nothing, Esc closes
      for key in [press, key('t'), key('f'), code(KeyCode::Right)] {
        kill.handle_key(key);
        assert_eq!(shown(&kill), Some((Err(error.into()), 0)), "{error}");
      }
      kill.handle_key(code(KeyCode::Esc));
      assert_eq!((shown(&kill), fake.calls()), (None, [vec![(631, 0)], sent].concat()), "{error}");
    }
  }

  /// `kill` drawn over a window `width` by `height`.
  fn render(kill: &mut Kill, width: u16, height: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| kill.render(f, f.area())).unwrap();
    term.backend().buffer().clone()
  }

  fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect()).collect()
  }

  /// The first cell of `text` in `rows`.
  fn find(rows: &[String], text: &str) -> (u16, u16) {
    let (y, row) = rows.iter().enumerate().find(|(_, row)| row.contains(text)).unwrap();
    (row[..row.find(text).unwrap()].chars().count() as u16, y as u16)
  }

  /// A mouse event `kind` on the first cell of `text` in the frame `buf` of `kill`.
  fn mouse_on(kill: &mut Kill, buf: &Buffer, kind: MouseEventKind, text: &str) {
    let (column, row) = find(&rows(buf), text);
    kill.handle_mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
  }

  #[test]
  fn a_click_on_a_button_presses_it_other_mouse_events_do_nothing() {
    let (left, right) =
      (MouseEventKind::Down(MouseButton::Left), MouseEventKind::Down(MouseButton::Right));
    let (mut kill, fake) = FakeSys::kill(&[631]);
    kill.open(&window_server(631));
    let buf = render(&mut kill, 80, 12);
    for (kind, text) in
      [(left, "WindowServer"), (right, "[ Force"), (MouseEventKind::ScrollDown, "[ Force")]
    {
      mouse_on(&mut kill, &buf, kind, text);
      assert_eq!(shown(&kill), Some((Ok(()), 0)), "{kind:?}");
    }
    mouse_on(&mut kill, &buf, left, "kill ]");
    assert_eq!((shown(&kill), fake.calls()), (None, vec![(631, 0), (631, libc::SIGKILL)]));

    // Cancel, and OK under an error, only close
    for (pid, button) in [(631, "[ Cancel ]"), (1, "[ OK ]")] {
      kill.open(&window_server(pid));
      let buf = render(&mut kill, 80, 12);
      mouse_on(&mut kill, &buf, left, button);
      assert_eq!(shown(&kill), None, "{button}");
    }
    assert_eq!(fake.calls(), [(631, 0), (631, libc::SIGKILL), (631, 0)]);
  }

  #[test]
  fn the_popup_draws_in_any_window() {
    let (mut kill, _) = FakeSys::kill(&[33928]);
    let chrome = "Google Chrome Helper (Renderer)";
    let path = format!("/Applications/Google Chrome.app/Contents/Frameworks/Helpers/{chrome}");
    kill.open(&ProcInfo { name: chrome.into(), path, user: "user".into(), ..window_server(33928) });
    for (width, height) in (0..70).flat_map(|w| (0..10).map(move |h| (w, h))) {
      render(&mut kill, width, height);
    }
    // the path cut from the start, the buttons right-aligned
    let popup = [
      "╭─ Kill process ─────────────────────────────────────────────╮",
      "│                                                            │",
      "│  Google Chrome Helper (Renderer)                           │",
      "│  33928 · user · …/Helpers/Google Chrome Helper (Renderer)  │",
      "│                                                            │",
      "│               [ Terminate ]   [ Force kill ]   [ Cancel ]  │",
      "│                                                            │",
      "╰───────────────────────────── ←→ select | ↵ ok | Esc close ─╯",
    ];
    let buf = render(&mut kill, 62, 8);
    let shown = rows(&buf);
    assert_eq!(shown, popup);
    // the selected button reversed, the letters of the shortcuts underlined
    let style = |text| buf[find(&shown, text)].modifier;
    let (none, reversed, underlined) =
      (Modifier::empty(), Modifier::REVERSED, Modifier::UNDERLINED);
    assert_eq!((style("[ Terminate"), style("Terminate")), (reversed, reversed | underlined));
    assert_eq!((style("[ Force"), style("Force"), style("Cancel")), (none, underlined, none));

    // without the blank rows when low, cut when narrow
    let narrow = [
      "╭─ Kill process ───────────────────────╮",
      "│  Google Chrome Helper (Renderer)     │",
      "│  33928 · user · … Helper (Renderer)  │",
      "│[ Terminate ]   [ Force kill ]   [ Can│",
    ];
    assert_eq!(rows(&render(&mut kill, 40, 6))[..4], narrow);

    // an error over OK, as wide as the three buttons
    kill.open(&window_server(1));
    let error = [
      "╭─ Kill process ────────────────────────────────╮",
      "│                                               │",
      "│  WindowServer                                 │",
      "│  Won't kill launchd                           │",
      "│                                               │",
      "│                                       [ OK ]  │",
      "│                                               │",
      "╰──────────────────────────────────────── ↵ ok ─╯",
    ];
    assert_eq!(rows(&render(&mut kill, 49, 8)), error);
  }

  /// A child of the test, killed and reaped when the test ends, however it ends.
  struct OwnChild(Child);

  impl Drop for OwnChild {
    fn drop(&mut self) {
      // after a `wait`, neither signals the pid again
      let _ = self.0.kill();
      let _ = self.0.wait();
    }
  }

  /// The only test with the real system calls, on a child of its own.
  #[test]
  fn t_terminates_a_real_child() {
    let mut child = OwnChild(Command::new("sleep").arg("30").spawn().unwrap());
    let pid = child.0.id() as i32;
    let info = procs::bsd_info(pid).unwrap();
    let mut kill = Kill { sys: Box::new(LibcSys), popup: None };
    let started = Some((info.pbi_start_tvsec, info.pbi_start_tvusec));
    kill.open(&ProcInfo { name: "sleep".into(), started, ..window_server(pid) });
    assert_eq!(shown(&kill), Some((Ok(()), 0)));
    kill.handle_key(key('t'));
    assert_eq!(shown(&kill), None);
    assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGTERM));
  }
}
