//! Killing the selected process (`k`): a y/n prompt bound to the process it asks about, then
//! SIGTERM. Nothing is signalled without `y`, and only the process the prompt was for.

use std::{fmt, io, process};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;

use super::proc_view::cut_end;
use super::theme::{heading, text};
use crate::procs;

/// What tells a process from a later one with the same pid: its start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
  sec: u64,
  usec: u64,
}

/// System calls of `Kill`, so tests can use a fake.
pub(super) trait KillSys {
  /// `kill(pid, sig)`; the errno when it fails.
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32>;
  /// Start time of `pid`; `None` when it is gone or not readable for this user.
  fn identity(&self, pid: i32) -> Option<Identity>;
}

/// The real system calls.
struct LibcSys;

impl KillSys for LibcSys {
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32> {
    match unsafe { libc::kill(pid, sig) } {
      0 => Ok(()),
      _ => Err(io::Error::last_os_error().raw_os_error().unwrap_or(0)),
    }
  }

  fn identity(&self, pid: i32) -> Option<Identity> {
    let info = procs::bsd_info(pid)?;
    Some(Identity { sec: info.pbi_start_tvsec, usec: info.pbi_start_tvusec })
  }
}

/// `strerror` of `errno`: `Operation not permitted`.
fn strerror(errno: i32) -> String {
  let err = io::Error::from_raw_os_error(errno).to_string();
  match err.split_once(" (os error") {
    Some((text, _)) => text.to_string(),
    None => err,
  }
}

/// A line about one process for the bottom border: `before`, the process name, `after`. Only the
/// name is cut when the line is too long, so its end (`? y/n`) stays.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Note {
  before: String,
  name: String,
  after: String,
}

impl Note {
  fn new(before: impl Into<String>, name: &str, after: impl Into<String>) -> Self {
    Self { before: before.into(), name: name.to_string(), after: after.into() }
  }

  /// A line without a process name.
  fn plain(text: impl Into<String>) -> Self {
    Self::new(text, "", "")
  }

  /// Cells of the line without the name.
  fn fixed_width(&self) -> usize {
    Span::raw(&self.before).width() + Span::raw(&self.after).width()
  }

  /// Fewest cells that show the line with its end: the name cut to `…`.
  pub(super) fn min_width(&self) -> usize {
    self.fixed_width() + usize::from(!self.name.is_empty())
  }

  /// The line in `room` cells, the name bold and cut to the room the rest leaves.
  pub(super) fn spans(&self, room: usize) -> Vec<Span<'static>> {
    let name = cut_end(&self.name, room.saturating_sub(self.fixed_width()));
    vec![text(self.before.clone()), heading(name), text(self.after.clone())]
  }
}

impl fmt::Display for Note {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}{}{}", self.before, self.name, self.after)
  }
}

/// The process a prompt asks to kill, as it was when asked.
#[derive(Debug, Clone, PartialEq)]
struct Target {
  pid: i32,
  name: String,
  identity: Identity,
}

impl Target {
  /// `<before><pid> <name><after>`.
  fn note(&self, before: &str, after: impl Into<String>) -> Note {
    Note::new(format!("{before}{} ", self.pid), &self.name, after)
  }
}

#[derive(Debug, Default)]
enum State {
  #[default]
  None,
  /// Waiting for `y` to kill the target.
  Asking(Target),
  /// The outcome of the last `k`, shown until the next one.
  Done(Note),
}

/// Kill state: the prompt and the outcome of the last `k`, with the system calls it goes through.
pub(super) struct Kill {
  sys: Box<dyn KillSys>,
  state: State,
}

/// The real system calls.
impl Default for Kill {
  fn default() -> Self {
    Self::new(Box::new(LibcSys))
  }
}

impl fmt::Debug for Kill {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.debug_struct("Kill").field("state", &self.state).finish_non_exhaustive()
  }
}

/// Why `pid` is never signalled: not a single process, launchd or macmon itself.
fn refusal(pid: i32) -> Option<Note> {
  match pid {
    ..=0 => Some(Note::plain(format!("Won't kill pid {pid}"))),
    1 => Some(Note::plain("Won't kill launchd (pid 1)")),
    _ if u32::try_from(pid) == Ok(process::id()) => Some(Note::plain("Won't kill macmon itself")),
    _ => None,
  }
}

impl Kill {
  pub(super) fn new(sys: Box<dyn KillSys>) -> Self {
    Self { sys, state: State::None }
  }

  /// Whether the prompt is open: every key goes to `answer` then.
  pub(super) fn asking(&self) -> bool {
    matches!(self.state, State::Asking(_))
  }

