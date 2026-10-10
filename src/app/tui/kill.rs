//! Killing the selected process (`k`): a y/n prompt bound to the process it asks about, then
//! SIGTERM; the process is followed until it exits, and offered SIGKILL once it has had
//! `FORCE_AFTER` to. Nothing is signalled without `y`, and only the process the prompt was for.

use std::time::{Duration, Instant};
use std::{fmt, io, process};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;

use super::proc_view::cut_end;
use super::theme::{heading, text};
use crate::procs;

/// Time a process gets to exit after SIGTERM before SIGKILL is offered.
const FORCE_AFTER: Duration = Duration::from_secs(2);
/// How long a one-off message (an exit, an error, a refusal) stays.
const MESSAGE_FOR: Duration = Duration::from_secs(5);
/// The answer to `k` on a process that has not had `FORCE_AFTER` yet, or got SIGKILL.
const WAITING: &str = "waiting for exit…";
/// Cells of the process name a line keeps before key hints give way to it.
const NAME_KEPT: usize = 16;

/// What tells a process from a later one with the same pid: its start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
  sec: u64,
  usec: u64,
}

/// The start time in seconds and microseconds, as `ProcInfo::started` has it.
impl From<(u64, u64)> for Identity {
  fn from((sec, usec): (u64, u64)) -> Self {
    Self { sec, usec }
  }
}

/// System calls of `Kill`, so tests can use a fake.
pub(super) trait KillSys {
  /// `kill(pid, sig)`; the errno when it fails.
  fn signal(&self, pid: i32, sig: i32) -> Result<(), i32>;
  /// Start time of `pid`; `None` when it is gone, a zombie or not readable for this user.
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
    // a zombie has exited: only its entry is left until the parent reaps it
    (info.pbi_status != libc::SZOMB)
      .then_some(Identity { sec: info.pbi_start_tvsec, usec: info.pbi_start_tvusec })
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

fn signal_name(sig: i32) -> &'static str {
  match sig {
    libc::SIGKILL => "SIGKILL",
    _ => "SIGTERM",
  }
}

/// A line about one process for the bottom border: `before`, the process name, `after`. Key hints
/// give way to it first; only then the name is cut, so the rest of the line (`? y/n`) stays.
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

  /// Cells key hints give way to: the whole line with the name cut to `NAME_KEPT` cells.
  pub(super) fn wanted_width(&self) -> usize {
    self.fixed_width() + Span::raw(&self.name).width().min(NAME_KEPT)
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

  fn exited(&self) -> Note {
    self.note("", " exited")
  }
}

/// A process that was sent a signal, followed until it exits.
#[derive(Debug)]
struct Tracked {
  target: Target,
  /// SIGTERM, or SIGKILL after the force kill.
  signal: i32,
  sent: Instant,
  /// Still running `FORCE_AFTER` after the signal.
  late: bool,
  /// The force-kill prompt is open: it closes with the tracking.
  asking: bool,
}

impl Tracked {
  fn new(target: Target, signal: i32, sent: Instant) -> Self {
    Self { target, signal, sent, late: false, asking: false }
  }

  /// `SIGTERM sent to …`, then `… still running · k force kill` (no force kill after SIGKILL).
  fn note(&self) -> Note {
    match (self.late, self.signal) {
      (false, sig) => self.target.note(&format!("{} sent to ", signal_name(sig)), ""),
      (true, libc::SIGTERM) => self.target.note("", " still running · k force kill"),
      (true, _) => self.target.note("", " still running"),
    }
  }
}

/// Whether `target` is gone: no process with its pid, another one, or a zombie (unreadable).
fn gone(sys: &dyn KillSys, target: &Target) -> bool {
  sys.signal(target.pid, 0) == Err(libc::ESRCH) || sys.identity(target.pid) != Some(target.identity)
}

/// A one-off message.
#[derive(Debug)]
enum Message {
  /// `WAITING`: it goes as soon as SIGKILL is offered.
  Waiting,
  /// An exit, an error or a refusal.
  Note(Note),
}

impl Message {
  fn note(&self) -> Note {
    match self {
      Self::Waiting => Note::plain(WAITING),
      Self::Note(note) => note.clone(),
    }
  }
}

/// What became of a signal.
enum Sent {
  Yes,
  /// The process exited before it: nothing was sent.
  Gone,
  Failed(Note),
}

/// Kill state: the prompt, the process signalled last until it exits, and a one-off message,
/// with the system calls it goes through. It takes the time of each call, so tests choose it.
pub(super) struct Kill {
  sys: Box<dyn KillSys>,
  /// The prompt to SIGTERM a process (the force-kill prompt is in `tracked`).
  prompt: Option<Target>,
  tracked: Option<Tracked>,
  /// A one-off message with the time it was shown, for `MESSAGE_FOR`.
  message: Option<(Message, Instant)>,
}

