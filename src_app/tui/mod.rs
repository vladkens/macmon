//! Terminal user interface.

mod boxes;
mod help;
mod layout;
mod proc_view;
mod store;
mod theme;
mod widgets;

use std::io::{self, Stdout, Write, stdout};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ratatui::crossterm::{
  ExecutableCommand, cursor,
  event::{
    self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
  },
  terminal,
};
use ratatui::prelude::*;

use crate::config::{Config, TUI_MIN_MS};
use crate::procs::{ProcInfo, ProcSampler};
use layout::{LayoutPlan, compute_layout};
use macmon::{Metrics, Sampler, SocInfo};
use proc_view::ProcView;
use store::{CpuClusters, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

// MARK: Term utils

/// Set while the terminal is in raw mode on the alternate screen, possibly with mouse capture.
static TERM_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped, so an error return can't leave it in raw mode with mouse
/// capture on. Panics restore it in the panic hook (release builds abort without unwinding).
struct TermGuard;

impl Drop for TermGuard {
  fn drop(&mut self) {
    leave_term();
  }
}

/// Raw mode and the alternate screen; `run_loop` turns mouse capture on while the process list is
/// shown. Whatever happens next, the terminal is restored by the guard, the panic hook or
/// `leave_term`.
fn enter_term() -> WithError<(Terminal<CrosstermBackend<Stdout>>, TermGuard)> {
  std::panic::set_hook(Box::new(|info| {
    leave_term();
    eprintln!("{}", info);
  }));

  // set before the first change, so a failure half-way still undoes what was done
  TERM_ACTIVE.store(true, Ordering::SeqCst);
  let guard = TermGuard;
  terminal::enable_raw_mode()?;
  stdout().execute(terminal::EnterAlternateScreen)?;
  Ok((Terminal::new(CrosstermBackend::new(stdout()))?, guard))
}

/// Restores the terminal once, whichever comes first: the normal exit, an error return or a panic.
fn leave_term() {
  restore_term_once(&TERM_ACTIVE, &mut stdout(), terminal::disable_raw_mode);
}

/// Turns mouse capture off, leaves the alternate screen, shows the cursor (ratatui hides it while
/// drawing, and a panic aborts before the terminal is dropped) and turns raw mode off when
/// `active` is set, and clears it. Every step runs even if an earlier one fails. Returns whether
/// it ran.
fn restore_term_once(
  active: &AtomicBool,
  out: &mut impl Write,
  disable_raw_mode: impl FnOnce() -> io::Result<()>,
) -> bool {
  if !active.swap(false, Ordering::SeqCst) {
    return false;
  }

  let _ = out.execute(DisableMouseCapture);
  let _ = out.execute(terminal::LeaveAlternateScreen);
  let _ = out.execute(cursor::Show);
  let _ = disable_raw_mode();
  true
}

/// Turns mouse capture on or off to `want`, if it isn't already (`on`); returns the new state.
/// Only the process list uses the mouse, so capture is off while it is hidden and the terminal
/// selects text as usual.
fn set_mouse_capture(out: &mut impl Write, on: bool, want: bool) -> io::Result<bool> {
  match (on, want) {
    (false, true) => out.execute(EnableMouseCapture).map(|_| true),
    (true, false) => out.execute(DisableMouseCapture).map(|_| false),
    _ => Ok(on),
  }
}

// MARK: Threads

enum Event {
  Update(Box<Metrics>),
  /// A process sample with the number of the panel showing it was started in.
  Procs {
    showing: u64,
    procs: Vec<ProcInfo>,
  },
  Key(KeyEvent),
  Mouse(MouseEvent),
  /// Redraw: the periodic tick, and a resize, so the mouse targets follow the new layout at once.
  Tick,
}

/// App event of a terminal event: keys, left clicks and the wheel, and a resize as a redraw.
/// Mouse capture reports every move too; moves, drags and releases are dropped here, so they
/// don't cost a frame each.
fn input_event(event: event::Event) -> Option<Event> {
  match event {
    event::Event::Key(key) => Some(Event::Key(key)),
    event::Event::Mouse(mouse) => {
      let action = matches!(
        mouse.kind,
        MouseEventKind::Down(MouseButton::Left)
          | MouseEventKind::ScrollUp
          | MouseEventKind::ScrollDown
      );
      action.then_some(Event::Mouse(mouse))
    }
    event::Event::Resize(..) => Some(Event::Tick),
    _ => None,
  }
}

/// Window of the first process sample after the panel shows up, so the list fills in quickly.
const PROCS_WARMUP: Duration = Duration::from_millis(TUI_MIN_MS as u64);

/// Sends input events and a `Tick` every `tick` ms; stops once the app is gone.
fn run_inputs_thread(tx: mpsc::Sender<Event>, tick: u64) {
  let tick_rate = Duration::from_millis(tick);

  thread::spawn(move || {
    let mut last_tick = Instant::now();

    loop {
      if event::poll(tick_rate).unwrap()
        && let Some(event) = input_event(event::read().unwrap())
        && tx.send(event).is_err()
      {
        return;
      }

      if last_tick.elapsed() >= tick_rate {
        if tx.send(Event::Tick).is_err() {
          return;
        }
        last_tick = Instant::now();
      }
    }
  });
}

/// Sends metrics: the first sample after 100 ms, then one per interval; stops once the app is
/// gone.
fn run_sampler_thread(tx: mpsc::Sender<Event>, msec: Arc<RwLock<u32>>) {
  thread::spawn(move || {
    let mut sampler = Sampler::new().unwrap();
    let mut window = 100;

    loop {
      let metrics = sampler.get_metrics(window).unwrap();
      if tx.send(Event::Update(Box::new(metrics))).is_err() {
        return;
      }
      window = (*msec.read().unwrap()).max(TUI_MIN_MS);
    }
  });
}

/// Whether the process panel is on screen, shared with the process thread, which samples only
/// while it is. Every change wakes the thread, and every show starts a new showing, so the thread
/// also notices a hide when the panel is back before it gets to look.
#[derive(Debug, Default)]
struct ProcsShown {
  state: Mutex<Showing>,
  changed: Condvar,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Showing {
  /// The panel is on screen.
  on: bool,
  /// Times the panel was shown: the number of the current showing while `on`.
  count: u64,
}

impl ProcsShown {
  /// The number of the current showing while the panel is on screen.
  fn showing(&self) -> Option<u64> {
    let state = self.state.lock().unwrap();
    state.on.then_some(state.count)
  }

  fn set(&self, on: bool) {
    let mut state = self.state.lock().unwrap();
    if state.on != on {
      *state = Showing { on, count: state.count + u64::from(on) };
      self.changed.notify_all();
    }
  }

  /// Blocks until the panel is on screen; returns the number of that showing.
  fn wait_shown(&self) -> u64 {
    let state = self.changed.wait_while(self.state.lock().unwrap(), |state| !state.on);
    state.unwrap().count
  }

  /// Waits `timeout`, or less once the showing `count` ends (a hide, also with a show right
  /// after it); returns whether it goes on.
  fn wait_while_shown(&self, count: u64, timeout: Duration) -> bool {
    let same = |state: &mut Showing| *state == Showing { on: true, count };
    let state = self.state.lock().unwrap();
    let (mut state, _) = self.changed.wait_timeout_while(state, timeout, same).unwrap();
    same(&mut state)
  }
}

/// Sampler of a showing in `run_procs_thread`: samples processes with a new `ProcSampler`.
fn proc_sampler() -> impl FnMut() -> Vec<ProcInfo> {
  let mut sampler = ProcSampler::new();
  move || sampler.sample()
}

/// Sends `Event::Procs` every `msec` while the process panel is `shown` and blocks otherwise.
/// Each showing gets a new sampler from `new_sampler`, so rates after a hide don't average over
/// the hidden time; a hide ends the wait for the next sample at once, also during a long
/// interval. Every sample carries the showing it was started in, so the app can drop one that
/// a hide (and maybe a show) overtook while it ran. Exits when the receiver is gone.
fn run_procs_thread<S: FnMut() -> Vec<ProcInfo>>(
  tx: mpsc::Sender<Event>,
  msec: Arc<RwLock<u32>>,
  shown: Arc<ProcsShown>,
  new_sampler: impl Fn() -> S + Send + 'static,
) -> JoinHandle<()> {
  thread::spawn(move || {
    loop {
      let showing = shown.wait_shown();

      // the first sample only sets the baseline: its CPU and power rates are zero
      let started = Instant::now();
      let mut sample = new_sampler();
      sample();
      let mut wait = PROCS_WARMUP.saturating_sub(started.elapsed());

      while shown.wait_while_shown(showing, wait) {
        let started = Instant::now();
        if tx.send(Event::Procs { showing, procs: sample() }).is_err() {
          return;
        }
        let interval = Duration::from_millis((*msec.read().unwrap()).max(TUI_MIN_MS).into());
        wait = interval.saturating_sub(started.elapsed());
      }
    }
  })
}

// MARK: App

#[derive(Debug, Default)]
pub struct App {
  cfg: Config,
  /// Graph bars in three levels (blank, `▄`, `█`): Apple Terminal draws gaps between the eighth
  /// blocks.
  three_level_bars: bool,

  soc: SocInfo,
  mem: MemoryStore,

  cpu_power: PowerStore,
  gpu_power: PowerStore,
  ane_power: PowerStore,
  all_power: PowerStore,
  sys_power: PowerStore,

  cpu_temp: TempStore,
  gpu_temp: TempStore,
  fans: FanStore,

  /// CPU clusters, lowest tier first (E / P on M1–M4, P / S on M5+).
  clusters: CpuClusters,
  igpu_freq: FreqStore,

  /// Process panel state with the latest process list (none until the first sample with rates).
  proc_view: ProcView,
  /// Whether the process panel is on screen; the process thread samples only then.
  procs_shown: Arc<ProcsShown>,
  /// The window of the last frame has room for the process list, whether it is shown or not;
  /// `p` works only then (no room before the first frame).
  procs_fit: bool,

  /// The help overlay (`?`) with its first line on screen, while it is open.
  help: Option<usize>,
}

impl App {
  pub fn new() -> WithError<Self> {
    Ok(Self::from_parts(SocInfo::new()?, Config::load()))
  }

  /// App for the chip `soc` with the settings `cfg`. The CPU clusters come from the chip, so the
  /// first frame already has their boxes and the chip title its core counts.
  fn from_parts(soc: SocInfo, cfg: Config) -> Self {
    let clusters = CpuClusters::from_soc(&soc);
    let proc_view = ProcView::new(cfg.proc_sort, cfg.proc_sort_desc);
    Self { cfg, soc, clusters, proc_view, ..Default::default() }
  }

  fn update_metrics(&mut self, data: Metrics) {
    self.cpu_power.push(data.cpu_power as f64);
    self.gpu_power.push(data.gpu_power as f64);
    self.ane_power.push(data.ane_power as f64);
    self.all_power.push(data.all_power as f64);
    self.sys_power.push(data.sys_power as f64);

    self.clusters.push(&store::cluster_samples(&data));
    let igpu = FreqSample::new(data.gpu_freq_mhz, data.gpu_scaled_ratio, data.gpu_active_ratio);
    self.igpu_freq.push(igpu);

    self.cpu_temp.push(data.temp.cpu_temp_avg);
    self.gpu_temp.push(data.temp.gpu_temp_avg);
    self.fans.push(data.fans);

    self.mem.push(data.memory);
  }

  fn procs_visible(&self) -> bool {
    self.procs_shown.showing().is_some()
  }

  /// Follows the process list visibility (`p` or auto-hidden). A hidden panel drops its
  /// list, so it reads "collecting…" when shown again instead of showing stale rows, and ends
  /// filter input, so keys don't go to a filter that isn't on screen.
  fn set_procs_visible(&mut self, visible: bool) {
    self.procs_shown.set(visible);
    if !visible {
      self.proc_view.clear();
    }
  }

  /// Stores a process sample started in the panel's `showing`. Only one of the current showing
  /// gets in: one still in flight when the panel got hidden is dropped, also when the panel is
  /// back by now, so a re-shown panel reads "collecting…" until its own first sample.
  fn update_procs(&mut self, showing: u64, procs: Vec<ProcInfo>) {
    if self.procs_shown.showing() == Some(showing) {
      self.proc_view.set_procs(procs);
    }
  }

  /// Applies one event. Returns `Break` when the app should quit. Keys can change the interval,
  /// which `msec` hands on to the sampling threads.
  fn handle_event(&mut self, event: Event, msec: &RwLock<u32>) -> ControlFlow<()> {
    let flow = match event {
      Event::Key(key) => self.handle_key(key),
      Event::Mouse(mouse) => {
        self.handle_mouse(mouse);
        return ControlFlow::Continue(());
      }
      Event::Update(data) => {
        self.update_metrics(*data);
        return ControlFlow::Continue(());
      }
      Event::Procs { showing, procs } => {
        self.update_procs(showing, procs);
        return ControlFlow::Continue(());
      }
      Event::Tick => return ControlFlow::Continue(()),
    };

    *msec.write().unwrap() = self.cfg.interval();
    flow
  }

  /// Applies a key press to the app state. Returns `Break` when the app should quit. The help
  /// overlay takes every key while it is open; then keys of the process panel (only while it is
  /// on screen) take precedence, and while a filter is typed every key except Ctrl-C goes to it.
  fn handle_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
      return ControlFlow::Break(());
    }

    if let Some(scroll) = self.help {
      self.help = match key.code {
        KeyCode::Esc | KeyCode::Char('?' | 'q') => None,
        KeyCode::Up => Some(scroll.saturating_sub(1)),
        KeyCode::Down => Some(scroll + 1),
        _ => Some(scroll),
      };
      return ControlFlow::Continue(());
    }

    if self.procs_visible() && self.update_proc_view(|view| view.handle_key(key)) {
      return ControlFlow::Continue(());
    }

    match key.code {
      KeyCode::Char('q') => return ControlFlow::Break(()),
      KeyCode::Char('?') => self.help = Some(0),
      KeyCode::Char('r') => self.cfg.toggle_ratio_mode(),
      KeyCode::Char('+') => self.cfg.inc_interval(),
      KeyCode::Char('=') => self.cfg.inc_interval(), // fallback to press without shift
      KeyCode::Char('-') => self.cfg.dec_interval(),
      // a window too small for the list keeps it hidden whatever the setting says, so `p` would
      // change a setting without a change on screen
      KeyCode::Char('p') if self.procs_fit => self.cfg.toggle_procs(),
      KeyCode::Char('v') => self.cfg.toggle_view_type(),
      _ => {}
    }

    ControlFlow::Continue(())
  }

  /// Applies a mouse event at the cells of the last frame, only while the process list is on
  /// screen (mouse capture is off otherwise): it goes to the process list. With the help open, a
  /// click closes it and the wheel scrolls it.
  fn handle_mouse(&mut self, mouse: MouseEvent) {
    if !self.procs_visible() {
      return;
    }

    let Some(scroll) = self.help else {
      self.update_proc_view(|view| view.handle_mouse(mouse));
      return;
    };
    self.help = match mouse.kind {
      MouseEventKind::Down(MouseButton::Left) => None,
      MouseEventKind::ScrollUp => Some(scroll.saturating_sub(3)),
      MouseEventKind::ScrollDown => Some(scroll + 3),
      _ => Some(scroll),
    };
  }

  /// Runs `f` on the process panel state and saves a changed sort order.
  fn update_proc_view<R>(&mut self, f: impl FnOnce(&mut ProcView) -> R) -> R {
    let sort = (self.proc_view.sort, self.proc_view.sort_desc);
    let result = f(&mut self.proc_view);
    if (self.proc_view.sort, self.proc_view.sort_desc) != sort {
      self.cfg.set_proc_sort(self.proc_view.sort, self.proc_view.sort_desc);
    }
    result
  }

  /// Screen layout for the current settings and metrics.
  fn layout(&self, area: Rect) -> LayoutPlan {
    compute_layout(area, self.cfg.show_procs, self.clusters.items.len())
  }

  /// Layout of a frame for the window `area`, with the state that follows the window updated
  /// before anything is drawn: whether the process list fits, and whether it is on screen.
  fn layout_frame(&mut self, area: Rect) -> LayoutPlan {
    self.procs_fit = layout::procs_fit(area);
    let plan = self.layout(area);
    self.set_procs_visible(plan.proc.is_some());
    plan
  }

  fn render(&mut self, f: &mut Frame) {
    let plan = self.layout_frame(f.area());

    // the key hints go on the lowest box: the process box, or the metrics box without it
    self.render_metrics_box(f, &plan);
    if let Some(r) = plan.proc {
      self.render_proc_box(f, r);
    }
    if let Some(scroll) = self.help {
      self.help = Some(help::render(f, f.area(), scroll));
    }
  }

  pub fn run_loop(&mut self, interval: Option<u32>) -> WithError<()> {
    // an interval from `-i` is used for this run only, the saved one stays
    if let Some(interval) = interval {
      self.cfg.set_run_interval(interval);
    }
    let msec = Arc::new(RwLock::new(self.cfg.interval()));

    let (tx, rx) = mpsc::channel::<Event>();
    run_sampler_thread(tx.clone(), msec.clone());
    run_procs_thread(tx.clone(), msec.clone(), self.procs_shown.clone(), proc_sampler);

    // the guard restores the terminal on every way out of here, `?` included
    let (mut term, _guard) = enter_term()?;
    run_inputs_thread(tx.clone(), 250);
    self.three_level_bars = theme::detect_three_level_bars();

    let mut mouse = false;
    loop {
      term.draw(|f| self.render(f))?;
      mouse = set_mouse_capture(&mut stdout(), mouse, self.procs_visible())?;
      if self.handle_event(rx.recv()?, &msec).is_break() {
        break;
      }
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::io::{self, Write};
  use std::ops::ControlFlow;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::{Arc, Mutex, RwLock, mpsc};
  use std::thread;
  use std::time::{Duration, Instant};

  use macmon::{FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::ExecutableCommand;
  use ratatui::crossterm::event::{
    self as term_event, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
  };
  use ratatui::layout::{Margin, Rect};
  use ratatui::style::{Color, Modifier};

  use super::layout::Metric;
  use super::store::{CpuClusters, FreqSample};
  use super::theme::{self, gradient};
  use super::{
    App, Event, PROCS_WARMUP, ProcsShown, input_event, proc_sampler, restore_term_once,
    run_procs_thread, set_mouse_capture,
  };
  use crate::config::{Config, ProcSort, RatioMode, TUI_MAX_MS, TUI_MIN_MS, TempConfig, ViewType};
  use crate::procs::ProcInfo;

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
  }

  fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
  }

  fn press(app: &mut App, code: KeyCode) -> ControlFlow<()> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
  }

  /// Screen column where `text` starts in the rendered `line`.
  fn x_of(line: &str, text: &str) -> u16 {
    let i = line.find(text).unwrap_or_else(|| panic!("no {text:?} in {line}"));
    line[..i].chars().count() as u16
  }

  #[test]
  fn restoring_the_terminal_turns_mouse_capture_off_once() {
    let active = AtomicBool::new(true);
    let mut out = vec![];
    let mut raw_off = 0;
    assert!(restore_term_once(&active, &mut out, || {
      raw_off += 1;
      Ok(())
    }));

    // every mouse mode that mouse capture turns on goes off, then back to the main screen
    let text = String::from_utf8(out).unwrap();
    let mut on = vec![];
    on.execute(EnableMouseCapture).unwrap();
    let on = String::from_utf8(on).unwrap();
    let modes: Vec<&str> = on.split("\x1b[?").filter_map(|mode| mode.strip_suffix('h')).collect();
    assert_eq!(modes.len(), 5, "{on:?}");
    for mode in modes {
      assert!(text.contains(&format!("\x1b[?{mode}l")), "{mode} stays on: {text:?}");
    }
    // the main screen, with the cursor that drawing hid (a panic never drops the terminal)
    assert!(text.ends_with("\x1b[?1049l\x1b[?25h"), "{text:?}");
    assert_eq!(raw_off, 1);

    // the normal exit, the guard of an error return and the panic hook can all get here: only
    // the first one writes anything
    let mut out = vec![];
    assert!(!restore_term_once(&active, &mut out, || {
      raw_off += 1;
      Ok(())
    }));
    assert!(out.is_empty());
    assert_eq!(raw_off, 1);
  }

  #[test]
  fn restoring_the_terminal_runs_every_step_despite_errors() {
    struct Closed;
    impl Write for Closed {
      fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
      }
      fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("closed"))
      }
    }

    let active = AtomicBool::new(true);
    let mut raw_off = false;
    let restored = restore_term_once(&active, &mut Closed, || {
      raw_off = true;
      Err(io::Error::other("not a tty"))
    });
    assert!(restored && raw_off && !active.load(Ordering::SeqCst));
  }

  #[test]
  fn input_thread_forwards_keys_clicks_wheel_and_resizes() {
    use MouseButton::*;
    use MouseEventKind::*;
    for kind in [Down(Left), ScrollUp, ScrollDown] {
      let event = input_event(term_event::Event::Mouse(mouse(kind, 3, 4)));
      assert!(matches!(event, Some(Event::Mouse(m)) if m == mouse(kind, 3, 4)), "{kind:?}");
    }
    let ignored = [Down(Right), Down(Middle), Up(Left), Drag(Left), Moved, ScrollLeft, ScrollRight];
    for kind in ignored {
      assert!(input_event(term_event::Event::Mouse(mouse(kind, 3, 4))).is_none(), "{kind:?}");
    }

    let event = input_event(term_event::Event::Key(key('q')));
    assert!(matches!(event, Some(Event::Key(k)) if k == key('q')));
    // a resize redraws at once, so clicks don't hit the cells of the old layout
    assert!(matches!(input_event(term_event::Event::Resize(80, 24)), Some(Event::Tick)));
    for other in [term_event::Event::FocusGained, term_event::Event::Paste("x".into())] {
      assert!(input_event(other).is_none());
    }
  }

  #[test]
  fn mouse_capture_turns_on_and_off_once() {
    let bytes = |command| {
      let mut out = vec![];
      match command {
        true => out.execute(EnableMouseCapture).unwrap(),
        false => out.execute(DisableMouseCapture).unwrap(),
      };
      out
    };
    for (on, want) in [(false, true), (true, false), (true, true), (false, false)] {
      let mut out = vec![];
      assert_eq!(set_mouse_capture(&mut out, on, want).unwrap(), want, "{on} -> {want}");
      let expected = if on == want { vec![] } else { bytes(want) };
      assert_eq!(out, expected, "{on} -> {want}");
    }
  }

  #[test]
  fn events_reach_their_handlers() {
    let file = TempConfig::new("events_reach_their_handlers");
    let mut app = saving_app(&file);
    let msec = RwLock::new(1000);
    let handle = |app: &mut App, event: Event| app.handle_event(event, &msec);

    // metrics and processes
    let metrics = Metrics { gpu_freq_mhz: 777, ..test_metrics() };
    assert!(handle(&mut app, Event::Update(Box::new(metrics))).is_continue());
    assert_eq!(app.igpu_freq.freq_mhz, 777);
    render_buffer(&mut app, 200, 50);
    let showing = app.procs_shown.showing().unwrap();
    assert!(handle(&mut app, Event::Procs { showing, procs: varied_procs() }).is_continue());
    assert_eq!(app.proc_view.row_count(), 3);
    assert!(handle(&mut app, Event::Tick).is_continue());

    // a key changing the interval hands it on to the sampling threads
    assert!(handle(&mut app, Event::Key(key('+'))).is_continue());
    assert_eq!(*msec.read().unwrap(), 1250);
    assert_eq!(file.saved()["interval"], 1250);

    // the mouse: a click on the MEM header sorts by it
    let buf = render_buffer(&mut app, 200, 50);
    let left = MouseEventKind::Down(MouseButton::Left);
    let at = mouse(left, x_of(&proc_row(&buf, 1), "MEM"), PROC_Y + 1);
    assert!(handle(&mut app, Event::Mouse(at)).is_continue());
    assert_eq!(app.proc_view.sort, ProcSort::Mem);
    assert!(handle(&mut app, Event::Key(key('-'))).is_continue());

    assert!(handle(&mut app, Event::Key(key('q'))).is_break());
    assert_eq!(*msec.read().unwrap(), 1000);
  }

  fn test_soc() -> SocInfo {
    SocInfo {
      chip_name: "Apple M3 Pro".to_string(),
      memory_gb: 36,
      ecpu_cores: 6,
      pcpu_cores: 6,
      ecpu_label: "E".to_string(),
      pcpu_label: "P".to_string(),
      gpu_cores: 18,
      ..Default::default()
    }
  }

  fn test_metrics() -> Metrics {
    Metrics {
      temp: TempMetrics { cpu_temp_avg: 45.0, gpu_temp_avg: 40.0 },
      memory: MemMetrics {
        ram_total: 36 << 30,
        ram_usage: 20 << 30,
        swap_total: 2 << 30,
        swap_usage: 1 << 30,
      },
      fans: vec![FanMetric { name: "fan0".to_string(), rpm: 1200, max_rpm: Some(6000) }],
      ecpu_freq_mhz: 1800,
      ecpu_scaled_ratio: 0.42,
      ecpu_active_ratio: 0.5,
      pcpu_freq_mhz: 3200,
      pcpu_scaled_ratio: 0.77,
      pcpu_active_ratio: 0.8,
      gpu_freq_mhz: 1400,
      gpu_scaled_ratio: 0.23,
      gpu_active_ratio: 0.3,
      cpu_power: 4.5,
      gpu_power: 2.0,
      ane_power: 0.1,
      all_power: 6.6,
      sys_power: 12.0,
      ..Default::default()
    }
  }

  /// App for `test_soc` with `samples` samples of `test_metrics` changed by `edit`.
  fn app_with_samples(samples: usize, edit: impl Fn(&mut Metrics)) -> App {
    let mut app = App::from_parts(test_soc(), Config::default());
    for _ in 0..samples {
      let mut metrics = test_metrics();
      edit(&mut metrics);
      app.update_metrics(metrics);
    }
    app
  }

  /// App with a few samples of `test_metrics` changed by `edit`.
  fn test_app_with(edit: impl Fn(&mut Metrics)) -> App {
    app_with_samples(3, edit)
  }

  fn test_app() -> App {
    test_app_with(|_| {})
  }

  /// `test_app` saving its settings to `file`.
  fn saving_app(file: &TempConfig) -> App {
    let mut app = App::from_parts(test_soc(), Config::load_from(Some(file.path())));
    for _ in 0..3 {
      app.update_metrics(test_metrics());
    }
    app
  }

  fn render_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    term.backend().buffer().clone()
  }

  /// Text of the whole frame, row after row.
  fn screen_text(buf: &Buffer) -> String {
    buf.content.iter().map(|cell| cell.symbol()).collect()
  }

  fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    screen_text(&render_buffer(app, width, height))
  }

  fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
  }

  /// Text of the one-row `area`.
  fn text(buf: &Buffer, area: Rect) -> String {
    (area.left()..area.right()).map(|x| buf[(x, area.y)].symbol()).collect()
  }

  /// Graph bar `▁`…`█`.
  fn is_bar(c: char) -> bool {
    ('▁'..='█').contains(&c)
  }

  #[test]
  fn quit_keys_break() {
    let mut app = App::default();
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
  }

  #[test]
  fn r_toggles_ratio_mode() {
    let mut app = App::default();
    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.handle_key(key('r')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.ratio_mode, RatioMode::Active);
  }

  #[test]
  fn plus_equals_minus_change_interval() {
    let mut app = App::default();
    assert_eq!(app.cfg.interval(), 1000);

    assert_eq!(app.handle_key(key('+')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval(), 1250);

    assert_eq!(app.handle_key(key('=')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval(), 1500);

    assert_eq!(app.handle_key(key('-')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval(), 1250);
  }

  #[test]
  fn unknown_keys_are_ignored() {
    let mut app = App::default();
    for code in [KeyCode::Char('x'), KeyCode::Esc, KeyCode::Enter, KeyCode::Up] {
      let event = KeyEvent::new(code, KeyModifiers::NONE);
      assert_eq!(app.handle_key(event), ControlFlow::Continue(()));
    }

    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval(), 1000);
    assert!(app.cfg.show_procs);

    // keys without a binding (the old theme, cores and panel keys among them) change nothing
    let file = TempConfig::new("unknown_keys_are_ignored");
    let mut app = with_procs(saving_app(&file), varied_procs());
    let screen = render_to_string(&mut app, 200, 50);
    for c in ['c', 'd', 'x', 'C', 'V', 'D', '0', '1', '5', '9'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?}");
    }
    assert!(!file.path().exists(), "nothing saved");
    assert!(!app.proc_view.typing() && app.proc_view.filter().is_empty());
    assert_eq!(render_to_string(&mut app, 200, 50), screen);
  }

  #[test]
  fn p_toggles_process_list() {
    let file = TempConfig::new("p_toggles_process_list");
    let mut app = with_procs(saving_app(&file), varied_procs());
    assert!(procs_active(&app));

    assert_eq!(app.handle_key(key('p')), ControlFlow::Continue(()));
    assert!(!app.cfg.show_procs);
    assert_eq!(file.saved()["show_procs"], false);

    // the metrics take the whole screen, the same hints move next to the power summary
    let buf = render_buffer(&mut app, 200, 50);
    let plan = app.layout(buf.area);
    assert_eq!((plan.top, plan.proc), (Some(buf.area), None));
    assert_eq!(row(&buf, 49), border(200, SUMMARY, &hints(6)));
    let screen = screen_text(&buf);
    assert!(!screen.contains(" proc ") && !screen.contains("WindowServer"));
    assert!(!procs_active(&app), "no process sampling while hidden");

    // shown again: collecting until the next sample, its controls in its box
    assert_eq!(app.handle_key(key('p')), ControlFlow::Continue(()));
    assert!(app.cfg.show_procs);
    assert_eq!(file.saved()["show_procs"], true);
    let buf = render_buffer(&mut app, 200, 50);
    let screen = screen_text(&buf);
    assert!(screen.contains("collecting…"));
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc ─ / filter ─ s sort ─"), "{title}");
    assert_eq!(row(&buf, PROC_Y - 1), border(200, SUMMARY, ""));
    assert_eq!(row(&buf, 49), hints_border(200, 6));
    assert!(procs_active(&app));
  }

  #[test]
  fn p_is_ignored_while_the_window_is_too_small() {
    let file = TempConfig::new("p_is_ignored_while_the_window_is_too_small");
    let mut app = saving_app(&file);

    // auto-hidden: `p` would change nothing on screen, so it changes no setting either
    let buf = render_buffer(&mut app, 100, 12);
    assert!(!procs_active(&app));
    assert!(app.handle_key(key('p')).is_continue());
    assert!(app.cfg.show_procs);
    assert!(!file.path().exists(), "nothing saved");
    // and the footer leaves it out
    let footer = " q quit | ? help | v graph | r scaled | -/+ 1000ms ";
    assert!(row(&buf, 11).ends_with(&format!("─{footer}─╯")), "{}", row(&buf, 11));

    // room again: the list is back, and so is `p`
    let buf = render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));
    assert_eq!(row(&buf, 49), hints_border(200, 6));
    assert!(app.handle_key(key('p')).is_continue());
    assert_eq!(file.saved()["show_procs"], false);

    // hidden with `p`: a small window keeps it hidden, so `p` does nothing either way, and the
    // footer leaves it out
    let buf = render_buffer(&mut app, 100, 12);
    assert!(row(&buf, 11).ends_with(&format!("─{footer}─╯")), "{}", row(&buf, 11));
    assert!(app.handle_key(key('p')).is_continue());
    assert!(!app.cfg.show_procs);
    assert_eq!(file.saved()["show_procs"], false);

    // room again: `p` shows it
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), border(200, SUMMARY, &hints(6)));
    assert!(app.handle_key(key('p')).is_continue());
    assert_eq!(file.saved()["show_procs"], true);
    assert!(render_to_string(&mut app, 200, 50).contains(" proc "));
  }

  /// App with every kind of colored cell on screen: metric boxes, processes with a selected row.
  fn colorful_app() -> App {
    let mut app = app_with_procs(varied_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    app
  }

  fn frame_colors(buf: &Buffer) -> impl Iterator<Item = Color> + '_ {
    buf.content.iter().flat_map(|cell| [cell.fg, cell.bg])
  }

  #[test]
  fn renders_terminal_colors_only() {
    // ANSI colors only, with either bar set
    let ansi = [Color::Reset, Color::DarkGray, Color::Green, Color::Yellow, Color::Red];
    for three_levels in [false, true] {
      let mut app = colorful_app();
      app.three_level_bars = three_levels;
      for (width, height) in [(200, 50), (80, 24), (60, 15)] {
        let buf = render_buffer(&mut app, width, height);
        let ctx = format!("three levels {three_levels} at {width}x{height}");
        assert_eq!((buf[(0, 0)].symbol(), buf[(0, 0)].fg), ("╭", Color::DarkGray), "{ctx}");
        for color in frame_colors(&buf) {
          assert!(ansi.contains(&color), "{color:?} in {ctx}");
        }
      }

      // all load levels show up: GPU 23%, E-CPU 42%, P-CPU 77%
      let buf = render_buffer(&mut app, 200, 50);
      for color in [Color::Green, Color::Yellow, Color::Red] {
        assert!(frame_colors(&buf).any(|c| c == color), "no {color:?}");
      }
    }
  }

  /// Full box titles of `test_metrics`: the original format without the RAM total and without
  /// alignment padding.
  const TITLES: [&str; 7] = [
    "E-CPU 42% @ 1800 MHz",
    "P-CPU 77% @ 3200 MHz",
    "GPU 23% @ 1400 MHz",
    "RAM 20.00 GB (55.6%) · SWAP 1.00 / 2.0 GB",
    "CPU 4.50W (4.50, 4.50)",
    "GPU 2.00W (2.00, 2.00)",
    "ANE 0.10W (0.10, 0.10)",
  ];

  /// Power summary of `test_metrics`, with its blank cells, on the bottom border of the metrics
  /// box.
  const SUMMARY: &str = " Power 6.60W (6.60, 6.60) | Fan 1200 RPM | Total 12.00W (12.00, 12.00) ";

  /// Global key hints, in the order of the original UI, with `? help`.
  const HINTS: [&str; 6] = ["q quit", "? help", "p procs", "v graph", "r scaled", "-/+ 1000ms"];

  /// Note on the bottom border of the process box while some processes have no power reading.
  const NOTE: &str = " POWER: own processes only ";

  /// The first `count` of `HINTS` joined, with a blank cell at both ends; empty for none.
  fn hints(count: usize) -> String {
    if count == 0 { String::new() } else { format!(" {} ", HINTS[..count].join(" | ")) }
  }

  /// Bottom border of a box `width` cells wide: `left` after `╰─`, `right` before `─╯`.
  fn border(width: usize, left: &str, right: &str) -> String {
    let dashes = width - 4 - left.chars().count() - right.chars().count();
    format!("╰─{left}{}{right}─╯", "─".repeat(dashes))
  }

  /// Bottom border of a box `width` cells wide with the first `count` of `HINTS` right-aligned.
  fn hints_border(width: usize, count: usize) -> String {
    border(width, "", &hints(count))
  }

  /// Top border of a box `width` cells wide: `left` after `╭─`, `right` (if any) before `─╮`.
  fn top_border(width: usize, left: &str, right: &str) -> String {
    let left = format!("─ {left} ");
    let right = if right.is_empty() { "─".to_string() } else { format!(" {right} ─") };
    let dashes = width - 2 - left.chars().count() - right.chars().count();
    format!("╭{left}{}{right}╮", "─".repeat(dashes))
  }

  /// Top border of a box `width` cells wide (at least 5) with the first of `variants` (`(left,
  /// right)` titles, longest first, `right` empty for none, both empty for no title) that fits
  /// uncut, or else the last one without its right title, the left one cut to the border.
  fn fitted_top(width: usize, variants: &[(&str, &str)]) -> String {
    let len = |s: &str| s.chars().count();
    // the left title and its blanks start 2 cells in; one cell between titles, 2 at the end
    let fits = |(left, right): (&str, &str)| match (left, right) {
      ("", "") => true,
      (_, "") => 2 + len(left) + 2 + 2 <= width,
      _ => 2 + len(left) + 2 + 1 + len(right) + 2 + 2 <= width,
    };
    match variants.iter().find(|v| fits(**v)) {
      Some(("", "")) => format!("╭{}╮", "─".repeat(width - 2)),
      Some((left, right)) => top_border(width, left, right),
      None => {
        let (left, _) = variants.last().unwrap();
        if fits((left, "")) {
          return top_border(width, left, "");
        }
        let cut: String = format!(" {left}").chars().take(width - 4).collect();
        format!("╭─{cut}─╮")
      }
    }
  }

  /// Top border of the metric box `metric` in a rendered frame.
  fn box_top(app: &App, buf: &Buffer, metric: Metric) -> String {
    let boxes = app.layout(buf.area).boxes;
    let (_, area) = boxes.into_iter().find(|(m, _)| *m == metric).expect("metric box");
    text(buf, Rect { height: 1, ..area })
  }

  /// Graph area (inside the borders) of the metric box `metric`.
  fn graph_area(app: &App, area: Rect, metric: Metric) -> Rect {
    let boxes = app.layout(area).boxes;
    let (_, area) = boxes.into_iter().find(|(m, _)| *m == metric).expect("metric box");
    area.inner(Margin::new(1, 1))
  }

  #[test]
  fn renders_original_titles() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    let screen = screen_text(&buf);
    for title in TITLES {
      assert!(screen.contains(&format!("╭─ {title} ─")), "missing {title}");
    }
    // whole-degree temperatures right on the CPU / GPU power boxes, none for ANE
    assert!(box_top(&app, &buf, Metric::CpuPower).ends_with("─ 45°C ─╮"));
    assert!(box_top(&app, &buf, Metric::GpuPower).ends_with("─ 40°C ─╮"));
    assert!(box_top(&app, &buf, Metric::AnePower).ends_with("───╮"));
    // the total RAM is in the chip title only
    assert!(!screen.contains("36.0 GB"));
    // the power summary on the bottom border of the metrics box, the process box below it
    assert_eq!(row(&buf, PROC_Y - 1), border(200, SUMMARY, ""));
  }

  #[test]
  fn renders_metrics_at_common_sizes() {
    // (width, height, process list shown); narrow boxes cut their titles, the names stay
    let sizes = [
      (200, 50, true),
      (120, 40, true),
      (110, 32, true),
      (80, 24, true),
      (60, 15, true),
      (60, 12, false),
    ];
    use Metric::*;
    let names = [
      (Cluster(0), "E-CPU "),
      (Cluster(1), "P-CPU "),
      (Gpu, "GPU "),
      (Ram, "RAM "),
      (CpuPower, "CPU "),
      (GpuPower, "GPU "),
      (AnePower, "ANE "),
    ];
    for (width, height, proc) in sizes {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, height);
      let screen = screen_text(&buf);
      let ctx = format!("{width}x{height}");

      // every box, each with its own name
      let boxes: Vec<Metric> = app.layout(buf.area).boxes.iter().map(|(m, _)| *m).collect();
      assert_eq!(boxes, names.map(|(metric, _)| metric), "{ctx}");
      for (metric, name) in names {
        let top = box_top(&app, &buf, metric);
        assert!(top.starts_with(&format!("╭─ {name}")), "{metric:?} ({ctx}): {top}");
      }

      for label in ["╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ", "q quit"] {
        assert!(screen.contains(label), "missing {label:?} ({ctx})");
      }
      assert_eq!(screen.contains(" proc "), proc, "{ctx}");
      // the hints come first: next to them at 60 columns there is no room for the power summary
      assert_eq!(screen.contains("─ Power 6.60W"), proc, "{ctx}");
    }
  }

  #[test]
  fn first_frame_has_every_cluster_box() {
    // before the first metrics sample: the cluster boxes and core counts come from the chip
    let mut app = App::from_parts(test_soc(), Config::default());
    let buf = render_buffer(&mut app, 200, 50);
    let plan = app.layout(buf.area);
    assert!(row(&buf, 0).starts_with("╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ─"), "{}", row(&buf, 0));
    assert!(box_top(&app, &buf, Metric::Cluster(0)).starts_with("╭─ E-CPU 0% @ 0 MHz ─"));
    assert!(box_top(&app, &buf, Metric::Cluster(1)).starts_with("╭─ P-CPU 0% @ 0 MHz ─"));

    // the first sample fills them in without moving a box
    app.update_metrics(test_metrics());
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(app.layout(buf.area), plan);
    assert!(box_top(&app, &buf, Metric::Cluster(0)).starts_with("╭─ E-CPU 42% @ 1800 MHz ─"));
  }

  #[test]
  fn metrics_title_has_chip_and_version() {
    let mut app = test_app();
    let chip = "╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ─";
    let version = format!("─ macmon v{} ─╮", env!("CARGO_PKG_VERSION"));
    for width in [200, 100, 60] {
      let top = row(&render_buffer(&mut app, width, 30), 0);
      assert!(top.starts_with(chip) && top.ends_with(&version), "{top}");
    }

    // the chip name plain, the details in parentheses dim
    let buf = render_buffer(&mut app, 100, 30);
    assert_eq!((buf[(3, 0)].symbol(), buf[(3, 0)].fg), ("A", theme::TEXT));
    assert_eq!((buf[(16, 0)].symbol(), buf[(16, 0)].fg), ("(", theme::DIM));

    // narrower: the version is dropped
    let top = row(&render_buffer(&mut app, 40, 30), 0);
    assert_eq!(top, format!("╭─ Apple M3 Pro (6E+6P+18GPU 36GB) {}╮", "─".repeat(4)));

    // narrowest: the chip is cut, nothing else fits
    let top = row(&render_buffer(&mut app, 24, 15), 0);
    assert_eq!(top, "╭─ Apple M3 Pro (6E+6P─╮");
  }

  #[test]
  fn matches_mockup_layout_at_110_columns() {
    // M3 Pro, enough history to fill the graphs
    let mut app = app_with_samples(60, |_| {});
    render_buffer(&mut app, 110, 32);
    put_procs(&mut app, varied_procs());
    let buf = render_buffer(&mut app, 110, 32);
    let rows: Vec<String> = (0..32).map(|y| row(&buf, y)).collect();

    let chip = "╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ";
    let version = format!(" macmon v{} ─╮", env!("CARGO_PKG_VERSION"));
    let dashes = 110 - chip.chars().count() - version.chars().count();
    assert_eq!(rows[0], format!("{chip}{}{version}", "─".repeat(dashes)));

    // the metrics box is 13 rows (40 %), 11 inside its borders: 6 for the top row of boxes, 5
    // for the bottom one. Top row: four boxes of 27 cells, the RAM title in its percent step
    let tops =
      ["E-CPU 42% @ 1800 MHz", "P-CPU 77% @ 3200 MHz", "GPU 23% @ 1400 MHz", "RAM 56% · SWAP 50%"];
    let tops: String = tops.iter().map(|title| top_border(27, title, "")).collect();
    assert_eq!(rows[1], format!("│{tops}│"));

    // 4 graph rows, top to bottom; the samples are constant, so every column of a graph is the
    // same: E-CPU 42% is 14 eighths of 32, P-CPU 77% 25, GPU 23% 8, RAM 20 of 36 GB 18
    let columns =
      [[' ', ' ', '▆', '█'], ['▁', '█', '█', '█'], [' ', ' ', ' ', '█'], [' ', '▂', '█', '█']];
    for (y, row) in rows[2..6].iter().enumerate() {
      let boxes: String =
        columns.iter().map(|c| format!("│{}│", c[y].to_string().repeat(25))).collect();
      assert_eq!(*row, format!("│{boxes}│"), "graph row {y}");
    }
    assert_eq!(rows[6], format!("│{}│", format!("╰{}╯", "─".repeat(25)).repeat(4)));

    // bottom row: three power boxes of 36 cells with the whole-degree temperatures; the graphs
    // scaled to their largest sample, so constant power is full height
    let titles = [
      ("CPU 4.50W (4.50, 4.50)", "45°C"),
      ("GPU 2.00W (2.00, 2.00)", "40°C"),
      ("ANE 0.10W (0.10, 0.10)", ""),
    ];
    let tops: String = titles.iter().map(|(left, right)| top_border(36, left, right)).collect();
    assert_eq!(rows[7], format!("│{tops}│"));
    for row in &rows[8..11] {
      assert_eq!(*row, format!("│{}│", format!("│{}│", "█".repeat(34)).repeat(3)));
    }
    assert_eq!(rows[11], format!("│{}│", format!("╰{}╯", "─".repeat(34)).repeat(3)));

    // the power summary on the bottom border of the metrics box
    assert_eq!(rows[12], border(110, SUMMARY, ""));

    // the process list in the rest of the screen: count, filter label and sort hint on its top
    // border, the sort arrow by the sorted column, the POWER note and the global hints on its
    // bottom border
    assert_eq!(rows[13], format!("╭─ proc 3 ─ / filter ─ s sort {}╮", "─".repeat(79)));
    let header = "│   PID NAME";
    let header_end = "USER       CPU% ↓    MEM   POWER   GPU% │";
    assert!(rows[14].starts_with(header) && rows[14].ends_with(header_end), "{}", rows[14]);
    assert!(rows[15].starts_with("│   631 WindowServer "), "{}", rows[15]);
    assert_eq!(rows[31], border(110, NOTE, &hints(6)));
  }

  /// App whose power samples differ, so the current value (the mean of the last two samples),
  /// average and maximum differ too: CPU 5 / 4 / 6 W, GPU 2.5 / 2 / 3, ANE 0.25 / 0.2 / 0.3,
  /// Power 9.5 / 8 / 12, Total 16 / 14 / 20.
  fn varied_power_app() -> App {
    let mut app = App::from_parts(test_soc(), Config::default());
    let samples =
      [(2.0, 1.0, 0.1, 10.0, 5.0), (4.0, 2.0, 0.2, 12.0, 7.0), (6.0, 3.0, 0.3, 20.0, 12.0)];
    for (cpu_power, gpu_power, ane_power, sys_power, all_power) in samples {
      let metrics =
        Metrics { cpu_power, gpu_power, ane_power, sys_power, all_power, ..test_metrics() };
      app.update_metrics(metrics);
    }
    app
  }

  #[test]
  fn power_boxes_show_current_avg_max_and_temperature() {
    let mut app = varied_power_app();
    let buf = render_buffer(&mut app, 200, 50);

    // 66 cells: current, average and maximum left, the temperature right
    let cases = [
      (Metric::CpuPower, "CPU 5.00W (4.00, 6.00)", " 45°C "),
      (Metric::GpuPower, "GPU 2.50W (2.00, 3.00)", " 40°C "),
      (Metric::AnePower, "ANE 0.25W (0.20, 0.30)", "─"),
    ];
    for (metric, title, end) in cases {
      let top = box_top(&app, &buf, metric);
      assert_eq!(top.chars().count(), 66, "{top}");
      let (start, end) = (format!("╭─ {title} ─"), format!("{end}─╮"));
      assert!(top.starts_with(&start) && top.ends_with(&end), "{top}");
    }

    // name bold, the current power plain, average and maximum dim; the temperature on the gradient
    let cpu = app.layout(buf.area).boxes[4].1;
    let cell = |dx: u16| &buf[(cpu.x + dx, cpu.y)];
    assert_eq!(cell(3).symbol(), "C");
    assert!(cell(3).modifier.contains(Modifier::BOLD));
    assert_eq!((cell(7).symbol(), cell(7).fg), ("5", theme::TEXT));
    assert!(!cell(7).modifier.contains(Modifier::BOLD));
    assert_eq!((cell(13).symbol(), cell(13).fg), ("(", theme::DIM));
    assert_eq!((cell(14).symbol(), cell(14).fg), ("4", theme::DIM));
    let top = box_top(&app, &buf, Metric::CpuPower);
    assert_eq!(cell(x_of(&top, "45°C")).fg, gradient((45.0 - 30.0) / 70.0));
    let gpu = box_top(&app, &buf, Metric::GpuPower);
    let gpu_x = app.layout(buf.area).boxes[5].1.x + x_of(&gpu, "40°C");
    assert_eq!(buf[(gpu_x, cpu.y)].fg, gradient((40.0 - 30.0) / 70.0));

    // the summary: Power (avg / max), the fan, Total (avg / max)
    let summary = " Power 9.50W (8.00, 12.00) | Fan 1200 RPM | Total 16.00W (14.00, 20.00) ";
    let bottom = row(&buf, PROC_Y - 1);
    assert_eq!(bottom, border(200, summary, ""));
    let fg = |text: &str| buf[(x_of(&bottom, text), PROC_Y - 1)].fg;
    assert_eq!(fg("Power"), theme::TEXT);
    assert_eq!(fg("(8.00"), theme::DIM);
    assert_eq!(fg("| Fan"), theme::DIM);
    assert_eq!(fg("Fan"), theme::TEXT);
    assert_eq!(fg("Total"), theme::TEXT);
    assert_eq!(fg("(14.00"), theme::DIM);
  }

  const GIB: f64 = (1u64 << 30) as f64;

  /// App with 16.81 of 24 GB RAM in use (70.0 %) and 2.37 of 3 GB swap (79.0 %), or no swap.
  fn ram_app(swap: bool) -> App {
    test_app_with(|m| {
      m.memory = MemMetrics {
        ram_total: 24 << 30,
        ram_usage: (16.81 * GIB) as u64,
        swap_total: if swap { 3 << 30 } else { 0 },
        swap_usage: if swap { (2.37 * GIB) as u64 } else { 0 },
      }
    })
  }

  /// Top border of the box of `metric` when the boxes of its row are `width` cells each: the
  /// screen is as many boxes + 2 borders wide (the clusters, GPU and RAM on top, three power
  /// boxes below).
  fn sized_box_top(app: &mut App, metric: Metric, width: u16) -> String {
    let power = matches!(metric, Metric::CpuPower | Metric::GpuPower | Metric::AnePower);
    let boxes = if power { 3 } else { app.clusters.items.len() as u16 + 2 };
    let buf = render_buffer(app, width * boxes + 2, 50);
    let top = box_top(app, &buf, metric);
    assert_eq!(top.chars().count(), usize::from(width), "{metric:?} at {width}: {top}");
    top
  }

  /// RAM box titles with swap, longest first; none at the end.
  const RAM_SWAP_STEPS: [&str; 4] =
    ["RAM 16.81 GB (70.0%) · SWAP 2.37 / 3.0 GB", "RAM 70% · SWAP 79%", "RAM 70%", ""];

  /// RAM box titles without swap, longest first; none at the end.
  const RAM_STEPS: [&str; 3] = ["RAM 16.81 GB (70.0%)", "RAM 70%", ""];

  #[test]
  fn ram_title_steps_down_with_swap() {
    let mut app = ram_app(true);
    // (box width, title): each from the width where it fits whole (its text + 6 cells)
    let cases = [
      (60, RAM_SWAP_STEPS[0]),
      (47, RAM_SWAP_STEPS[0]),
      (46, RAM_SWAP_STEPS[1]),
      (24, RAM_SWAP_STEPS[1]),
      (23, RAM_SWAP_STEPS[2]),
      (13, RAM_SWAP_STEPS[2]),
    ];
    for (width, title) in cases {
      let top = sized_box_top(&mut app, Metric::Ram, width);
      assert_eq!(top, top_border(width.into(), title, ""), "{width}");
    }

    // no number is ever cut: the swap part drops whole, then the title
    for width in [12, 8] {
      let top = sized_box_top(&mut app, Metric::Ram, width);
      assert_eq!(top, format!("╭{}╮", "─".repeat(usize::from(width) - 2)));
    }
  }

  #[test]
  fn ram_title_steps_down_without_swap() {
    let mut app = ram_app(false);
    let cases = [(60, RAM_STEPS[0]), (26, RAM_STEPS[0]), (25, RAM_STEPS[1]), (13, RAM_STEPS[1])];
    for (width, title) in cases {
      let top = sized_box_top(&mut app, Metric::Ram, width);
      assert_eq!(top, top_border(width.into(), title, ""), "{width}");
    }
    assert_eq!(sized_box_top(&mut app, Metric::Ram, 12), format!("╭{}╮", "─".repeat(10)));

    let screen = render_to_string(&mut app, 240, 60);
    assert!(!screen.contains("SWAP") && !screen.contains("SW "));
  }

  #[test]
  fn ram_title_styles() {
    let mut app = ram_app(true);
    let buf = render_buffer(&mut app, 60 * 4 + 2, 50);
    let ram = app.layout(buf.area).boxes[3].1;
    let top = box_top(&app, &buf, Metric::Ram);
    let cell = |text: &str| &buf[(ram.x + x_of(&top, text), ram.y)];

    // names bold, the separator dim, the RAM percent on the gradient
    assert!(cell("RAM").modifier.contains(Modifier::BOLD));
    assert!(cell("SWAP").modifier.contains(Modifier::BOLD));
    assert_eq!(cell("·").fg, theme::DIM);
    assert_eq!(cell("70.0%").fg, gradient(16.81 / 24.0));
    assert_eq!(cell("16.81").fg, theme::TEXT);

    // the short title: both percents on the gradient
    let buf = render_buffer(&mut app, 30 * 4 + 2, 50);
    let ram = app.layout(buf.area).boxes[3].1;
    let top = box_top(&app, &buf, Metric::Ram);
    assert_eq!(top, top_border(30, RAM_SWAP_STEPS[1], ""));
    let fg = |text: &str| buf[(ram.x + x_of(&top, text), ram.y)].fg;
    assert_eq!(fg("70%"), gradient(16.81 / 24.0));
    assert_eq!(fg("79%"), gradient(2.37 / 3.0));
  }

  #[test]
  fn titles_step_down_before_they_are_cut() {
    // every box width: the longest step that fits whole, the last one cut only when none does
    let clusters = [
      (Metric::Cluster(0), ["E-CPU 42% @ 1800 MHz", "E-CPU 42%"]),
      (Metric::Cluster(1), ["P-CPU 77% @ 3200 MHz", "P-CPU 77%"]),
      (Metric::Gpu, ["GPU 23% @ 1400 MHz", "GPU 23%"]),
    ];
    let mut app = ram_app(true);
    let mut dry = ram_app(false);
    for width in 6..=60 {
      for (metric, steps) in &clusters {
        let steps = steps.map(|title| (title, ""));
        assert_eq!(sized_box_top(&mut app, *metric, width), fitted_top(width.into(), &steps));
      }
      let steps = RAM_SWAP_STEPS.map(|title| (title, ""));
      assert_eq!(sized_box_top(&mut app, Metric::Ram, width), fitted_top(width.into(), &steps));
      let steps = RAM_STEPS.map(|title| (title, ""));
      assert_eq!(sized_box_top(&mut dry, Metric::Ram, width), fitted_top(width.into(), &steps));
    }

    // power: average and maximum go first, then the temperature; the current power stays
    let mut app = test_app();
    let cpu = [("CPU 4.50W (4.50, 4.50)", "45°C"), ("CPU 4.50W", "45°C")];
    let ane = [("ANE 0.10W (0.10, 0.10)", ""), ("ANE 0.10W", "")];
    for width in 6..=60 {
      let fitted = |steps: &[(&str, &str)]| fitted_top(width.into(), steps);
      assert_eq!(sized_box_top(&mut app, Metric::CpuPower, width), fitted(&cpu));
      assert_eq!(sized_box_top(&mut app, Metric::AnePower, width), fitted(&ane));
    }
  }

  #[test]
  fn power_and_cluster_titles_at_their_step_widths() {
    let mut app = test_app();
    let dashes = |n: usize| "─".repeat(n);
    let cpu = |app: &mut App, width| sized_box_top(app, Metric::CpuPower, width);
    assert_eq!(cpu(&mut app, 35), "╭─ CPU 4.50W (4.50, 4.50) ─ 45°C ─╮");
    // average and maximum go before the temperature, which stays at 80 columns (26-cell boxes)
    assert_eq!(cpu(&mut app, 34), format!("╭─ CPU 4.50W {} 45°C ─╮", dashes(13)));
    assert_eq!(cpu(&mut app, 27), format!("╭─ CPU 4.50W {} 45°C ─╮", dashes(6)));
    assert_eq!(cpu(&mut app, 22), "╭─ CPU 4.50W ─ 45°C ─╮");
    assert_eq!(cpu(&mut app, 21), format!("╭─ CPU 4.50W {}╮", dashes(7)));
    assert_eq!(cpu(&mut app, 15), "╭─ CPU 4.50W ─╮");
    assert_eq!(cpu(&mut app, 14), "╭─ CPU 4.50W─╮");

    let ecpu = |app: &mut App, width| sized_box_top(app, Metric::Cluster(0), width);
    assert_eq!(ecpu(&mut app, 26), "╭─ E-CPU 42% @ 1800 MHz ─╮");
    assert_eq!(ecpu(&mut app, 25), format!("╭─ E-CPU 42% {}╮", dashes(11)));
    assert_eq!(ecpu(&mut app, 15), "╭─ E-CPU 42% ─╮");
    assert_eq!(ecpu(&mut app, 13), "╭─ E-CPU 42─╮");
  }

  #[test]
  fn power_summary_follows_sensors() {
    let power = " Power 6.60W (6.60, 6.60)";
    let total = "Total 12.00W (12.00, 12.00)";
    let summary = |edit: fn(&mut Metrics)| {
      let mut app = test_app_with(edit);
      row(&render_buffer(&mut app, 200, 50), PROC_Y - 1)
    };

    assert_eq!(summary(|_| {}), border(200, SUMMARY, ""));
    // two fans
    let fans = |m: &mut Metrics| {
      m.fans =
        (0..2).map(|i| FanMetric { name: format!("fan{i}"), rpm: 2000, max_rpm: None }).collect()
    };
    let left = format!("{power} | Fans 2000/2000 RPM | {total} ");
    assert_eq!(summary(fans), border(200, &left, ""));
    // only parts whose sensors exist, no gap left behind
    assert_eq!(summary(|m| m.fans.clear()), border(200, &format!("{power} | {total} "), ""));
    let left = format!("{power} | Fan 1200 RPM ");
    assert_eq!(summary(|m| m.sys_power = 0.0), border(200, &left, ""));
    let neither = |m: &mut Metrics| {
      m.fans.clear();
      m.sys_power = 0.0;
    };
    assert_eq!(summary(neither), border(200, &format!("{power} "), ""));
  }

  #[test]
  fn key_hints_right_aligned_on_bottom_border() {
    let mut app = app_with_procs(test_procs());
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), hints_border(200, 6));
    let hints = "─ q quit | ? help | p procs | v graph | r scaled | -/+ 1000ms ─╯";
    assert!(row(&buf, 49).ends_with(hints));
    assert_eq!(row(&buf, PROC_Y - 1), border(200, SUMMARY, ""), "no hints on the metrics box");
    let screen = screen_text(&buf);
    assert_eq!(screen.matches("q quit").count(), 1);
    // the process list controls live on its top border, not in the footer
    assert!(screen.matches("s sort").count() == 1 && screen.matches("/ filter").count() == 1);
    assert!(proc_row(&buf, 0).contains(" / filter ─ s sort "));

    // keys bold, labels plain, separators dim
    let bottom = row(&buf, 49);
    let x = |text: &str| x_of(&bottom, text);
    let cell = |x: u16| &buf[(x, 49)];
    assert!(cell(x("q quit")).modifier.contains(Modifier::BOLD));
    assert!(cell(x("? help")).modifier.contains(Modifier::BOLD));
    assert!(cell(x("p procs")).modifier.contains(Modifier::BOLD));
    assert!(cell(x("v graph")).modifier.contains(Modifier::BOLD));
    assert!(!cell(x("quit")).modifier.contains(Modifier::BOLD));
    assert_eq!(cell(x("quit")).fg, theme::TEXT);
    assert_eq!(cell(x("| p")).fg, theme::DIM);

    // the state of the toggles: chart view, ratio mode and interval
    assert!(app.handle_key(key('r')).is_continue());
    assert!(app.handle_key(key('+')).is_continue());
    assert!(app.handle_key(key('v')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    let hints = "─ q quit | ? help | p procs | v gauge | r active | -/+ 1250ms ─╯";
    assert!(bottom.ends_with(hints), "{bottom}");

    // without the process list: the same hints on the metrics box, after the power summary
    app.cfg.show_procs = false;
    assert!(app.handle_key(key('-')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    let hints = " q quit | ? help | p procs | v gauge | r active | -/+ 1000ms ";
    assert_eq!(bottom, border(200, SUMMARY, hints));
  }

  #[test]
  fn key_hints_drop_from_the_end_when_narrow() {
    // (width, hints shown): `q quit` 6 cells, then 3 cells between hints and 1 at both ends,
    // plus `╰─` and `─╯`
    let cases = [
      (200, 6),
      (65, 6),
      (64, 5),
      (52, 5),
      (51, 4),
      (41, 4),
      (40, 3),
      (31, 3),
      (30, 2),
      (21, 2),
      (20, 1),
      (12, 1),
    ];
    for (width, count) in cases.into_iter().chain([(11, 0), (5, 0)]) {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, 60);
      let proc = app.layout(buf.area).proc.expect("process box");
      assert_eq!(row(&buf, proc.bottom() - 1), hints_border(width.into(), count), "{width}");
    }
  }

  #[test]
  fn footer_and_power_summary_share_the_border() {
    let text = SUMMARY.trim();
    // the summary cut to `room` cells, with its blank cells
    let cut = |room: usize| format!(" {}… ", &text[..room - 1]);
    // (width, summary shown, hints shown): the hints first, the summary in the room they leave
    // (cut, or left out with fewer than 8 cells), then the hints drop from the end
    let cases = [
      (200, SUMMARY.to_string(), 6),
      (137, SUMMARY.to_string(), 6),
      (136, cut(68), 6),
      (80, cut(12), 6),
      (76, cut(8), 6),
      (75, String::new(), 6),
      (65, String::new(), 6),
      (64, cut(9), 5),
      (53, String::new(), 5),
      (20, String::new(), 1),
      (12, String::new(), 1),
      (11, String::new(), 0),
    ];
    for (width, left, count) in cases {
      let mut app = test_app();
      app.cfg.show_procs = false;
      let buf = render_buffer(&mut app, width, 30);
      assert_eq!(row(&buf, 29), border(width.into(), &left, &hints(count)), "{width}");
    }

    // with the process list: the summary alone on the metrics box, the hints on the process box
    let mut app = test_app();
    let buf = render_buffer(&mut app, 80, 24);
    assert_eq!(row(&buf, 9), border(80, SUMMARY, ""));
    assert_eq!(row(&buf, 23), hints_border(80, 6));
    let buf = render_buffer(&mut app, 60, 24);
    assert_eq!(row(&buf, 9), border(60, &cut(54), ""));
  }

  /// App with synthetic CPU clusters of `(label, cores, load)`.
  fn clusters_app(clusters: &[(&str, usize, f32)]) -> App {
    let samples: Vec<FreqSample> =
      clusters.iter().map(|&(_, _, ratio)| FreqSample::new(2000, ratio, ratio)).collect();

    let mut app = test_app();
    app.clusters = CpuClusters::new(clusters.iter().map(|&(label, count, _)| (label, count)));
    for _ in 0..3 {
      app.clusters.push(&samples);
    }
    app
  }

  #[test]
  fn three_clusters_get_boxes_and_title() {
    let mut app = clusters_app(&[("E", 6, 0.2), ("P", 4, 0.4), ("S", 2, 0.6)]);
    let buf = render_buffer(&mut app, 200, 50);
    let screen = screen_text(&buf);
    for title in ["E-CPU 20% @ 2000 MHz", "P-CPU 40% @ 2000 MHz", "S-CPU 60% @ 2000 MHz"] {
      assert!(screen.contains(&format!("╭─ {title} ─")), "missing {title}");
    }
    assert!(row(&buf, 0).starts_with("╭─ Apple M3 Pro (6E+4P+2S+18GPU 36GB) ─"));

    // five boxes on top in cluster order, then GPU and RAM; the power boxes below
    use Metric::*;
    let boxes = app.layout(buf.area).boxes;
    let kinds: Vec<Metric> = boxes.iter().map(|(metric, _)| *metric).collect();
    assert_eq!(kinds, [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram, CpuPower, GpuPower, AnePower]);
    assert!(boxes[..5].iter().all(|(_, r)| r.y == 1) && boxes[5..].iter().all(|(_, r)| r.y > 1));
  }

  #[test]
  fn keys_update_rendered_metrics() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("E-CPU 42%") && screen.contains("GPU 23%"));

    // r: active ratios
    assert!(app.handle_key(key('r')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E-CPU 50%") && screen.contains("P-CPU 80%"));
    assert!(screen.contains("GPU 30%") && screen.contains("r active"));

    // +/-: interval in the hints only
    assert!(app.handle_key(key('+')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("-/+ 1250ms") && screen.matches("1250ms").count() == 1);
    assert!(app.handle_key(key('-')).is_continue());
    assert!(app.handle_key(key('-')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("-/+ 750ms"));
  }

  #[test]
  fn metrics_take_40_percent_and_procs_the_rest() {
    // (width, height, metrics box height)
    for (width, height, top) in [(200, 50, 20), (110, 32, 13), (80, 24, 10)] {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, height);
      let plan = app.layout(buf.area);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(Rect::new(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(Rect::new(0, top, width, height - top)), "{ctx}");
      assert!(row(&buf, top).starts_with("╭─ proc"), "{ctx}");

      // every metric box drawn where the layout put it, inside the metrics box
      assert_eq!(plan.boxes.len(), 7, "{ctx}");
      for (metric, r) in &plan.boxes {
        let corners = [(r.x, r.y, "╭"), (r.right() - 1, r.bottom() - 1, "╯")];
        for (x, y, corner) in corners {
          assert_eq!(buf[(x, y)].symbol(), corner, "{ctx}: {metric:?}");
        }
        assert!(r.bottom() < top, "{ctx}: {metric:?}");
      }
    }
  }

  #[test]
  fn proc_panel_auto_hides_in_small_window() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 200, 50).contains(" proc "));
    assert!(render_to_string(&mut app, 80, 24).contains(" proc "), "width doesn't matter");
    // the metrics shrink to their two rows of boxes before the process list goes
    assert!(render_to_string(&mut app, 60, 15).contains(" proc "));
    assert!(!render_to_string(&mut app, 60, 12).contains(" proc "));
    assert!(!render_to_string(&mut app, 200, 13).contains(" proc "));
    assert!(render_to_string(&mut app, 200, 14).contains(" proc "));
    assert!(app.cfg.show_procs, "auto-hide must not change the config");

    // auto-hidden: the metrics take the whole screen
    let buf = render_buffer(&mut app, 200, 13);
    assert_eq!(app.layout(buf.area).top, Some(buf.area));
  }

  #[test]
  fn renders_any_size() {
    let sizes = [
      (400, 120),
      (200, 50),
      (120, 40),
      (110, 32),
      (100, 20),
      (80, 24),
      (60, 15),
      (60, 12),
      (30, 8),
      (5, 3),
      (1, 1),
    ];
    let three = [("E", 6, 0.2), ("P", 4, 0.6), ("S", 2, 0.9)];
    for (width, height) in sizes {
      for bits in 0..16u8 {
        let mut app = match bits & 3 {
          0 => test_app(),
          1 => clusters_app(&three),
          2 => test_app_with(|m| {
            m.memory.swap_total = 0;
            m.fans.clear();
            m.sys_power = 0.0;
          }),
          _ => App::default(),
        };
        app.cfg.show_procs = bits & 4 != 0;
        app.proc_view.set_procs(test_procs());
        if bits & 8 != 0 {
          app.proc_view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        }
        let buf = render_buffer(&mut app, width, height);

        // every box drawn where the layout puts it: no title, summary, hint, graph or table
        // cell lands on a corner
        let plan = app.layout(buf.area);
        let boxes = plan.top.into_iter().chain(plan.proc).chain(plan.boxes.iter().map(|b| b.1));
        for r in boxes.filter(|r| r.width >= 2 && r.height >= 2) {
          let corners = [
            (r.left(), r.top(), "╭"),
            (r.right() - 1, r.top(), "╮"),
            (r.left(), r.bottom() - 1, "╰"),
            (r.right() - 1, r.bottom() - 1, "╯"),
          ];
          for (x, y, corner) in corners {
            assert_eq!(buf[(x, y)].symbol(), corner, "{width}x{height} bits {bits}: {r:?}");
          }
        }
      }
    }
  }

  #[test]
  fn graphs_are_block_bars() {
    let mut app = test_app();
    for _ in 0..40 {
      app.update_metrics(test_metrics());
    }

    for three_levels in [false, true] {
      app.three_level_bars = three_levels;
      for (width, height) in [(200, 50), (120, 40), (60, 15)] {
        let buf = render_buffer(&mut app, width, height);
        let screen = screen_text(&buf);
        let ctx = format!("{width}x{height}, three levels {three_levels}");
        assert!(screen.chars().filter(|c| is_bar(*c)).count() > 40, "{ctx}");

        // every box has a graph standing on its bottom row; Apple Terminal gets only `▄` and `█`
        for (metric, r) in app.layout(buf.area).boxes {
          let graph = r.inner(Margin::new(1, 1));
          let bottom = text(&buf, Rect { y: graph.bottom() - 1, height: 1, ..graph });
          assert!(bottom.chars().any(is_bar), "{ctx}: {metric:?} {bottom:?}");
          if three_levels {
            let cells = (graph.top()..graph.bottom()).map(|y| text(&buf, Rect { y, ..graph }));
            let glyphs: String = cells.collect();
            assert!(glyphs.chars().all(|c| [' ', '▄', '█'].contains(&c)), "{ctx}: {glyphs}");
          }
        }
      }
    }
  }

  #[test]
  fn graph_columns_follow_their_own_load_power_graphs_stay_low() {
    // E-CPU load cycles through 10%, 50%, 90%, the newest is 90%
    let loads = [0.1, 0.5, 0.9];
    {
      let mut app = App::from_parts(test_soc(), Config::default());
      for i in 0..60 {
        app.update_metrics(Metrics { ecpu_scaled_ratio: loads[i % 3], ..test_metrics() });
      }

      // 100x30: E-CPU graph 3 rows (24 eighths) tall: 10% ▃, 50% █ under ▄, 90% █ █ ▆
      let buf = render_buffer(&mut app, 100, 30);
      let graph = graph_area(&app, buf.area, Metric::Cluster(0));
      assert_eq!(graph.height, 3);
      let rows: Vec<String> =
        (graph.top()..graph.bottom()).map(|y| text(&buf, Rect { y, height: 1, ..graph })).collect();
      assert!(rows[0].ends_with("  ▆  ▆"), "{rows:#?}");
      assert!(rows[1].ends_with(" ▄█ ▄█"), "{rows:#?}");
      assert!(rows[2].ends_with("▃██▃██"), "{rows:#?}");

      // every cell of a column in the color of its own load
      for x in graph.left()..graph.right() {
        let age = usize::from(graph.right() - 1 - x);
        let load = [0.1, 0.5, 0.9][(59 - age) % 3];
        for y in graph.top()..graph.bottom() {
          let cell = &buf[(x, y)];
          if cell.symbol() != " " {
            assert_eq!(cell.fg, gradient(load), "{x}, {y}: {rows:#?}");
          }
        }
      }
      // the terminal's own green / yellow / red
      let colors = (graph.right() - 3..graph.right()).map(|x| buf[(x, graph.bottom() - 1)].fg);
      assert_eq!(colors.collect::<Vec<_>>(), [Color::Green, Color::Yellow, Color::Red]);

      // power graphs in the low load color, whatever their height
      for metric in [Metric::CpuPower, Metric::GpuPower, Metric::AnePower] {
        let graph = graph_area(&app, buf.area, metric);
        let cells: Vec<_> = (graph.top()..graph.bottom())
          .flat_map(|y| (graph.left()..graph.right()).map(move |x| (x, y)))
          .map(|(x, y)| &buf[(x, y)])
          .filter(|cell| cell.symbol() != " ")
          .collect();
        assert!(!cells.is_empty(), "{metric:?}");
        for cell in cells {
          assert!(cell.symbol().chars().all(is_bar), "{:?}", cell.symbol());
          assert_eq!(cell.fg, gradient(0.0), "{metric:?}");
        }
      }
    }
  }

  #[test]
  fn graphs_fill_boxes_once_history_is_long_enough() {
    // per metric box: bars on the bottom row of its graph, and the graph width
    let bottoms = |app: &mut App, width: u16| -> Vec<(usize, usize)> {
      let buf = render_buffer(app, width, 50);
      let boxes = app.layout(buf.area).boxes;
      let bottom = |r: &Rect| {
        let graph = r.inner(Margin::new(1, 1));
        let text = text(&buf, Rect { y: graph.bottom() - 1, height: 1, ..graph });
        (text.chars().filter(|c| is_bar(*c)).count(), usize::from(graph.width))
      };
      boxes.iter().map(|(_, r)| bottom(r)).collect()
    };

    // 3 samples: one bar each, on the right
    let mut app = test_app();
    for (bars, _) in bottoms(&mut app, 200) {
      assert_eq!(bars, 3);
    }

    // a long history fills every graph of a wide terminal, newest sample on the right
    for _ in 0..700 {
      app.update_metrics(test_metrics());
    }
    assert_eq!(app.igpu_freq.ratio(RatioMode::Scaled).items.len(), 703);
    assert_eq!(app.mem.items.len(), 703);
    for width in [100, 200, 400] {
      let graphs = bottoms(&mut app, width);
      assert!(graphs.iter().all(|(bars, graph)| bars == graph), "width {width}: {graphs:?}");
    }
    // 128 cells was all the old 128 sample history could fill
    assert!(bottoms(&mut app, 400).iter().any(|(_, graph)| *graph > 128));
  }

  /// Eighths of a row filled by the bar in column `x` of the graph `area`.
  fn column_eighths(buf: &Buffer, area: Rect, x: u16) -> u64 {
    (area.top()..area.bottom())
      .filter_map(|y| buf[(x, y)].symbol().chars().next())
      .filter(|c| is_bar(*c))
      .map(|c| u64::from(c) - 0x2580)
      .sum()
  }

  #[test]
  fn load_graphs_scale_to_full_load_power_graphs_to_their_peak() {
    // RAM at 60 % of 24 GB, then at 70 %: the newest columns are the largest visible samples, and
    // still fill only 70 % of the height
    let ram = |usage: f64| MemMetrics {
      ram_total: 24 << 30,
      ram_usage: (usage * GIB) as u64,
      ..test_metrics().memory
    };
    let mut app = app_with_samples(55, |m| m.memory = ram(14.4));
    for _ in 0..5 {
      app.update_metrics(Metrics { memory: ram(16.8), ..test_metrics() });
    }

    // (width, height, process list): graphs 7, 22, 4, 2 and 1 rows tall
    let sizes =
      [(200, 50, true), (200, 50, false), (110, 32, true), (80, 24, true), (60, 15, true)];
    for (width, height, procs) in sizes {
      app.cfg.show_procs = procs;
      let buf = render_buffer(&mut app, width, height);
      let ctx = format!("{width}x{height} procs {procs}");

      // (metric, load, age of the column): RAM at 70 % and an older 60 % column, E-CPU 42 %
      // (stored in whole percent), GPU 23 %, all scaled to a full load
      let cases = [
        (Metric::Ram, 0.7, 0),
        (Metric::Ram, 0.6, 7),
        (Metric::Cluster(0), 0.42, 0),
        (Metric::Gpu, 0.23, 0),
      ];
      for (metric, load, age) in cases {
        let area = graph_area(&app, buf.area, metric);
        let levels = u64::from(area.height) * 8;
        let eighths = column_eighths(&buf, area, area.right() - 1 - age);
        // rounded up to the next eighth
        let off = eighths as f64 - load * levels as f64;
        let msg = format!("{ctx}: {metric:?} at {load}: {eighths} of {levels} eighths");
        assert!((-1e-9..=1.0).contains(&off) && eighths < levels, "{msg}");
      }

      // constant power fills its graph: power graphs scale to their largest visible sample
      for metric in [Metric::CpuPower, Metric::GpuPower, Metric::AnePower] {
        let area = graph_area(&app, buf.area, metric);
        let eighths = column_eighths(&buf, area, area.right() - 1);
        assert_eq!(eighths, u64::from(area.height) * 8, "{ctx}: {metric:?}");
      }
    }
  }

  #[test]
  fn v_switches_load_boxes_to_gauges() {
    let file = TempConfig::new("v_switches_load_boxes_to_gauges");
    let mut app = saving_app(&file);
    for _ in 3..60 {
      app.update_metrics(test_metrics());
    }
    let graphs = render_buffer(&mut app, 200, 50);

    assert!(app.handle_key(key('v')).is_continue());
    assert_eq!(app.cfg.view_type, ViewType::Gauge);
    assert_eq!(file.saved()["view_type"], "Gauge");

    // every row filled to the load in its load color: E-CPU 42 % of 47 cells, P-CPU 77 % of 48,
    // GPU 23 % of 47, RAM 20 of 36 GB of 48
    let buf = render_buffer(&mut app, 200, 50);
    let m = test_metrics();
    let cases = [
      (Metric::Cluster(0), f64::from(m.ecpu_scaled_ratio), 20),
      (Metric::Cluster(1), f64::from(m.pcpu_scaled_ratio), 37),
      (Metric::Gpu, f64::from(m.gpu_scaled_ratio), 11),
      (Metric::Ram, 20.0 / 36.0, 27),
    ];
    for (metric, load, filled) in cases {
      let area = graph_area(&app, buf.area, metric);
      let gauge = format!("{}{}", "█".repeat(filled), " ".repeat(usize::from(area.width) - filled));
      for y in area.top()..area.bottom() {
        assert_eq!(text(&buf, Rect { y, height: 1, ..area }), gauge, "{metric:?} row {y}");
        assert_eq!(buf[(area.x, y)].fg, gradient(load), "{metric:?} row {y}");
      }
    }
    // titles stay; the power boxes keep their graphs
    assert_eq!(row(&buf, 1), row(&graphs, 1));
    for (metric, r) in app.layout(buf.area).boxes {
      if matches!(metric, Metric::CpuPower | Metric::GpuPower | Metric::AnePower) {
        for y in r.top()..r.bottom() {
          let line = Rect { y, height: 1, ..r };
          assert_eq!(text(&buf, line), text(&graphs, line), "{metric:?} row {y}");
        }
      }
    }

    // other sizes: the filled share of the width follows the load
    for (width, height) in [(110, 32), (80, 24), (60, 15), (400, 120)] {
      let buf = render_buffer(&mut app, width, height);
      for (metric, load, _) in cases {
        let area = graph_area(&app, buf.area, metric);
        let filled = (f64::from(area.width) * load).round() as usize;
        let gauge =
          format!("{}{}", "█".repeat(filled), " ".repeat(usize::from(area.width) - filled));
        for y in area.top()..area.bottom() {
          assert_eq!(
            text(&buf, Rect { y, height: 1, ..area }),
            gauge,
            "{width}x{height} {metric:?}"
          );
        }
      }
    }

    // `v` again: back to the graphs
    assert!(app.handle_key(key('v')).is_continue());
    assert_eq!(app.cfg.view_type, ViewType::Graph);
    assert_eq!(file.saved()["view_type"], "Sparkline");
    assert_eq!(render_buffer(&mut app, 200, 50), graphs);
  }

  #[test]
  fn renders_without_metrics() {
    let mut app = App::default();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("Power 0.00W") && screen.contains("RAM 0.00 GB (0.0%)"));
    assert!(screen.contains("╭─ macmon ─"), "title without chip info");
    assert!(!screen.contains("°C") && !screen.contains("Total") && !screen.contains("Fan"));
  }

  /// Processes that all have a power reading, without readable paths.
  fn test_procs() -> Vec<ProcInfo> {
    let proc = |pid: i32, name: &str| ProcInfo {
      pid,
      name: name.to_string(),
      path: String::new(),
      user: "root".to_string(),
      cpu_pct: 12.5,
      mem_bytes: 64 << 20,
      power_w: Some(0.5),
      gpu_pct: 3.0,
    };
    vec![proc(1, "launchd"), proc(631, "WindowServer"), proc(2301, "Safari")]
  }

  fn procs_active(app: &App) -> bool {
    app.procs_visible()
  }

  /// Hands `procs` to `app` as a sample of the panel's latest showing: the one on screen, or the
  /// last one while the panel is hidden (a sample still in flight).
  fn put_procs(app: &mut App, procs: Vec<ProcInfo>) {
    let showing = app.procs_shown.state.lock().unwrap().count;
    app.update_procs(showing, procs);
  }

  #[test]
  fn proc_sampling_follows_panel_visibility() {
    let mut app = test_app();
    assert!(!procs_active(&app), "no sampling before the first frame");

    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));

    // auto-hidden in a small window, back when it grows
    render_buffer(&mut app, 60, 12);
    assert!(!procs_active(&app));
    assert!(app.cfg.show_procs);
    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));

    // `p` hides the list; the flag follows on the next frame
    assert!(app.handle_key(key('p')).is_continue());
    assert!(procs_active(&app));
    render_buffer(&mut app, 200, 50);
    assert!(!procs_active(&app));
    assert!(app.handle_key(key('p')).is_continue());
    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));
  }

  #[test]
  fn proc_panel_collects_until_first_sample() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc ") && screen.contains("collecting…"));

    put_procs(&mut app, test_procs());
    assert_eq!(app.proc_view.procs(), Some(test_procs().as_slice()));
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc 3 ") && screen.contains("WindowServer"));
    assert!(!screen.contains("collecting"));

    put_procs(&mut app, vec![]);
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc 0 ") && !screen.contains("WindowServer"));
  }

  #[test]
  fn hidden_proc_panel_drops_samples() {
    let mut app = test_app();
    // samples arriving before the first frame are dropped
    put_procs(&mut app, test_procs());
    assert_eq!(app.proc_view.procs(), None);

    render_buffer(&mut app, 200, 50);
    let first = app.procs_shown.showing().unwrap();
    app.update_procs(first, test_procs());
    assert!(app.proc_view.procs().is_some());

    // hiding drops the list and a sample still in flight
    render_buffer(&mut app, 60, 12);
    assert_eq!(app.proc_view.procs(), None);
    app.update_procs(first, test_procs());
    assert_eq!(app.proc_view.procs(), None);

    // shown again: collecting until the next sample instead of stale rows, also when a sample
    // started before the hide arrives only now
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    app.update_procs(first, test_procs());
    assert_eq!(app.proc_view.procs(), None);
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    let second = app.procs_shown.showing().unwrap();
    assert_ne!(second, first);
    app.update_procs(second, test_procs());
    assert!(render_to_string(&mut app, 200, 50).contains(" proc 3 "));
  }

  /// `app` with the process panel on screen at 200x50 and `procs` in it.
  fn with_procs(mut app: App, procs: Vec<ProcInfo>) -> App {
    render_buffer(&mut app, 200, 50);
    put_procs(&mut app, procs);
    app
  }

  /// `test_app` with the process panel on screen at 200x50 and `procs` in it.
  fn app_with_procs(procs: Vec<ProcInfo>) -> App {
    with_procs(test_app(), procs)
  }

  /// Screen row where the process box starts in a 200x50 window: right under the metrics box, 40 %
  /// of the height.
  const PROC_Y: u16 = 20;

  /// Text of row `y` of the process box in a 200x50 window: 0 is the title, 1 the header.
  fn proc_row(buf: &Buffer, y: u16) -> String {
    row(buf, PROC_Y + y)
  }

  /// Path of the varied processes.
  const WINDOW_SERVER: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/Resources/WindowServer";

  /// Processes with different values, one of them (root's launchd) without a power reading.
  fn varied_procs() -> Vec<ProcInfo> {
    let path = |name: &str| match name {
      "launchd" => "/sbin/launchd".to_string(),
      "WindowServer" => WINDOW_SERVER.to_string(),
      _ => format!("/Applications/{name}.app/Contents/MacOS/{name}"),
    };
    let proc = |pid: i32, name: &str, cpu, mem_mb: u64, power_w, gpu_pct| ProcInfo {
      pid,
      name: name.to_string(),
      path: path(name),
      user: if pid < 100 { "root" } else { "vlad" }.to_string(),
      cpu_pct: cpu,
      mem_bytes: mem_mb << 20,
      power_w,
      gpu_pct,
    };
    vec![
      proc(1, "launchd", 0.0, 20, None, 0.0),
      proc(631, "WindowServer", 25.0, 300, Some(1.5), 40.0),
      proc(2301, "Safari", 12.0, 1536, Some(0.8), 5.0),
    ]
  }

  #[test]
  fn proc_table_renders_rows() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);

    // title: count and the filter label, nothing on the right
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 3 ─ / filter ─"), "{title}");
    assert!(title.ends_with("───╮"), "{title}");

    // the sort arrow next to the sorted column's header, right-aligned like its numbers
    let header = proc_row(&buf, 1);
    let words: Vec<&str> = header.split_whitespace().collect();
    assert_eq!(words, ["│", "PID", "NAME", "USER", "CPU%", "↓", "MEM", "POWER", "GPU%", "│"]);
    assert!(header.ends_with(" CPU% ↓    MEM   POWER   GPU% │"), "{header}");

    // sorted by CPU, descending; numbers right-aligned
    let rows: Vec<String> = (2..5).map(|y| proc_row(&buf, y)).collect();
    let row_words = |row: &str| row.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
      row_words(&rows[0]),
      ["│", "631", "WindowServer", "vlad", "25.0", "300M", "1.50W", "40.0", "│"]
    );
    assert_eq!(
      row_words(&rows[1]),
      ["│", "2301", "Safari", "vlad", "12.0", "1.5G", "0.80W", "5.0", "│"]
    );
    assert_eq!(row_words(&rows[2]), ["│", "1", "launchd", "root", "0.0", "20M", "-", "0.0", "│"]);
    // one blank cell after the left border and before the right one
    assert!(header.starts_with("│   PID NAME "), "{header}");
    assert!(rows[0].starts_with("│   631 WindowServer"), "{}", rows[0]);
    assert!(rows[0].ends_with("  25.0   300M   1.50W   40.0 │"), "{}", rows[0]);

    // gradient colors for load values, dim zeros and missing power
    let cell = |y: u16, row: &str, text: &str| buf[(x_of(row, text), PROC_Y + y)].fg;
    assert_eq!(cell(2, &rows[0], "25.0"), gradient(0.25));
    assert_eq!(cell(2, &rows[0], "40.0"), gradient(0.4));
    // MEM by its share of the RAM (36 GB), POWER against 10 W
    assert_eq!(cell(2, &rows[0], "300M"), gradient(300.0 / (36.0 * 1024.0)));
    assert_eq!(cell(3, &rows[1], "1.5G"), gradient(1.5 / 36.0));
    assert_eq!(cell(2, &rows[0], "1.50W"), gradient(0.15));
    assert_eq!(cell(3, &rows[1], "0.80W"), gradient(0.08));
    assert_eq!(cell(4, &rows[2], "-"), theme::DIM);
    assert_eq!(cell(4, &rows[2], "0.0"), theme::DIM);
    assert_eq!(cell(4, &rows[2], "launchd"), theme::TEXT);
    // the sorted column header stands out
    assert_eq!(cell(1, &header, "CPU%"), theme::TEXT);
    assert_eq!(cell(1, &header, "MEM"), theme::DIM);
  }

  #[test]
  fn zero_power_is_a_dim_number_missing_power_a_dash() {
    let mut procs = varied_procs();
    procs[0].power_w = Some(0.0); // launchd, last by CPU
    let mut app = app_with_procs(procs);
    let buf = render_buffer(&mut app, 200, 50);
    let line = proc_row(&buf, 4);
    assert!(line.ends_with("   0.0    20M   0.00W    0.0 │"), "{line}");
    assert_eq!(buf[(x_of(&line, "0.00W"), PROC_Y + 4)].fg, theme::DIM);

    put_procs(&mut app, varied_procs());
    let line = proc_row(&render_buffer(&mut app, 200, 50), 4);
    assert!(line.ends_with("   0.0    20M       -    0.0 │"), "{line}");
  }

  #[test]
  fn narrow_proc_panel_drops_columns() {
    let mut app = test_app();
    render_buffer(&mut app, 40, 20);
    put_procs(&mut app, varied_procs());

    // the process box under the smallest metrics box, 8 rows
    let buf = render_buffer(&mut app, 40, 20);
    assert_eq!(app.layout(buf.area).proc, Some(Rect::new(0, 8, 40, 12)));
    let header = row(&buf, 9);
    for (label, shown) in [
      ("PID", true),
      ("NAME", true),
      ("CPU%", true),
      ("MEM", true),
      ("GPU%", true),
      ("POWER", false),
      ("USER", false),
    ] {
      assert_eq!(header.contains(label), shown, "{label} in {header}");
    }
    // NAME gets the 9 cells left: cut, with `…`
    assert!(row(&buf, 10).starts_with("│   631 WindowSe…   25.0"), "{}", row(&buf, 10));

    // very narrow: PID, NAME and the sorted column, nothing drawn over the border
    let buf = render_buffer(&mut app, 18, 20);
    assert_eq!(row(&buf, 9), "│   PID … CPU% ↓ │");
    assert_eq!(row(&buf, 10), "│   631 …   25.0 │");
    // `s` goes to the next column on screen: PID, from the lowest
    assert!(app.handle_key(key('s')).is_continue());
    let buf = render_buffer(&mut app, 18, 20);
    assert_eq!(row(&buf, 9), "│ PID ↑ NAME     │");
    assert_eq!(row(&buf, 10), "│     1 launchd  │");

    // USER goes before NAME gets fewer than 16 cells
    let buf = render_buffer(&mut app, 66, 24);
    let header = row(&buf, 11);
    assert!(header.contains(&format!(" NAME{} USER ", " ".repeat(12))), "{header}");
    let buf = render_buffer(&mut app, 65, 24);
    let header = row(&buf, 11);
    assert!(header.contains(&format!(" NAME{}  CPU% ", " ".repeat(23))), "{header}");
  }

  #[test]
  fn proc_table_keeps_a_blank_cell_at_both_borders() {
    // the widest pids (5 digits) and a name longer than its column
    let mut procs = varied_procs();
    procs[0].pid = 99_998;
    procs[1].name = "x".repeat(300);
    let mut app = app_with_procs(procs);
    assert!(press(&mut app, KeyCode::Down).is_continue());

    for (width, height) in [(200, 50), (80, 24), (40, 20), (18, 20)] {
      let buf = render_buffer(&mut app, width, height);
      let proc = app.layout(buf.area).proc.expect("process box");
      for y in proc.top() + 1..proc.bottom() - 1 {
        let ctx = format!("{width}x{height}: {}", row(&buf, y));
        assert_eq!(buf[(proc.left() + 1, y)].symbol(), " ", "{ctx}");
        assert_eq!(buf[(proc.right() - 2, y)].symbol(), " ", "{ctx}");
      }

      let screen = screen_text(&buf);
      assert!(screen.contains("│ 99998 "), "{width}x{height}");
      // the selected row is one bar from border to border, padding cells included
      let selected = proc.top() + 2;
      for x in proc.left() + 1..proc.right() - 1 {
        let cell = &buf[(x, selected)];
        assert!(cell.modifier.contains(Modifier::REVERSED), "{width}x{height}: x {x}");
      }
    }
  }

  #[test]
  fn proc_sort_keys_persist_in_config() {
    let file = TempConfig::new("proc_sort_keys_persist_in_config");
    let mut app = with_procs(saving_app(&file), varied_procs());
    assert!(app.handle_key(key('s')).is_continue());
    assert_eq!(app.cfg.proc_sort, ProcSort::Mem);
    assert!(app.cfg.proc_sort_desc);
    assert_eq!(file.saved()["proc_sort"], "Mem");
    assert_eq!(file.saved()["proc_sort_desc"], true);

    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 1).contains(" CPU%  MEM ↓ "), "{}", proc_row(&buf, 1));
    assert!(proc_row(&buf, 2).contains("Safari"), "largest memory first");

    assert!(app.handle_key(key('S')).is_continue());
    assert!(!app.cfg.proc_sort_desc);
    assert_eq!(file.saved()["proc_sort_desc"], false);
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 1).contains(" CPU%  MEM ↑ "), "{}", proc_row(&buf, 1));
    assert!(proc_row(&buf, 2).contains("launchd"));

    // the next run starts with the saved sort
    let app = App::from_parts(test_soc(), Config::load_from(Some(file.path())));
    let mut app = with_procs(app, varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 1).contains(" CPU%  MEM ↑ "), "{}", proc_row(&buf, 1));
    assert_eq!(shown_pids(&app), [1, 631, 2301]);
  }

  #[test]
  fn typing_filter_ignores_global_keys() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());

    for c in ['q', 'c', 'v', 'd', 'r', 'p', '5', '+', '-', 's'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?} while typing");
    }
    assert_eq!(app.proc_view.filter(), "qcvdrp5+-s");
    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval(), 1000);
    assert!(app.cfg.show_procs);
    assert_eq!(app.cfg.proc_sort, ProcSort::Cpu);

    // the filter shows in the title with a cursor, no process matches it
    let buf = render_buffer(&mut app, 200, 50);
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 0/3 ─ /qcvdrp5+-s█ ─"), "{title}");

    // ctrl-c still quits
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));

    // esc leaves input mode, then q quits again
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(!app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));
  }

  #[test]
  fn filter_narrows_the_table() {
    let mut app = app_with_procs(varied_procs());
    for c in "/SAF".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(press(&mut app, KeyCode::Enter).is_continue());

    let buf = render_buffer(&mut app, 200, 50);
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 1/3 ─ /SAF ─"), "{title}");
    assert!(proc_row(&buf, 2).contains("Safari"));
    assert!(!proc_row(&buf, 3).contains("WindowServer"));
  }

  #[test]
  fn proc_keys_ignored_while_panel_hidden() {
    let mut app = test_app();
    render_buffer(&mut app, 60, 12); // auto-hidden

    assert!(app.handle_key(key('/')).is_continue());
    assert!(!app.proc_view.typing());
    assert!(app.handle_key(key('s')).is_continue());
    assert_eq!(app.cfg.proc_sort, ProcSort::Cpu);
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    // hiding the panel while typing ends the input mode
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.proc_view.typing());
    render_buffer(&mut app, 60, 12);
    assert!(!app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));
  }

  fn shown_pids(app: &App) -> Vec<i32> {
    app.proc_view.rows().map(|p| p.pid).collect()
  }

  #[test]
  fn click_on_header_sorts_and_again_reverses() {
    use ProcSort::*;
    let file = TempConfig::new("click_on_header_sorts_and_again_reverses");
    let mut app = with_procs(saving_app(&file), varied_procs());

    // (header, sort key, its first direction, pids then, pids reversed); numbers sort largest
    // first, PIDs and text from the start
    let cases = [
      ("PID", Pid, false, [1, 631, 2301], [2301, 631, 1]),
      ("NAME", Name, false, [1, 2301, 631], [631, 2301, 1]),
      // ties by pid
      ("USER", User, false, [1, 631, 2301], [631, 2301, 1]),
      ("MEM", Mem, true, [2301, 631, 1], [1, 631, 2301]),
      // launchd has no power reading: last both ways
      ("POWER", Power, true, [631, 2301, 1], [2301, 631, 1]),
      ("GPU%", Gpu, true, [631, 2301, 1], [1, 2301, 631]),
      ("CPU%", Cpu, true, [631, 2301, 1], [1, 2301, 631]),
    ];
    let arrow = |desc: bool| if desc { "↓" } else { "↑" };
    for (header, sort, desc, first, reversed) in cases {
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      click(&mut app, x_of(&line, header), PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, desc), "{header}");
      assert_eq!(shown_pids(&app), first, "{header}");
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      assert_eq!(line.matches('↓').count() + line.matches('↑').count(), 1, "{line}");

      // again, on the arrow this time: the whole header cell counts
      let at = x_of(&line, &format!("{header} {}", arrow(desc))) + header.len() as u16 + 1;
      click(&mut app, at, PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, !desc), "{header}");
      // saved like `s` / `S`
      assert_eq!(file.saved()["proc_sort"], format!("{sort:?}"), "{header}");
      assert_eq!(file.saved()["proc_sort_desc"], !desc, "{header}");
      assert_eq!(shown_pids(&app), reversed, "{header}");
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      assert!(line.contains(&format!("{header} {}", arrow(!desc))), "{line}");

      // and back
      click(&mut app, x_of(&line, header), PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, desc), "{header}");
    }

    assert_eq!(file.saved()["proc_sort"], "Cpu");
    assert_eq!(file.saved()["proc_sort_desc"], true);
  }

  #[test]
  fn long_filter_keeps_its_end_and_cursor_on_the_border() {
    let mut app = app_with_procs(varied_procs());
    let filter = "abcdefghij".repeat(25);
    for c in format!("/{filter}").chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }

    // `/…`, the end of the filter and the cursor, next to the count
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert_eq!(title, format!("╭─ proc 0/3 ─ /…{}█ ─╮", &filter[70..]));

    // kept: one more character instead of the cursor; `/` edits it again
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert_eq!(title, format!("╭─ proc 0/3 ─ /…{} ─╮", &filter[69..]));
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.proc_view.typing());

    // no room next to the count: the filter takes its place
    let buf = render_buffer(&mut app, 20, 50);
    assert_eq!(app.layout(buf.area).proc.map(|r| r.y), Some(PROC_Y));
    assert_eq!(proc_row(&buf, 0), format!("╭─ /…{}█ ─╮", &filter[239..]));
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    let buf = render_buffer(&mut app, 20, 50);
    assert_eq!(proc_row(&buf, 0), format!("╭─ /…{} ─╮", &filter[238..]));
  }

  #[test]
  fn narrow_process_box_drops_the_filter_label() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 23, 50);
    assert_eq!(proc_row(&buf, 0), "╭─ proc 3 ─ / filter ─╮");

    // one cell less: no label
    let buf = render_buffer(&mut app, 22, 50);
    assert_eq!(proc_row(&buf, 0), format!("╭─ proc 3 {}╮", "─".repeat(11)));

    // `/` still does, and the filter shows up
    assert!(app.handle_key(key('/')).is_continue());
    let title = proc_row(&render_buffer(&mut app, 22, 50), 0);
    assert!(title.starts_with("╭─ proc 3 ─ /█ ─"), "{title}");
  }

  /// App with 100 processes `proc0`… (pids 1000…) in the same order by CPU and pid; 27 of them
  /// fit in a 200x50 window.
  fn hundred_procs_app() -> App {
    let procs: Vec<ProcInfo> = (0..100)
      .map(|i| ProcInfo { pid: 1000 + i, name: format!("proc{i}"), ..test_procs()[0].clone() })
      .collect();
    let mut app = app_with_procs(procs);
    render_buffer(&mut app, 200, 50);
    app
  }

  #[test]
  fn click_on_row_selects_its_process() {
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    render_buffer(&mut app, 200, 50);

    // anywhere across the row, the blank cells at the borders too
    for (x, row, pid) in [(100, 1, 2301), (1, 2, 1), (198, 0, 631)] {
      click(&mut app, x, PROC_Y + 2 + row);
      assert_eq!(app.proc_view.selected_pid(), Some(pid), "x {x}, row {row}");
    }
    let buf = render_buffer(&mut app, 200, 50);
    assert!(buf[(100, PROC_Y + 2)].modifier.contains(Modifier::REVERSED));

    // blank rows below the last process keep the selection
    click(&mut app, 100, PROC_Y + 10);
    assert_eq!(app.proc_view.selected_pid(), Some(631));

    // the selected row again: no selection
    click(&mut app, 30, PROC_Y + 2);
    assert_eq!(app.proc_view.selected_pid(), None);
    let buf = render_buffer(&mut app, 200, 50);
    assert!(!buf[(100, PROC_Y + 2)].modifier.contains(Modifier::REVERSED));

    // a scrolled table (End scrolls without a selection): the row on screen, not the row from
    // the top
    let mut app = hundred_procs_app();
    assert!(press(&mut app, KeyCode::End).is_continue());
    assert_eq!(app.proc_view.selected_pid(), None);
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc73 "));
    click(&mut app, 50, PROC_Y + 2);
    assert_eq!(app.proc_view.selected_pid(), Some(1073));
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc73 "), "the table doesn't move");
  }

  #[test]
  fn wheel_moves_selection_and_scrolls() {
    let mut app = hundred_procs_app();
    let wheel = |app: &mut App, kind| {
      app.handle_mouse(mouse(kind, 100, PROC_Y + 10));
    };
    // (screen row of the selection, its process)
    let selected = |app: &mut App| {
      let buf = render_buffer(app, 200, 50);
      let rows = (2..29).filter(|&y| buf[(1, PROC_Y + y)].modifier.contains(Modifier::REVERSED));
      let rows: Vec<u16> = rows.map(|y| y - 2).collect();
      assert_eq!(rows.len(), 1, "{rows:?}");
      (rows[0], app.proc_view.selected_pid().unwrap() - 1000)
    };
    use MouseEventKind::{ScrollDown, ScrollUp};
    let top = |app: &mut App| proc_row(&render_buffer(app, 200, 50), 2);

    // without a selection: it only scrolls, and ↓ then selects the top row on screen
    wheel(&mut app, ScrollDown);
    wheel(&mut app, ScrollDown);
    assert!(top(&mut app).contains(" proc6 "), "{}", top(&mut app));
    wheel(&mut app, ScrollUp);
    assert!(top(&mut app).contains(" proc3 "), "{}", top(&mut app));
    assert_eq!(app.proc_view.selected_pid(), None);
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert_eq!(selected(&mut app), (0, 3));
    // the top of the table: the selection moves up on screen instead
    wheel(&mut app, ScrollUp);
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (0, 0));

    // the selected row keeps its place on screen
    for _ in 0..5 {
      assert!(press(&mut app, KeyCode::Down).is_continue());
    }
    assert_eq!(selected(&mut app), (5, 5));
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (5, 8));
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (5, 5));

    // the end of the table: the selection moves down on screen, then stops
    assert!(press(&mut app, KeyCode::End).is_continue());
    assert_eq!(selected(&mut app), (26, 99));
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (26, 96));
    wheel(&mut app, ScrollDown);
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (26, 99));

    // the wheel over the metrics box doesn't move the list
    wheel(&mut app, ScrollUp);
    app.handle_mouse(mouse(ScrollUp, 100, 3));
    assert_eq!(selected(&mut app), (26, 96));
  }

  #[test]
  fn clicks_outside_targets_do_nothing() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    let header = proc_row(&buf, 1);
    let state = |app: &App| {
      let view = &app.proc_view;
      let cfg = serde_json::to_string(&app.cfg).unwrap();
      (cfg, view.typing(), view.filter().to_string(), view.selected_pid(), shown_pids(app))
    };
    let before = state(&app);

    let footer = row(&buf, 49);
    let cells = [
      // borders and the padding cells of the header row, the gaps between header cells
      (0, PROC_Y + 1),
      (1, PROC_Y + 1),
      (198, PROC_Y + 1),
      (199, PROC_Y + 3),
      (x_of(&header, "CPU% ↓") - 1, PROC_Y + 1),
      (x_of(&header, "USER") + 10, PROC_Y + 1),
      // top border, the count title, the blank cells around the sort hint
      (100, PROC_Y),
      (4, PROC_Y),
      (x_of(&proc_row(&buf, 0), "s sort") - 1, PROC_Y),
      (x_of(&proc_row(&buf, 0), "s sort") + 6, PROC_Y),
      // the bottom border: the note, the separators and blank cells around the key hints
      (5, 49),
      (100, 49),
      (x_of(&footer, "q quit") - 1, 49),
      (x_of(&footer, "| ? help"), 49),
      (198, 49),
      // metrics box
      (0, 0),
      (10, 2),
      (100, 3),
    ];
    for (x, y) in cells {
      click(&mut app, x, y);
      assert_eq!(state(&app), before, "click at {x}, {y}");
    }

    // other buttons, releases, drags and moves over the targets
    use MouseEventKind::*;
    let kinds = [Down(MouseButton::Right), Up(MouseButton::Left), Drag(MouseButton::Left), Moved];
    let targets = [
      (x_of(&header, "MEM"), PROC_Y + 1),
      (x_of(&proc_row(&buf, 0), "/"), PROC_Y),
      (x_of(&proc_row(&buf, 0), "s sort"), PROC_Y),
      (x_of(&footer, "q quit"), 49),
      (x_of(&footer, "v graph"), 49),
    ];
    for kind in kinds {
      for (x, y) in targets.into_iter().chain([(100, PROC_Y + 2)]) {
        app.handle_mouse(mouse(kind, x, y));
        assert_eq!(state(&app), before, "{kind:?} at {x}, {y}");
      }
    }
    assert_eq!(render_to_string(&mut app, 200, 50), screen_text(&buf));
  }

  #[test]
  fn mouse_does_nothing_while_process_list_hidden() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    let mem = (x_of(&proc_row(&buf, 1), "MEM"), PROC_Y + 1);
    let filter = (x_of(&proc_row(&buf, 0), "/ filter"), PROC_Y);
    let row = (100, PROC_Y + 2);
    let try_all = |app: &mut App| {
      for (x, y) in [mem, filter, row] {
        click(app, x, y);
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, x, y));
      }
      assert_eq!((app.cfg.proc_sort, app.proc_view.typing()), (ProcSort::Cpu, false));
      assert_eq!(app.proc_view.selected_pid(), None);
    };

    // hidden with `p`, then auto-hidden in a small window
    assert!(app.handle_key(key('p')).is_continue());
    render_buffer(&mut app, 200, 50);
    try_all(&mut app);

    assert!(app.handle_key(key('p')).is_continue());
    render_buffer(&mut app, 200, 50);
    put_procs(&mut app, varied_procs());
    render_buffer(&mut app, 200, 50);
    render_buffer(&mut app, 60, 12);
    try_all(&mut app);

    // back on screen, the same clicks work again
    render_buffer(&mut app, 200, 50);
    put_procs(&mut app, varied_procs());
    render_buffer(&mut app, 200, 50);
    click(&mut app, mem.0, mem.1);
    assert_eq!(app.cfg.proc_sort, ProcSort::Mem);
  }

  #[test]
  fn selected_row_is_highlighted_and_scrolled_into_view() {
    let mut app = hundred_procs_app();

    // reverse video in the default colors
    let selected = |buf: &Buffer, x: u16, y: u16| {
      let cell = &buf[(x, PROC_Y + y)];
      cell.modifier.contains(Modifier::REVERSED) && cell.fg == Color::Reset
    };

    // same CPU everywhere: ordered by pid
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc0 "));
    let reversed = |cell: &ratatui::buffer::Cell| cell.modifier.contains(Modifier::REVERSED);
    assert!(!buf.content.iter().any(reversed), "no selection yet");

    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert_eq!(app.proc_view.selected_pid(), Some(1001));
    let buf = render_buffer(&mut app, 200, 50);
    let line = proc_row(&buf, 3);
    assert!(line.contains("proc1 "));
    // the whole row, gradient-colored CPU% too
    let cpu = x_of(&line, "12.5");
    for x in [1, 100, cpu, 198] {
      assert!(selected(&buf, x, 3), "x {x}");
    }
    assert_eq!(buf[(cpu, PROC_Y + 2)].fg, gradient(0.125));
    assert!(!selected(&buf, 1, 2));
    assert!(!selected(&buf, 0, 3), "border not highlighted");
    assert_eq!(buf.content.iter().filter(|cell| reversed(cell)).count(), 198);

    // End: the last process is on the last row (29 is the border); 27 rows on screen
    assert!(press(&mut app, KeyCode::End).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 28).contains("proc99 "));
    assert!(selected(&buf, 1, 28));
    assert!(proc_row(&buf, 2).contains("proc73 "), "{}", proc_row(&buf, 2));

    // esc clears the selection, the table goes back to the top
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc0 "));
  }

  #[test]
  fn selected_process_and_its_path_on_the_bottom_border() {
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]

    // nothing selected: a note on POWER, as launchd has no reading next to processes with one
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), border(200, NOTE, &hints(6)));
    assert_eq!(buf[(3, 49)].fg, theme::DIM);

    // the PID and the full path
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let selected = format!(" 631 {WINDOW_SERVER} ");
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert_eq!(bottom, border(200, &selected, &hints(6)));
    let buf = render_buffer(&mut app, 200, 50);
    assert!(buf[(3, 49)].modifier.contains(Modifier::BOLD), "the PID");
    assert_eq!(buf[(7, 49)].fg, theme::TEXT);

    // narrower: the path is cut from the left to the room the hints leave, and left out with
    // fewer than 8 cells; then the hints drop
    let cut = |keep: usize| format!(" 631 …{} ", &WINDOW_SERVER[WINDOW_SERVER.len() - keep..]);
    let bottom = |app: &mut App, width| row(&render_buffer(app, width, 50), 49);
    assert_eq!(bottom(&mut app, 158), border(158, &selected, &hints(6)));
    assert_eq!(bottom(&mut app, 157), border(157, &cut(84), &hints(6)));
    assert_eq!(bottom(&mut app, 120), border(120, &cut(47), &hints(6)));
    assert_eq!(bottom(&mut app, 76), border(76, &cut(3), &hints(6)));
    assert_eq!(bottom(&mut app, 75), hints_border(75, 6));
    assert_eq!(bottom(&mut app, 40), hints_border(40, 3));

    // another process; no path: the name
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(bottom.starts_with("╰─ 2301 /Applications/Safari.app/Contents/MacOS/Safari ─"));
    let mut app = app_with_procs(test_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert_eq!(bottom, border(200, " 1 launchd ", &hints(6)));

    // cleared: every process has power here, so no note either
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert_eq!(row(&render_buffer(&mut app, 200, 50), 49), hints_border(200, 6));
  }

  #[test]
  fn power_note_gives_way_and_follows_the_power_column() {
    let mut app = app_with_procs(varied_procs());
    // the note gets the room the hints leave: 4 corner cells, the note, a gap and 61 cells of
    // hints; it is cut when shorter, and left out with fewer than 8 cells
    assert_eq!(row(&render_buffer(&mut app, 93, 50), 49), border(93, NOTE, &hints(6)));
    let cut = " POWER: own processes on… ";
    assert_eq!(row(&render_buffer(&mut app, 92, 50), 49), border(92, cut, &hints(6)));
    assert_eq!(row(&render_buffer(&mut app, 76, 50), 49), border(76, " POWER:… ", &hints(6)));
    assert_eq!(row(&render_buffer(&mut app, 75, 50), 49), hints_border(75, 6));

    // no POWER column on screen, no note
    let buf = render_buffer(&mut app, 46, 50);
    assert!(!proc_row(&buf, 1).contains("POWER"));
    assert_eq!(row(&buf, 49), hints_border(46, 4));

    // every process with a reading (root), or none: no note
    for power in [Some(0.5), None] {
      let procs = varied_procs().into_iter().map(|p| ProcInfo { power_w: power, ..p }).collect();
      put_procs(&mut app, procs);
      assert_eq!(row(&render_buffer(&mut app, 200, 50), 49), hints_border(200, 6));
    }
  }

  #[test]
  fn sort_hint_shows_and_arrows_do_nothing() {
    use ProcSort::*;
    let file = TempConfig::new("sort_hint_shows_and_arrows_do_nothing");
    let mut app = with_procs(saving_app(&file), varied_procs());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert!(title.starts_with("╭─ proc 3 ─ / filter ─ s sort ─"), "{title}");
    // a hint: the key bold, the label plain
    let x = x_of(&title, "s sort");
    assert!(render_buffer(&mut app, 200, 50)[(x, PROC_Y)].modifier.contains(Modifier::BOLD));

    // ← / → don't sort
    for code in [KeyCode::Left, KeyCode::Right] {
      assert!(press(&mut app, code).is_continue());
    }
    assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (Cpu, true));
    assert!(!file.path().exists(), "nothing saved");
    // a click on it does nothing
    click(&mut app, x, PROC_Y);
    assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (Cpu, true));

    // gone while a filter is typed (`s` is text then), back after the kept filter
    assert!(app.handle_key(key('/')).is_continue());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert!(title.starts_with("╭─ proc 3 ─ /█ ──") && !title.contains("sort"), "{title}");
    for c in "saf".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert!(title.starts_with("╭─ proc 1/3 ─ /saf ─ s sort ─"), "{title}");
  }

  #[test]
  fn typing_a_filter_changes_the_footer_and_says_when_nothing_matches() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());
    let typing = " Enter keep | Esc clear | ↑↓ select ";
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), border(200, NOTE, typing));

    // ↑↓ select while typing
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let first = app.proc_view.selected_pid().expect("↓ selects the top row");
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let second = app.proc_view.selected_pid();
    assert!(second.is_some() && second != Some(first), "↓ moves down");
    assert!(press(&mut app, KeyCode::Up).is_continue());
    assert_eq!(app.proc_view.selected_pid(), Some(first));
    assert!(app.proc_view.typing());
    // the selected process on the left
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert_eq!(bottom, border(200, &format!(" 631 {WINDOW_SERVER} "), typing));

    // Enter keeps the filter
    for c in "zz".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    let buf = render_buffer(&mut app, 200, 50);
    let message = "no process matches \"zz\"";
    let middle = (PROC_Y + 2..49).find(|&y| row(&buf, y).contains(message)).expect(message);
    assert_eq!(middle, PROC_Y + 2 + 13, "centered in the 27 table rows");
    assert_eq!(buf[(x_of(&row(&buf, middle), "no"), middle)].fg, theme::DIM);
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    assert!(!app.proc_view.typing());
    assert_eq!(app.proc_view.filter(), "zz");

    // kept: the global hints are back, the message stays while nothing matches
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).ends_with(&format!("{}─╯", hints(6))));
    assert!(row(&buf, middle).contains(message));
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(!render_to_string(&mut app, 200, 50).contains("no process matches"));
  }

  #[test]
  fn help_opens_over_the_screen_and_closes() {
    let file = TempConfig::new("help_opens_over_the_screen_and_closes");
    let mut app = with_procs(saving_app(&file), varied_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let screen = render_to_string(&mut app, 200, 50);

    assert!(app.handle_key(key('?')).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    let rows: Vec<String> = (0..50).map(|y| row(&buf, y)).collect();
    let top = rows.iter().position(|r| r.contains("╭─ help ─")).expect("help box");
    assert!(rows[top].contains("─ Esc close ─╮"), "{}", rows[top]);
    assert!(!rows[top].contains("scroll"), "everything fits");
    // the `Esc close` hint: the key bold, the label plain
    let esc = x_of(&rows[top], "Esc close");
    assert!(buf[(esc, top as u16)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(esc + 4, top as u16)].modifier.contains(Modifier::BOLD));
    // the text of the box only: 18 lines between its borders
    let left = x_of(&rows[top], "╭─ help");
    let width = rows[top].chars().skip(usize::from(left)).position(|c| c == '╮').unwrap() + 1;
    let bottom = (top..50).find(|&y| buf[(left, y as u16)].symbol() == "╰").expect("box end");
    assert_eq!(bottom - top, 19);
    let line = |y: usize| text(&buf, Rect::new(left, y as u16, width as u16, 1));
    let text = (top..=bottom).map(line).collect::<Vec<_>>().join("\n");
    for line in [
      " Keys ",
      "show / hide the processes",
      "s / S",
      "↑ ↓",
      " Mouse ",
      "a column header to sort, a row to select",
      " Notes ",
      "100% is one fully busy core",
      "POWER -",
      "not available for other users' processes",
      "Option (iTerm2)",
    ] {
      assert!(text.contains(line), "missing {line:?}");
    }
    // the essentials only: no paging keys, arrow sort, wheel or filter typing details
    for line in ["PgUp", "Home", "← →", "wheel", "Typing", "Enter"] {
      assert!(!text.contains(line), "{line:?} in the help");
    }

    // other keys do nothing while it is open; `q`, Esc and `?` close it
    for c in ['p', 'v', 'r', '+', 's', '/'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(!file.path().exists(), "nothing saved");
    assert!(app.help.is_some() && !app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Continue(()), "q closes the help");
    assert_eq!(render_to_string(&mut app, 200, 50), screen);
    for close in [KeyCode::Esc, KeyCode::Char('?')] {
      assert!(app.handle_key(key('?')).is_continue());
      assert!(press(&mut app, close).is_continue());
      assert!(app.help.is_none(), "{close:?}");
    }
    assert_eq!(app.proc_view.selected_pid(), Some(631), "Esc closed only the help");

    // a click closes it; Ctrl-C still quits
    assert!(app.handle_key(key('?')).is_continue());
    render_buffer(&mut app, 200, 50);
    click(&mut app, 100, 25);
    assert!(app.help.is_none() && app.proc_view.selected_pid() == Some(631));
    assert!(app.handle_key(key('?')).is_continue());
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.handle_key(ctrl_c).is_break());

    // while typing a filter `?` is text
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.handle_key(key('?')).is_continue());
    assert_eq!((app.help, app.proc_view.filter()), (None, "?"));
  }

  #[test]
  fn help_scrolls_when_the_window_is_short() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('?')).is_continue());
    let lines = |app: &mut App, width, height| {
      let buf = render_buffer(app, width, height);
      (0..height).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n")
    };

    // 80x24: all of it, from the first section to the last note
    let text = lines(&mut app, 80, 24);
    assert!(text.contains("╭─ help ─") && text.contains("─ Esc close ─╮"), "{text}");
    assert!(text.contains(" Keys ") && text.contains("select text"), "{text}");

    // 80x14: 12 of its 18 lines; ↑↓ scroll, the wheel too, never past its end
    let text = lines(&mut app, 80, 14);
    assert!(text.contains(" Keys ") && !text.contains("select text"), "{text}");
    for _ in 0..40 {
      assert!(press(&mut app, KeyCode::Down).is_continue());
    }
    let text = lines(&mut app, 80, 14);
    assert!(text.contains("select text") && !text.contains(" Keys "), "{text}");
    assert!(press(&mut app, KeyCode::Up).is_continue());
    assert!(!lines(&mut app, 80, 14).contains("select text"), "one line back up");
    for _ in 0..20 {
      app.handle_mouse(mouse(MouseEventKind::ScrollUp, 40, 7));
    }
    assert!(lines(&mut app, 80, 14).contains(" Keys "));

    // any size: inside the screen, corners in place
    for (width, height) in [(400, 120), (200, 50), (80, 24), (40, 12), (10, 4), (2, 2), (1, 1)] {
      let buf = render_buffer(&mut app, width, height);
      if width >= 2 && height >= 2 {
        let text = lines(&mut app, width, height);
        assert!(text.contains("╭─ ") || width < 5, "{width}x{height}");
      }
      assert_eq!(buf.area, Rect::new(0, 0, width, height));
    }
  }

  #[test]
  fn a_hide_ends_the_showing_even_when_the_panel_is_back_at_once() {
    let shown = Arc::new(ProcsShown::default());
    assert_eq!(shown.showing(), None);
    shown.set(true);
    let first = shown.wait_shown();
    assert_eq!(shown.showing(), Some(first));
    // setting the same state again changes nothing
    shown.set(true);
    assert!(shown.wait_while_shown(first, Duration::from_millis(10)), "the showing goes on");

    // hidden and shown again before the process thread looks: a new showing all the same
    shown.set(false);
    shown.set(true);
    let started = Instant::now();
    assert!(!shown.wait_while_shown(first, Duration::from_secs(10)));
    assert!(started.elapsed() < Duration::from_secs(1), "no wait for the old showing");
    let second = shown.wait_shown();
    assert_ne!(second, first);
    assert_eq!(shown.showing(), Some(second));

    // a hide wakes a waiting thread at once
    let waiter = {
      let shown = shown.clone();
      thread::spawn(move || shown.wait_while_shown(second, Duration::from_secs(10)))
    };
    thread::sleep(Duration::from_millis(50));
    let hidden = Instant::now();
    shown.set(false);
    assert!(!waiter.join().unwrap());
    assert!(hidden.elapsed() < Duration::from_secs(1), "{:?}", hidden.elapsed());
  }

  #[test]
  fn procs_thread_samples_only_while_shown() {
    let (tx, rx) = mpsc::channel();
    let shown = Arc::new(ProcsShown::default());
    let msec = Arc::new(RwLock::new(TUI_MIN_MS));
    let sampler = run_procs_thread(tx, msec.clone(), shown.clone(), proc_sampler);

    // Shows the panel; returns how long the first sample took and the sample, which belongs to
    // this showing.
    let show = |shown: &ProcsShown| {
      let started = Instant::now();
      shown.set(true);
      let Ok(Event::Procs { showing, procs }) = rx.recv_timeout(Duration::from_secs(5)) else {
        panic!("no process sample");
      };
      assert_eq!(Some(showing), shown.showing());
      (started.elapsed(), procs)
    };

    // hidden: nothing
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "sampled while hidden");

    // shown: the baseline sample stays silent, the first message comes after the warm-up with
    // the own process in it
    let (waited, procs) = show(&shown);
    assert!(waited >= PROCS_WARMUP, "{waited:?}: the baseline sample was sent");
    let pid = std::process::id() as i32;
    assert!(procs.iter().any(|p| p.pid == pid && !p.name.is_empty()));

    // hidden again: a sample already in progress may still arrive, but no more
    shown.set(false);
    let deadline = Instant::now() + Duration::from_millis(1200);
    let mut late = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
      if rx.recv_timeout(left).is_ok() {
        late += 1;
      }
    }
    assert!(late <= 1, "{late} samples after hiding");

    // the hide dropped the sampler: a new baseline and warm-up, then the long interval
    *msec.write().unwrap() = TUI_MAX_MS;
    let (waited, _) = show(&shown);
    assert!(waited >= PROCS_WARMUP, "{waited:?}: no new baseline after the hide");

    // hidden and shown again within the 10 s interval (`show` waits 5 s at most), after a while
    // and at once: either way a new baseline and warm-up, not the rest of the interval
    shown.set(false);
    thread::sleep(Duration::from_millis(300));
    let (waited, _) = show(&shown);
    assert!(waited >= PROCS_WARMUP, "{waited:?}");
    shown.set(false);
    let (waited, _) = show(&shown);
    assert!(waited >= PROCS_WARMUP, "{waited:?}");

    // exits once the receiver is gone (at its next send, after a hide cuts the wait short)
    drop(rx);
    shown.set(false);
    shown.set(true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sampler.is_finished() {
      assert!(Instant::now() < deadline, "the process thread doesn't exit");
      thread::sleep(Duration::from_millis(10));
    }
    sampler.join().expect("process thread exits cleanly");
  }

  #[test]
  fn a_sample_started_before_a_hide_and_show_never_reaches_the_list() {
    let mut app = test_app();
    render_buffer(&mut app, 200, 50);

    // a sampler that reports each sample as it starts and finishes it only when released
    let (started_tx, started) = mpsc::channel();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(gate));
    let new_sampler = move || {
      let (started_tx, gate) = (started_tx.clone(), gate.clone());
      move || {
        let _ = started_tx.send(());
        let _ = gate.lock().unwrap().recv();
        test_procs()
      }
    };
    let (tx, rx) = mpsc::channel();
    let msec = Arc::new(RwLock::new(TUI_MIN_MS));
    run_procs_thread(tx, msec.clone(), app.procs_shown.clone(), new_sampler);

    let timeout = Duration::from_secs(5);
    // Lets the next sample of the process thread run to its end.
    let finish_sample = || {
      started.recv_timeout(timeout).expect("no sample started");
      release.send(()).unwrap();
    };
    // Hands the next event of the process thread to the app.
    let take_event = |app: &mut App| {
      let event = rx.recv_timeout(timeout).expect("no process sample");
      assert!(app.handle_event(event, &msec).is_continue());
    };

    // the baseline, then a sample that runs while the panel is hidden and shown again
    finish_sample();
    started.recv_timeout(timeout).expect("no sample started");
    render_buffer(&mut app, 60, 12);
    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));
    release.send(()).unwrap();
    take_event(&mut app);
    assert_eq!(app.proc_view.procs(), None, "a stale sample reached the list");
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));

    // the new showing: its own baseline, then its first sample fills the list
    finish_sample();
    finish_sample();
    take_event(&mut app);
    assert_eq!(app.proc_view.procs(), Some(test_procs().as_slice()));
  }
}