  /// The line for the bottom border: the prompt or the outcome of the last `k`.
  pub(super) fn note(&self) -> Option<Note> {
    match &self.state {
      State::None => None,
      State::Asking(target) => Some(target.note("Kill ", "? y/n")),
      State::Done(note) => Some(note.clone()),
    }
  }

  /// `k` on process `pid` named `name`: asks to kill it, or says why it can't be.
  pub(super) fn ask(&mut self, pid: i32, name: &str) {
    self.state = match self.check(pid, name) {
      Ok(target) => State::Asking(target),
      Err(note) => State::Done(note),
    };
  }

  /// The target for a prompt about `pid`, or why there is none: it is refused, gone, or not
  /// signalled or read by this user (setuid processes it launched can take a signal and still be
  /// unreadable).
  fn check(&self, pid: i32, name: &str) -> Result<Target, Note> {
    if let Some(refusal) = refusal(pid) {
      return Err(refusal);
    }

    let about = |before: &str, after: String| Note::new(format!("{before}{pid} "), name, after);
    match self.sys.signal(pid, 0) {
      Ok(()) => {}
      Err(libc::ESRCH) => return Err(about("", " exited".into())),
      Err(libc::EPERM) => return Err(about("Not permitted to kill ", String::new())),
      Err(errno) => return Err(about("Failed to kill ", format!(": {}", strerror(errno)))),
    }

    match self.sys.identity(pid) {
      Some(identity) => Ok(Target { pid, name: name.to_string(), identity }),
      None => Err(about("Not permitted to kill ", String::new())),
    }
  }

  /// A key while the prompt is open: `y` (without Ctrl, Alt or Cmd) kills the target, any other
  /// key only closes the prompt.
  pub(super) fn answer(&mut self, key: KeyEvent) {
    let State::Asking(target) = &self.state else { return };
    let chord = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
    self.state = match key.code {
      KeyCode::Char('y') if !key.modifiers.intersects(chord) => State::Done(self.terminate(target)),
      _ => State::None,
    };
  }

  /// Closes the prompt, if open (a click, focus loss, the list hidden); an outcome stays.
  pub(super) fn cancel(&mut self) {
    if self.asking() {
      self.state = State::None;
    }
  }

  /// Sends SIGTERM to `target` if it is still the same process: its start time is read again
  /// right before, so a pid reused since the prompt is never signalled.
  fn terminate(&self, target: &Target) -> Note {
    if let Some(refusal) = refusal(target.pid) {
      return refusal;
    }
    if self.sys.identity(target.pid) != Some(target.identity) {
      return target.note("", " exited");
    }

    match self.sys.signal(target.pid, libc::SIGTERM) {
      Ok(()) => target.note("SIGTERM sent to ", ""),
      Err(libc::ESRCH) => target.note("", " exited"),
      Err(errno) => target.note("Failed to kill ", format!(": {}", strerror(errno))),
    }
  }
}

/// Fake system calls for tests: only the processes it is given exist, each call is logged, and
/// clones share all that, so a test keeps one while the app owns another. No test may use the
/// real ones: the pids of the test processes are real pids on the dev machine.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub(super) struct FakeSys(std::rc::Rc<std::cell::RefCell<FakeProcs>>);

#[cfg(test)]
#[derive(Debug, Default)]
struct FakeProcs {
  /// Start second (`None`: unreadable) and errno of the signals of each process.
  procs: std::collections::HashMap<i32, (Option<u64>, Option<i32>)>,
  /// `(pid, signal)` of every `signal` call, the checks with signal 0 too.
  calls: Vec<(i32, i32)>,
}

#[cfg(test)]
impl FakeSys {
  /// Processes `pids`, each started at the second of its pid.
  pub(super) fn with(pids: &[i32]) -> Self {
    let fake = Self::default();
    for &pid in pids {
      fake.0.borrow_mut().procs.insert(pid, (Some(pid as u64), None));
    }
    fake
  }

  /// Signals other than the checks with signal 0.
  pub(super) fn sent(&self) -> Vec<(i32, i32)> {
    self.0.borrow().calls.iter().copied().filter(|&(_, sig)| sig != 0).collect()
  }

  pub(super) fn calls(&self) -> Vec<(i32, i32)> {
    self.0.borrow().calls.clone()
  }

  /// `pid` exits and another process starts with it.
  pub(super) fn restart(&self, pid: i32) {
    let mut fake = self.0.borrow_mut();
    let start = &mut fake.procs.get_mut(&pid).unwrap().0;
    *start = start.map(|sec| sec + 1000);
  }

  /// `pid` stays, unreadable.
  pub(super) fn hide(&self, pid: i32) {
    self.0.borrow_mut().procs.get_mut(&pid).unwrap().0 = None;
  }