/// The real system calls.
impl Default for Kill {
  fn default() -> Self {
    Self::new(Box::new(LibcSys))
  }
}

impl fmt::Debug for Kill {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.debug_struct("Kill")
      .field("prompt", &self.prompt)
      .field("tracked", &self.tracked)
      .field("message", &self.message)
      .finish_non_exhaustive()
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
    Self { sys, prompt: None, tracked: None, message: None }
  }

  /// Whether a prompt is open: every key goes to `answer` then.
  pub(super) fn asking(&self) -> bool {
    self.prompt.is_some() || self.forcing()
  }

  /// Whether the force-kill prompt is open.
  pub(super) fn forcing(&self) -> bool {
    self.tracked.as_ref().is_some_and(|tracked| tracked.asking)
  }

  /// The line for the bottom border while the process `selected` (a pid) is selected: a prompt,
  /// else a message, else the signalled process while it is selected or nothing is.
  pub(super) fn note(&self, selected: Option<i32>) -> Option<Note> {
    if let Some(target) = &self.prompt {
      return Some(target.note("Kill ", "? y/n"));
    }
    match (&self.tracked, &self.message) {
      (Some(tracked), _) if tracked.asking => Some(tracked.target.note("Force kill ", "? y/n")),
      (_, Some((message, _))) => Some(message.note()),
      (Some(tracked), None) if selected.is_none_or(|pid| pid == tracked.target.pid) => {
        Some(tracked.note())
      }
      _ => None,
    }
  }

  /// Shows `note` from `now` for `MESSAGE_FOR`.
  fn show(&mut self, note: Note, now: Instant) {
    self.message = Some((Message::Note(note), now));
  }

  /// A tick at `now`: drops an old message and checks the signalled process. Gone: `exited`, the
  /// tracking ends and the force-kill prompt closes. Still running `FORCE_AFTER` after the signal:
  /// `still running` (SIGKILL is offered after SIGTERM).
  pub(super) fn tick(&mut self, now: Instant) {
    if self.message.as_ref().is_some_and(|(_, shown)| now.duration_since(*shown) >= MESSAGE_FOR) {
      self.message = None;
    }

    let Some(tracked) = &mut self.tracked else { return };
    if gone(&*self.sys, &tracked.target) {
      let note = tracked.target.exited();
      self.tracked = None;
      self.show(note, now);
    } else if !tracked.late && now.duration_since(tracked.sent) >= FORCE_AFTER {
      tracked.late = true;
      // no more waiting: the force kill is offered
      if matches!(self.message, Some((Message::Waiting, _))) {
        self.message = None;
      }
    }
  }

  /// `k` at `now` on process `pid` named `name`, sampled with the start time `started` (`None`
  /// when unknown). The signalled process: `waiting for exit…` until it has had `FORCE_AFTER`
  /// after SIGTERM, then the force-kill prompt. Another process: asks to kill it (the tracking
  /// stays until that is confirmed), or says why it can't be.
  pub(super) fn ask(&mut self, pid: i32, name: &str, started: Option<Identity>, now: Instant) {
    // a signalled process that exited since the last tick is asked about anew
    self.tick(now);

    if let Some(tracked) = self.tracked.as_mut().filter(|tracked| tracked.target.pid == pid) {
      if tracked.late && tracked.signal == libc::SIGTERM {
        tracked.asking = true;
        self.message = None;
      } else {
        self.message = Some((Message::Waiting, now));
      }
      return;
    }

    match self.check(pid, name, started) {
      Ok(target) => {
        self.prompt = Some(target);
        self.message = None;
      }
      Err(note) => self.show(note, now),
    }
  }

  /// The target for a prompt about `pid`, or why there is none: it is refused, gone (also when
  /// it is not the process sampled with the start time `started` any more, so the prompt never
  /// names one process and asks about another), or not signalled or read by this user (setuid
  /// processes it launched can take a signal and still be unreadable).
  fn check(&self, pid: i32, name: &str, started: Option<Identity>) -> Result<Target, Note> {
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

    match (self.sys.identity(pid), started) {
      (identity, Some(started)) if identity != Some(started) => Err(about("", " exited".into())),
      (Some(identity), _) => Ok(Target { pid, name: name.to_string(), identity }),
      (None, _) => Err(about("Not permitted to kill ", String::new())),
    }
  }

  /// A key at `now` while a prompt is open: `y` (without Ctrl, Alt or Cmd) sends its signal, any
  /// other key only closes the prompt.
  pub(super) fn answer(&mut self, key: KeyEvent, now: Instant) {
    let chord = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
    let yes = key.code == KeyCode::Char('y') && !key.modifiers.intersects(chord);
    if let Some(target) = self.prompt.take() {
      if yes {
        self.terminate(target, now);
      }
    } else if let Some(tracked) = self.tracked.as_mut().filter(|tracked| tracked.asking) {
      tracked.asking = false;
      if yes {
        self.force(now);
      }
    }
  }

  /// Closes a prompt, if open (a click, focus loss, the list hidden); the tracking and a message
  /// stay.
  pub(super) fn cancel(&mut self) {
    self.prompt = None;
    if let Some(tracked) = &mut self.tracked {
      tracked.asking = false;
    }
  }

  /// Sends `sig` to `target` if it is still the same process: its start time is read again
  /// right before, so a pid reused since the prompt is never signalled.
  fn send(&self, target: &Target, sig: i32) -> Sent {
    if let Some(refusal) = refusal(target.pid) {
      return Sent::Failed(refusal);
    }
    if self.sys.identity(target.pid) != Some(target.identity) {
      return Sent::Gone;
    }

    match self.sys.signal(target.pid, sig) {
      Ok(()) => Sent::Yes,
      Err(libc::ESRCH) => Sent::Gone,
      Err(errno) => Sent::Failed(target.note("Failed to kill ", format!(": {}", strerror(errno)))),
    }
  }

  /// SIGTERM to `target` at `now`; once sent, it is tracked instead of the process before.
  fn terminate(&mut self, target: Target, now: Instant) {
    match self.send(&target, libc::SIGTERM) {
      Sent::Yes => {
        self.tracked = Some(Tracked::new(target, libc::SIGTERM, now));
        self.message = None;
      }
      Sent::Gone => self.show(target.exited(), now),
      Sent::Failed(note) => self.show(note, now),
    }
  }

  /// SIGKILL at `now` to the tracked process, which is then tracked again (no further escalation).
  fn force(&mut self, now: Instant) {
    let Some(tracked) = self.tracked.take() else { return };
    match self.send(&tracked.target, libc::SIGKILL) {
      Sent::Yes => {
        self.tracked = Some(Tracked::new(tracked.target, libc::SIGKILL, now));
        self.message = None;
      }
      Sent::Gone => self.show(tracked.target.exited(), now),
      Sent::Failed(note) => {
        self.tracked = Some(tracked);
        self.show(note, now);
      }
    }
  }
}

