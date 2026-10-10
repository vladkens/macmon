//! Killing the selected process (`k`): a popup over the list, `t` sends SIGTERM, `f` SIGKILL.
//! Only the sampled process gets a signal: a row without a sampled start time (`ps`) gets none, and
//! the start time is read again right before `kill()`. That leaves the gap between the two calls:
//! macOS has no identity-bound kill and hands out pids in sequence, so a reuse needs microseconds.

use std::{fmt, io, process};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::text::Line;
use ratatui::widgets::Clear;

use super::boxes::{Titles, cells, draw_box, hint, join};
use super::proc_view::cut_end;
use super::theme::{heading, text};
use crate::procs;

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

/// The popup for one process: its start time, or why it can't be killed.
#[derive(Debug)]
struct Popup {
  pid: i32,
  name: String,
  state: Result<(u64, u64), String>,
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

  /// Opens the popup for `pid` named `name`, sampled with the start time `started`.
  pub(super) fn open(&mut self, pid: i32, name: &str, started: Option<(u64, u64)>) {
    self.popup = Some(Popup { pid, name: name.to_string(), state: self.check(pid, started) });
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

  /// A key while the popup is open: `t` sends SIGTERM, `f` SIGKILL (without a modifier),
  /// and the popup closes, or shows why the signal failed. Any other key only closes it.
  pub(super) fn handle_key(&mut self, key: KeyEvent) {
    let Some(Popup { pid, name, state: Ok(started) }) = self.popup.take() else { return };
    let sig = match (key.code, key.modifiers.is_empty()) {
      (KeyCode::Char('t'), true) => libc::SIGTERM,
      (KeyCode::Char('f'), true) => libc::SIGKILL,
      _ => return,
    };

    // read again right before `kill()`: a pid reused since `k` gets no signal
    let same = self.sys.identity(pid) == Some(started);
    let sent = if same { self.sys.signal(pid, sig) } else { Err(libc::ESRCH) };
    if let Err(errno) = sent {
      self.popup = Some(Popup { pid, name, state: Err(failure(errno)) });
    }
  }

  /// Draws the popup centered over `area` (when not empty), as long as it is open.
  pub(super) fn render(&self, f: &mut Frame, area: Rect) {
    let Some(popup) = self.popup.as_ref().filter(|_| !area.is_empty()) else { return };
    let keys = [hint("t", "terminate"), hint("f", "force kill"), hint("Esc", "cancel")];
    let keys = Line::from(join(keys));
    let last =
      popup.state.as_ref().map_or_else(|error| Line::from(text(error.clone())), |_| keys.clone());
    // the text with a blank column on both sides, and the borders
    let width = cells(keys.width().max(last.width()) + 4);
    let rect = area.centered(Constraint::Length(width), Constraint::Length(5));
    f.render_widget(Clear, rect);
    let inner = draw_box(f, rect, Titles::new(heading(format!("Kill {}", popup.pid))));
    let room = inner.width.saturating_sub(2);
    let name = Line::from(heading(cut_end(&popup.name, usize::from(room))));
    for (line, y) in [name, Line::default(), last].iter().zip(inner.y..inner.bottom()) {
      f.buffer_mut().set_line(inner.x + 1, y, line, room);
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
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

  use super::{Kill, KillSys, LibcSys};
  use crate::procs;

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

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  /// The popup's error, `Ok` while it offers the keys, `None` when closed.
  fn shown(kill: &Kill) -> Option<Result<(), String>> {
    kill.popup.as_ref().map(|popup| popup.state.clone().map(|_| ()))
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
      kill.open(pid, "WindowServer", started);
      assert_eq!(shown(&kill), Some(Err(error.into())), "{error}");
      kill.handle_key(key('t'));
      // refusals make no system call at all, the rest only checks
      let checked = if pid == 631 { vec![(631, 0)] } else { vec![] };
      assert_eq!((shown(&kill), fake.calls()), (None, checked), "{error}");
    }
  }

  #[test]
  fn t_and_f_signal_once_other_keys_and_failures_close() {
    let chord = |c, modifiers| KeyEvent::new(KeyCode::Char(c), modifiers);
    let (alive, failing) = (Some((Some(631), None)), Some((Some(631), Some(libc::EINVAL))));
    // 631 at the key (`None`: exited), the key, the error shown, the signals sent
    let cases = [
      (alive, key('t'), None, vec![(631, libc::SIGTERM)]),
      (alive, key('f'), None, vec![(631, libc::SIGKILL)]),
      (alive, key('T'), None, vec![]),
      (alive, key('k'), None, vec![]),
      (alive, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), None, vec![]),
      (alive, chord('t', KeyModifiers::CONTROL), None, vec![]),
      (alive, chord('f', KeyModifiers::ALT), None, vec![]),
      (alive, chord('t', KeyModifiers::SUPER), None, vec![]),
      // gone, restarted or a zombie since `k`, or the signal fails
      (None, key('t'), Some("exited"), vec![]),
      (Some((Some(2000), None)), key('t'), Some("exited"), vec![]),
      (Some((None, None)), key('f'), Some("exited"), vec![]),
      (failing, key('t'), Some("Invalid argument"), vec![(631, libc::SIGTERM)]),
    ];
    for (proc, key, error, sent) in cases {
      let (mut kill, fake) = FakeSys::kill(&[631]);
      kill.open(631, "WindowServer", Some((631, 0)));
      fake.set(631, proc);
      kill.handle_key(key);
      assert_eq!(shown(&kill), error.map(|error| Err(error.into())), "{key:?}");
      // any key closes an error, then keys do nothing
      kill.handle_key(key);
      assert_eq!((shown(&kill), fake.calls()), (None, [vec![(631, 0)], sent].concat()), "{key:?}");
    }
  }

  /// Rows of `kill` drawn over a window `width` by `height`.
  fn render(kill: &Kill, width: u16, height: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| kill.render(f, f.area())).unwrap();
    let buf = term.backend().buffer();
    (0..height).map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect()).collect()
  }

  #[test]
  fn the_popup_draws_in_any_window() {
    let (mut kill, _) = FakeSys::kill(&[33928]);
    kill.open(33928, "Google Chrome Helper (Renderer)", Some((33928, 0)));
    for (width, height) in (0..50).flat_map(|w| (0..8).map(move |h| (w, h))) {
      render(&kill, width, height);
    }
    let popup = [
      "╭─ Kill 33928 ────────────────────────────╮",
      "│ Google Chrome Helper (Renderer)         │",
      "│                                         │",
      "│ t terminate | f force kill | Esc cancel │",
      "╰─────────────────────────────────────────╯",
    ];
    assert_eq!(render(&kill, 43, 5), popup);
    let narrow = ["╭─ Kill 33928 ────╮", "│ Google Chrome … │", "│                 │"];
    assert_eq!(render(&kill, 19, 5)[..3], narrow);
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
    kill.open(pid, "sleep", Some((info.pbi_start_tvsec, info.pbi_start_tvusec)));
    assert_eq!(shown(&kill), Some(Ok(())));
    kill.handle_key(key('t'));
    assert_eq!(shown(&kill), None);
    assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGTERM));
  }
}