  /// Signals to `pid` fail with `errno`.
  pub(super) fn fail(&self, pid: i32, errno: i32) {
    self.0.borrow_mut().procs.get_mut(&pid).unwrap().1 = Some(errno);
  }

  pub(super) fn exit(&self, pid: i32) {
    self.0.borrow_mut().procs.remove(&pid);
  }
}

#[cfg(test)]
impl KillSys for FakeSys {
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32> {
    let mut fake = self.0.borrow_mut();
    fake.calls.push((pid, sig));
    match fake.procs.get(&pid) {
      None => Err(libc::ESRCH),
      Some(&(_, errno)) => errno.map_or(Ok(()), Err),
    }
  }

  fn identity(&self, pid: i32) -> Option<Identity> {
    let (sec, _) = *self.0.borrow().procs.get(&pid)?;
    Some(Identity { sec: sec?, usec: 0 })
  }
}

#[cfg(test)]
mod tests {
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

  use super::{FakeSys, Kill};

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  /// A `Kill` with the fake processes `pids`.
  fn kill_with(pids: &[i32]) -> (Kill, FakeSys) {
    let fake = FakeSys::with(pids);
    (Kill::new(Box::new(fake.clone())), fake)
  }

  fn note(kill: &Kill) -> Option<String> {
    kill.note().map(|note| note.to_string())
  }

  #[test]
  fn never_signals_launchd_macmon_or_a_group() {
    let own = std::process::id() as i32;
    let (mut kill, fake) = kill_with(&[1, own]);
    let cases = [
      (0, "Won't kill pid 0"),
      (-1, "Won't kill pid -1"),
      (-631, "Won't kill pid -631"),
      (1, "Won't kill launchd (pid 1)"),
      (own, "Won't kill macmon itself"),
    ];
    for (pid, message) in cases {
      kill.ask(pid, "proc");
      assert!(!kill.asking(), "{pid}");
      assert_eq!(note(&kill).as_deref(), Some(message), "{pid}");
      // `y` right after it is no answer
      kill.answer(key('y'));
    }
    assert_eq!(fake.calls(), [], "not even checked");
  }

  #[test]
  fn gone_or_unpermitted_processes_get_no_prompt() {
    let (mut kill, fake) = kill_with(&[631, 2301, 77]);
    fake.fail(631, libc::EPERM);
    fake.exit(2301);
    fake.hide(77);
    let cases = [
      (631, "WindowServer", "Not permitted to kill 631 WindowServer"),
      (2301, "Safari", "2301 Safari exited"),
      (77, "sudo", "Not permitted to kill 77 sudo"),
    ];
    for (pid, name, message) in cases {
      kill.ask(pid, name);
      assert!(!kill.asking(), "{pid}");
      assert_eq!(note(&kill).as_deref(), Some(message), "{pid}");
      kill.answer(key('y'));
    }
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn y_sends_sigterm_once() {
    let (mut kill, fake) = kill_with(&[631]);
    kill.ask(631, "WindowServer");
    assert!(kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("Kill 631 WindowServer? y/n"));
    assert_eq!(fake.calls(), [(631, 0)], "only checked");

    kill.answer(key('y'));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
    // the outcome stays until the next `k`; another `y` is no answer
    kill.answer(key('y'));
    kill.cancel();
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
  }

  #[test]
  fn other_keys_cancel_without_a_signal() {
    let (mut kill, fake) = kill_with(&[631]);
    let chord = |modifiers| KeyEvent::new(KeyCode::Char('y'), modifiers);
    let keys = [
      key('n'),
      key('Y'),
      key('k'),
      key('q'),
      KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
      KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
      chord(KeyModifiers::CONTROL),
      chord(KeyModifiers::ALT),
      chord(KeyModifiers::SUPER),
    ];
    for key in keys {
      kill.ask(631, "WindowServer");
      kill.answer(key);
      assert_eq!((kill.asking(), note(&kill)), (false, None), "{key:?}");
    }
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn y_after_a_restart_or_exit_sends_nothing() {
    let (mut kill, fake) = kill_with(&[631, 2301]);

    // the same pid, started again since the prompt
    kill.ask(631, "WindowServer");
    fake.restart(631);
    kill.answer(key('y'));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));

    kill.ask(2301, "Safari");
    fake.exit(2301);
    kill.answer(key('y'));
    assert_eq!(note(&kill).as_deref(), Some("2301 Safari exited"));
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn a_failed_signal_says_why() {
    let (mut kill, fake) = kill_with(&[631]);
    kill.ask(631, "WindowServer");
    fake.fail(631, libc::EINVAL);
    kill.answer(key('y'));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    let message = "Failed to kill 631 WindowServer: Invalid argument";
    assert_eq!(note(&kill).as_deref(), Some(message));
  }
}