/// Fake system calls for tests: only the processes it is given exist, each call is logged, and
/// clones share all that, so a test keeps one while the app owns another. No test may use the
/// real ones (but the one with its own child): the pids of the test processes are real pids on the
/// dev machine.
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

  /// `pid` stays, unreadable: also how a zombie looks (`LibcSys` reads `SZOMB` as unreadable).
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
  use std::process::{Child, Command};
  use std::time::{Duration, Instant};

  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

  use super::{FakeSys, Identity, Kill, LibcSys};
  use crate::procs;

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  /// A `Kill` with the fake processes `pids`.
  fn kill_with(pids: &[i32]) -> (Kill, FakeSys) {
    let fake = FakeSys::with(pids);
    (Kill::new(Box::new(fake.clone())), fake)
  }

  fn note(kill: &Kill) -> Option<String> {
    kill.note(None).map(|note| note.to_string())
  }

  /// A clock for one test: `at(ms)` is `ms` after its start.
  fn clock() -> impl Fn(u64) -> Instant {
    let start = Instant::now();
    move |ms| start + Duration::from_millis(ms)
  }

  /// `kill` asked about `pid` and told `y` at `at`.
  fn terminate(kill: &mut Kill, pid: i32, name: &str, at: Instant) {
    kill.ask(pid, name, None, at);
    assert!(kill.asking(), "{pid}");
    kill.answer(key('y'), at);
  }

  #[test]
  fn never_signals_launchd_macmon_or_a_group() {
    let at = clock();
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
      kill.ask(pid, "proc", None, at(0));
      assert!(!kill.asking(), "{pid}");
      assert_eq!(note(&kill).as_deref(), Some(message), "{pid}");
      // `y` right after it is no answer
      kill.answer(key('y'), at(0));
    }
    assert_eq!(fake.calls(), [], "not even checked");
  }

  #[test]
  fn gone_or_unpermitted_processes_get_no_prompt() {
    let at = clock();
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
      kill.ask(pid, name, None, at(0));
      assert!(!kill.asking(), "{pid}");
      assert_eq!(note(&kill).as_deref(), Some(message), "{pid}");
      kill.answer(key('y'), at(0));
    }
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn y_sends_sigterm_once() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    kill.ask(631, "WindowServer", None, at(0));
    assert!(kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("Kill 631 WindowServer? y/n"));
    assert_eq!(fake.calls(), [(631, 0)], "only checked");

    kill.answer(key('y'), at(0));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
    // another `y` is no answer
    kill.answer(key('y'), at(0));
    kill.cancel();
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
  }

  #[test]
  fn other_keys_cancel_without_a_signal() {
    let at = clock();
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
      kill.ask(631, "WindowServer", None, at(0));
      kill.answer(key, at(0));
      assert_eq!((kill.asking(), note(&kill)), (false, None), "{key:?}");
    }
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn y_after_a_restart_or_exit_sends_nothing() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631, 2301]);

    // the same pid, started again since the prompt
    kill.ask(631, "WindowServer", None, at(0));
    fake.restart(631);
    kill.answer(key('y'), at(0));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));

    kill.ask(2301, "Safari", None, at(0));
    fake.exit(2301);
    kill.answer(key('y'), at(0));
    assert_eq!(note(&kill).as_deref(), Some("2301 Safari exited"));
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn k_on_a_pid_restarted_since_the_sample_says_exited() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    // the row was sampled before the restart: its name is the old process'
    let sampled = Some(Identity::from((631, 0)));
    fake.restart(631);
    kill.ask(631, "WindowServer", sampled, at(0));
    assert!(!kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    kill.answer(key('y'), at(0));

    // the start time of the process running now opens the prompt
    kill.ask(631, "WindowServer", Some(Identity::from((1631, 0))), at(100));
    assert!(kill.asking());
    kill.cancel();
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn a_failed_signal_says_why() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    kill.ask(631, "WindowServer", None, at(0));
    fake.fail(631, libc::EINVAL);
    kill.answer(key('y'), at(0));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    let message = "Failed to kill 631 WindowServer: Invalid argument";
    assert_eq!(note(&kill).as_deref(), Some(message));
  }

  #[test]
  fn an_exit_before_or_after_the_force_kill_offer_ends_the_tracking() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631, 2301]);

    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.tick(at(250));
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
    fake.exit(631);
    kill.tick(at(500));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));

    // the tick at 2 s, not the number of ticks, makes it late
    terminate(&mut kill, 2301, "Safari", at(1000));
    kill.tick(at(2999));
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 2301 Safari"));
    kill.tick(at(3000));
    assert_eq!(note(&kill).as_deref(), Some("2301 Safari still running · k force kill"));
    fake.exit(2301);
    kill.tick(at(3250));
    assert_eq!(note(&kill).as_deref(), Some("2301 Safari exited"));

    // nothing is tracked any more: the message goes and nothing comes back
    kill.tick(at(8250));
    assert_eq!(note(&kill), None);
    assert_eq!(fake.sent(), [(631, libc::SIGTERM), (2301, libc::SIGTERM)]);
  }

  #[test]
  fn a_zombie_counts_as_exited() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    // it still takes signal 0, but its start time is unreadable
    fake.hide(631);
    kill.tick(at(250));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    kill.ask(631, "WindowServer", None, at(500));
    assert!(!kill.asking());
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn k_before_2s_only_waits() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.ask(631, "WindowServer", None, at(1000));
    assert!(!kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("waiting for exit…"));
    kill.answer(key('y'), at(1100));

    // the offer replaces the wait at once
    kill.tick(at(2000));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer still running · k force kill"));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn y_to_the_force_kill_prompt_sends_sigkill_once() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    // without a tick since 2 s too
    kill.ask(631, "WindowServer", None, at(2100));
    assert!(kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("Force kill 631 WindowServer? y/n"));
    kill.answer(key('y'), at(2200));
    assert_eq!(note(&kill).as_deref(), Some("SIGKILL sent to 631 WindowServer"));

    // tracked the same way, without another offer
    kill.tick(at(4200));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer still running"));
    kill.ask(631, "WindowServer", None, at(4300));
    assert!(!kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("waiting for exit…"));
    fake.exit(631);
    kill.tick(at(4500));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM), (631, libc::SIGKILL)]);
  }

  #[test]
  fn a_failed_sigkill_keeps_the_tracking() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.tick(at(2000));
    kill.ask(631, "WindowServer", None, at(2100));
    assert!(kill.asking());
    fake.fail(631, libc::EINVAL);
    kill.answer(key('y'), at(2200));
    let message = "Failed to kill 631 WindowServer: Invalid argument";
    assert_eq!(note(&kill).as_deref(), Some(message));

    // still followed after the message, and offered SIGKILL again
    kill.tick(at(7200));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer still running · k force kill"));
    kill.ask(631, "WindowServer", None, at(7300));
    assert_eq!(note(&kill).as_deref(), Some("Force kill 631 WindowServer? y/n"));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM), (631, libc::SIGKILL)]);
  }

  #[test]
  fn a_reused_pid_never_gets_sigkill() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.tick(at(2000));
    kill.ask(631, "WindowServer", None, at(2100));
    assert!(kill.asking());
    // exits and its pid is taken between the ticks
    fake.restart(631);
    kill.answer(key('y'), at(2200));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    kill.tick(at(2250));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn an_exit_while_the_force_kill_prompt_is_open_closes_it() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.tick(at(2000));
    kill.ask(631, "WindowServer", None, at(2100));
    assert!(kill.asking());
    fake.exit(631);
    kill.tick(at(2250));
    assert!(!kill.asking());
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    kill.answer(key('y'), at(2300));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn a_prompt_for_another_process_replaces_the_tracking_only_at_y() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631, 2301]);
    terminate(&mut kill, 631, "WindowServer", at(0));
    kill.tick(at(2000));
    let still_running = Some("631 WindowServer still running · k force kill");

    // cancelled or answered with another key: the tracking goes on
    kill.ask(2301, "Safari", None, at(2100));
    assert_eq!(note(&kill).as_deref(), Some("Kill 2301 Safari? y/n"));
    kill.cancel();
    assert_eq!(note(&kill).as_deref(), still_running);
    kill.ask(2301, "Safari", None, at(2200));
    kill.answer(key('n'), at(2200));
    assert_eq!(note(&kill).as_deref(), still_running);
    kill.ask(631, "WindowServer", None, at(2300));
    assert_eq!(note(&kill).as_deref(), Some("Force kill 631 WindowServer? y/n"));
    kill.cancel();

    // ticks go on behind a prompt; `y` replaces the tracking
    kill.ask(2301, "Safari", None, at(2400));
    kill.tick(at(2500));
    kill.answer(key('y'), at(2600));
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 2301 Safari"));
    fake.exit(631);
    kill.tick(at(2750));
    assert_eq!(note(&kill).as_deref(), Some("SIGTERM sent to 2301 Safari"));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM), (2301, libc::SIGTERM)]);
  }

  #[test]
  fn one_off_messages_go_after_5s() {
    let at = clock();
    let (mut kill, fake) = kill_with(&[631, 2301]);
    kill.ask(1, "launchd", None, at(0));
    kill.tick(at(4999));
    assert_eq!(note(&kill).as_deref(), Some("Won't kill launchd (pid 1)"));
    kill.tick(at(5000));
    assert_eq!(note(&kill), None);

    // over the tracking: it comes back after them, and stays while the process runs
    terminate(&mut kill, 631, "WindowServer", at(6000));
    kill.tick(at(8000));
    kill.ask(1, "launchd", None, at(8100));
    assert_eq!(note(&kill).as_deref(), Some("Won't kill launchd (pid 1)"));
    kill.tick(at(13100));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer still running · k force kill"));
    kill.tick(at(60_000));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer still running · k force kill"));

    fake.exit(631);
    kill.tick(at(60_250));
    kill.tick(at(65_249));
    assert_eq!(note(&kill).as_deref(), Some("631 WindowServer exited"));
    kill.tick(at(65_250));
    assert_eq!(note(&kill), None);
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
  fn a_real_child_exits_on_sigterm() {
    use std::os::unix::process::ExitStatusExt;

    let mut child = OwnChild(Command::new("sleep").arg("30").spawn().unwrap());
    let pid = child.0.id() as i32;
    let mut kill = Kill::new(Box::new(LibcSys));
    // the start time as the process sampler reads it
    let info = procs::bsd_info(pid).unwrap();
    let started = Some(Identity::from((info.pbi_start_tvsec, info.pbi_start_tvusec)));
    kill.ask(pid, "sleep", started, Instant::now());
    assert!(kill.asking(), "{:?}", note(&kill));
    kill.answer(key('y'), Instant::now());
    assert_eq!(note(&kill), Some(format!("SIGTERM sent to {pid} sleep")));

    // followed with the real clock before it is reaped: a zombie, so its pid is not reused
    let exited = Some(format!("{pid} sleep exited"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while note(&kill) != exited && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(50));
      kill.tick(Instant::now());
    }
    assert_eq!(note(&kill), exited);
    let status = child.0.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGTERM));
  }
}
