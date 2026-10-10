//! Terminal user interface.

mod boxes;
mod help;
mod kill;
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

use kill::{Identity, Kill};
use layout::{LayoutPlan, compute_layout};
use macmon::{Metrics, Sampler, SocInfo};
use proc_view::ProcView;
use ratatui::crossterm::event::{
  self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, KeyCode,
  KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::{ExecutableCommand, cursor, terminal};
use ratatui::prelude::*;
use store::{CpuClusters, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};

use crate::config::{Config, TUI_MIN_MS};
use crate::procs::{ProcInfo, ProcSampler};

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

/// Raw mode, the alternate screen and focus reports (focus loss ends filter input); `run_loop`
/// turns mouse capture on while the process list is shown. Whatever happens next, the terminal is
/// restored by the guard, the panic hook or `leave_term`.
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
  stdout().execute(EnableFocusChange)?;
  Ok((Terminal::new(CrosstermBackend::new(stdout()))?, guard))
}

/// Restores the terminal once, whichever comes first: the normal exit, an error return or a panic.
fn leave_term() {
  restore_term_once(&TERM_ACTIVE, &mut stdout(), terminal::disable_raw_mode);
}

/// Turns mouse capture and focus reports off, leaves the alternate screen, shows the cursor
/// (ratatui hides it while drawing, and a panic aborts before the terminal is dropped) and turns
/// raw mode off when `active` is set, and clears it. Every step runs even if an earlier one fails.
/// Returns whether it ran.
fn restore_term_once(
  active: &AtomicBool,
  out: &mut impl Write,
  disable_raw_mode: impl FnOnce() -> io::Result<()>,
) -> bool {
  if !active.swap(false, Ordering::SeqCst) {
    return false;
  }

  let _ = out.execute(DisableMouseCapture);
  let _ = out.execute(DisableFocusChange);
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
  /// The terminal window lost focus.
  FocusLost,
  /// Redraw: the periodic tick, and a resize, so the mouse targets follow the new layout at once.
  Tick,
}

/// App event of a terminal event: keys, left clicks and the wheel, focus loss, and a resize as a
/// redraw.
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
    event::Event::FocusLost => Some(Event::FocusLost),
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

  /// CPU clusters, lowest tier first (E / P on M1–M4, E / S on M5, P / S on M5 Pro/Max, E / P / S
  /// on M6).
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
  /// The kill prompt (`k`), the process signalled last until it exits, and the messages.
  kill: Kill,
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

    self.clusters.push(&data);
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
  /// filter input and closes the kill prompt, so keys don't go to something that isn't on screen.
  fn set_procs_visible(&mut self, visible: bool) {
    self.procs_shown.set(visible);
    if !visible {
      self.proc_view.clear();
      self.kill.cancel();
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
      // switching away is done typing: keys back as shortcuts, the filter stays; and no answer
      // to the kill prompt
      Event::FocusLost => {
        self.proc_view.end_typing();
        self.kill.cancel();
        return ControlFlow::Continue(());
      }
      // the signalled process is followed by time, also while the list is hidden
      Event::Tick => {
        self.kill.tick(Instant::now());
        return ControlFlow::Continue(());
      }
    };

    *msec.write().unwrap() = self.cfg.interval();
    flow
  }

  /// Applies a key press to the app state. Returns `Break` when the app should quit. The help
  /// overlay takes every key while it is open, then the kill prompt; then keys of the process
  /// panel (only while it is on screen) take precedence, and while a filter is typed every key
  /// except Ctrl-C goes to it.
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

    if self.kill.asking() {
      self.kill.answer(key, Instant::now());
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
      KeyCode::Char('k') if self.procs_visible() => {
        if let Some(proc) = self.proc_view.selected() {
          let started = proc.started.map(Identity::from);
          self.kill.ask(proc.pid, &proc.name, started, Instant::now());
        }
      }
      _ => {}
    }

    ControlFlow::Continue(())
  }

  /// Whether the mouse is captured: only for the process list on screen, and not under the help,
  /// so the terminal handles clicks there (its link, text selection).
  fn wants_mouse(&self) -> bool {
    self.procs_visible() && self.help.is_none()
  }

  /// Applies a mouse event at the cells of the last frame to the process list, only while the
  /// mouse is captured (events still on the way when capture turns off are dropped). While the
  /// kill prompt is open, a click or the wheel only closes it.
  fn handle_mouse(&mut self, mouse: MouseEvent) {
    if self.kill.asking() {
      self.kill.cancel();
    } else if self.wants_mouse() {
      self.update_proc_view(|view| view.handle_mouse(mouse));
    }
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
      mouse = set_mouse_capture(&mut stdout(), mouse, self.wants_mouse())?;
      if self.handle_event(rx.recv()?, &msec).is_break() {
        break;
      }
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::num::NonZeroU16;
  use std::ops::ControlFlow;
  use std::sync::atomic::AtomicBool;
  use std::sync::{Arc, Mutex, RwLock, mpsc};
  use std::time::{Duration, Instant};

  use macmon::{CpuTierInfo, CpuTierMetrics, FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::{Buffer, CellDiffOption};
  use ratatui::crossterm::ExecutableCommand;
  use ratatui::crossterm::event::{
    EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
  };
  use ratatui::layout::Margin;

  use super::kill::{FakeSys, Kill};
  use super::layout::Metric;
  use super::theme::gradient;
  use super::{App, Event, restore_term_once, run_procs_thread};
  use crate::config::{Config, ProcSort, RatioMode, TUI_MIN_MS, TempConfig, ViewType};
  use crate::procs::ProcInfo;

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  fn press(app: &mut App, code: KeyCode) -> ControlFlow<()> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
  }

  fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
  }

  fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
  }

  /// Screen column where `text` starts in the rendered `line`.
  fn x_of(line: &str, text: &str) -> u16 {
    let i = line.find(text).unwrap_or_else(|| panic!("no {text:?} in {line}"));
    line[..i].chars().count() as u16
  }

  fn test_soc() -> SocInfo {
    let tier = |label: &str| CpuTierInfo { label: label.to_string(), cores: 6, freqs: vec![] };
    SocInfo {
      chip_name: "Apple M3 Pro".to_string(),
      memory_gb: 36,
      cpu_tiers: vec![tier("E"), tier("P")],
      gpu_cores: 18,
      ..Default::default()
    }
  }

  /// Sampled CPU tier `label` with no per-core metrics.
  fn cpu_tier(label: &str, freq_mhz: u32, scaled_ratio: f32, active_ratio: f32) -> CpuTierMetrics {
    let label = label.to_string();
    CpuTierMetrics { label, freq_mhz, scaled_ratio, active_ratio, cores: vec![] }
  }

  fn test_metrics() -> Metrics {
    Metrics {
      temp: TempMetrics { cpu_temp_avg: Some(45.0), gpu_temp_avg: Some(40.0) },
      memory: MemMetrics {
        ram_total: 36 << 30,
        ram_usage: 20 << 30,
        swap_total: 2 << 30,
        swap_usage: 1 << 30,
      },
      fans: vec![FanMetric { name: "fan0".to_string(), rpm: 1200, max_rpm: Some(6000) }],
      cpu_tiers: vec![cpu_tier("E", 1800, 0.42, 0.5), cpu_tier("P", 3200, 0.77, 0.8)],
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

  /// App for `test_soc` with a few samples of `test_metrics` changed by `edit`, killing through a
  /// fake without processes.
  fn test_app_with(edit: impl Fn(&mut Metrics)) -> App {
    let mut app = App::from_parts(test_soc(), Config::default());
    app.kill = Kill::new(Box::new(FakeSys::default()));
    for _ in 0..3 {
      let mut metrics = test_metrics();
      edit(&mut metrics);
      app.update_metrics(metrics);
    }
    app
  }

  fn test_app() -> App {
    test_app_with(|_| {})
  }

  /// `App::default()` before any sample, killing through a fake without processes.
  fn bare_app() -> App {
    App { kill: Kill::new(Box::new(FakeSys::default())), ..App::default() }
  }

  /// `test_app` saving its settings to `file`.
  fn saving_app(file: &TempConfig) -> App {
    App { cfg: Config::load_from(Some(file.path())), ..test_app() }
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

  /// Screen row where the process box starts in a 200x50 window: right under the metrics box, 40 %
  /// of the height.
  const PROC_Y: u16 = 20;

  /// Text of row `y` of the process box in a 200x50 window: 0 is the title, 1 the header.
  fn proc_row(buf: &Buffer, y: u16) -> String {
    row(buf, PROC_Y + y)
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
      started: None,
    };
    vec![proc(1, "launchd"), proc(631, "WindowServer"), proc(2301, "Safari")]
  }

  /// Path of the varied processes.
  const WINDOW_SERVER: &str =
    "/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/Resources/WindowServer";

  /// Processes with different values, one of them (root's launchd) without a power reading and a
  /// start time; the others started at the second of their pid, as in `FakeSys`.
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
      started: (pid >= 100).then_some((pid as u64, 0)),
    };
    vec![
      proc(1, "launchd", 0.0, 20, None, 0.0),
      proc(631, "WindowServer", 25.0, 300, Some(1.5), 40.0),
      proc(2301, "Safari", 12.0, 1536, Some(0.8), 5.0),
    ]
  }

  /// Hands `procs` to `app` as a sample of the panel's latest showing.
  fn put_procs(app: &mut App, procs: Vec<ProcInfo>) {
    let showing = app.procs_shown.state.lock().unwrap().count;
    app.update_procs(showing, procs);
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

  /// App with 100 processes `proc0`… (pids 1000…) in the same order by CPU and pid; 27 of them
  /// fit in a 200x50 window.
  fn hundred_procs_app() -> App {
    let procs = (0..100).map(|i| ProcInfo {
      pid: 1000 + i,
      name: format!("proc{i}"),
      ..test_procs()[0].clone()
    });
    let mut app = app_with_procs(procs.collect());
    render_buffer(&mut app, 200, 50);
    app
  }

  /// `app` killing through a fake with the processes of `varied_procs`.
  fn faking_kill(mut app: App) -> (App, FakeSys) {
    let fake = FakeSys::with(&[1, 631, 2301]);
    app.kill = Kill::new(Box::new(fake.clone()));
    (app, fake)
  }

  fn shown_pids(app: &App) -> Vec<i32> {
    app.proc_view.rows().map(|p| p.pid).collect()
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
    assert!(text.contains("\x1b[?1004l"), "focus reports stay on: {text:?}");
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
  fn quit_keys_break() {
    let mut app = bare_app();
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
  }

  #[test]
  fn setting_keys_change_and_save_the_config() {
    let file = TempConfig::new("setting_keys_change_and_save_the_config");
    let mut app = saving_app(&file);

    // r: active ratios, E-CPU 50 % instead of 42 %
    assert!(app.handle_key(key('r')).is_continue());
    assert_eq!(file.saved()["ratio_mode"], "Active");
    assert!(render_to_string(&mut app, 200, 50).contains("╭─ E-CPU 50% @ 1800 MHz ─"));

    // -/+ step the interval by 250 ms; `=` is `+` without shift
    for (c, interval) in [('+', 1250), ('=', 1500), ('-', 1250)] {
      assert!(app.handle_key(key(c)).is_continue());
      let saved = (app.cfg.interval(), file.saved()["interval"].clone());
      assert_eq!(saved, (interval, interval.into()), "{c}");
    }

    // v: the load boxes become gauges filled to the load, saved under the released name
    assert!(app.handle_key(key('v')).is_continue());
    assert_eq!(file.saved()["view_type"], "Gauge");
    let buf = render_buffer(&mut app, 200, 50);
    let gauge = app.layout(buf.area).boxes[0].1.inner(Margin::new(1, 1));
    let filled = (f64::from(gauge.width) * 0.5).round() as usize;
    let line = format!("{}{}", "█".repeat(filled), " ".repeat(usize::from(gauge.width) - filled));
    for y in gauge.top()..gauge.bottom() {
      let cells: String = (gauge.left()..gauge.right()).map(|x| buf[(x, y)].symbol()).collect();
      assert_eq!(cells, line, "row {y}");
    }
    // the key hints show the settings
    let hints = " v gauge | r active | -/+ 1250ms ─╯";
    assert!(row(&buf, 49).ends_with(hints), "{}", row(&buf, 49));

    assert!(app.handle_key(key('v')).is_continue());
    assert_eq!(app.cfg.view_type, ViewType::Graph);
    assert_eq!(file.saved()["view_type"], "Sparkline");
  }

  #[test]
  fn p_toggles_the_list_unless_the_window_is_too_small() {
    let file = TempConfig::new("p_toggles_the_list_unless_the_window_is_too_small");
    let mut app = with_procs(saving_app(&file), varied_procs());
    assert!(app.procs_visible());

    // hidden: the metrics take the whole screen, and no process is sampled
    assert!(app.handle_key(key('p')).is_continue());
    assert_eq!(file.saved()["show_procs"], false);
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(app.layout(buf.area).top, Some(buf.area));
    assert!(!screen_text(&buf).contains("WindowServer") && !app.procs_visible());

    // shown again: collecting until the next sample
    assert!(app.handle_key(key('p')).is_continue());
    assert_eq!(file.saved()["show_procs"], true);
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    assert!(app.procs_visible());

    // a window too small for the list keeps it hidden whatever the setting: `p` changes nothing,
    // and the key hints leave it out
    let buf = render_buffer(&mut app, 100, 12);
    assert!(!app.procs_visible());
    assert!(app.handle_key(key('p')).is_continue());
    assert!(app.cfg.show_procs);
    assert!(row(&buf, 11).contains(" q quit | ? help | v graph "), "{}", row(&buf, 11));
  }

  #[test]
  fn help_opens_over_the_screen_and_closes() {
    let file = TempConfig::new("help_opens_over_the_screen_and_closes");
    let mut app = with_procs(saving_app(&file), varied_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let screen = render_to_string(&mut app, 200, 50);

    assert!(app.handle_key(key('?')).is_continue());
    let help = render_to_string(&mut app, 200, 50);
    assert!(help.contains("╭─ help ─") && help.contains("show / hide the processes"));

    // other keys do nothing while it is open; `q`, Esc and `?` close it, and only it
    for c in ['p', 'v', 'r', '+', 's', '/'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(!file.path().exists(), "nothing saved");
    assert!(app.handle_key(key('q')).is_continue(), "q closes the help");
    assert_eq!(render_to_string(&mut app, 200, 50), screen);
    for close in [KeyCode::Esc, KeyCode::Char('?')] {
      assert!(app.handle_key(key('?')).is_continue());
      assert!(press(&mut app, close).is_continue());
      assert!(app.help.is_none(), "{close:?}");
    }
    assert_eq!(app.proc_view.selected_pid(), Some(631));
  }

  #[test]
  fn help_draws_in_any_window_even_without_rows_or_columns() {
    for (width, height) in [(5, 0), (0, 5), (0, 0), (1, 1), (3, 2), (80, 1), (1, 24)] {
      let mut app = with_procs(test_app(), varied_procs());
      assert!(app.handle_key(key('?')).is_continue());
      render_buffer(&mut app, width, height); // must not panic
      assert!(app.help.is_some(), "{width}x{height}");
    }
  }

  #[test]
  fn help_links_the_support_page_centered_and_only_when_the_line_fits() {
    let text = "buymeacoffee.com/vladkens";
    let link = format!("\x1b]8;;https://{text}\x1b\\{text}\x1b]8;;\x1b\\");
    let mut app = with_procs(test_app(), varied_procs());
    assert!(app.handle_key(key('?')).is_continue());

    let buf = render_buffer(&mut app, 200, 50);
    let at = buf.content.iter().position(|cell| cell.symbol() == link).expect("no link");
    assert_eq!(
      buf.content[at].diff_option,
      CellDiffOption::ForcedWidth(NonZeroU16::new(25).unwrap())
    );

    // centered: as many blank cells on each side up to the box borders, ±1; the line is
    // "Like macmon? Support it: " (25 cells) before the link (25 cells)
    let (x, y) = ((at % 200) as u16, (at / 200) as u16);
    let blank = |x: u16| buf[(x, y)].symbol() == " ";
    let left = (0..x - 25).rev().take_while(|&x| blank(x)).count();
    let right = (x + 25..200).take_while(|&x| blank(x)).count();
    assert!(left.abs_diff(right) <= 1, "{left} blank cells on the left, {right} on the right");

    // still whole in a 60 cells window; cut in a narrower one: plain text, no link
    let buf = render_buffer(&mut app, 60, 50);
    assert!(buf.content.iter().any(|cell| cell.symbol() == link));
    let buf = render_buffer(&mut app, 40, 50);
    assert!(screen_text(&buf).contains("Like macmon"));
    assert!(!buf.content.iter().any(|cell| cell.symbol().contains("\x1b]8")));
  }

  #[test]
  fn help_releases_the_mouse_for_its_link() {
    let mut app = with_procs(test_app(), varied_procs());
    assert!(app.wants_mouse());
    assert!(app.handle_key(key('?')).is_continue());
    assert!(!app.wants_mouse());

    // a click on the way when capture turned off doesn't reach the list under the help
    click(&mut app, 10, PROC_Y + 3);
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert_eq!(app.proc_view.selected_pid(), None);
    assert!(app.wants_mouse());
  }

  #[test]
  fn typing_filter_ignores_global_keys() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());

    for c in ['q', 'v', 'r', 'p', '+', '-', 's', '?'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?} while typing");
    }
    assert_eq!(app.proc_view.filter(), "qvrp+-s?");
    assert_eq!((app.cfg.view_type, app.cfg.ratio_mode), (ViewType::Graph, RatioMode::Scaled));
    assert_eq!((app.cfg.interval(), app.cfg.show_procs), (1000, true));
    assert_eq!((app.cfg.proc_sort, app.help), (ProcSort::Cpu, None));

    // ctrl-c still quits; after Esc `q` quits again
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(!app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));
  }

  #[test]
  fn focus_loss_and_clicks_end_typing_and_keep_the_filter() {
    let msec = RwLock::new(TUI_MIN_MS);
    let typing = |app: &mut App| {
      for c in "/saf".chars() {
        assert!(app.handle_key(key(c)).is_continue());
      }
      assert!(app.proc_view.typing());
    };

    // switching to another window
    let mut app = app_with_procs(varied_procs());
    typing(&mut app);
    assert!(app.handle_event(super::Event::FocusLost, &msec).is_continue());
    assert_eq!((app.proc_view.typing(), app.proc_view.filter()), (false, "saf"));
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    // a click, which also selects the row under it: Safari below the `saf` total and its line
    let mut app = app_with_procs(varied_procs());
    typing(&mut app);
    render_buffer(&mut app, 200, 50);
    click(&mut app, 10, PROC_Y + 4);
    assert_eq!((app.proc_view.typing(), app.proc_view.filter()), (false, "saf"));
    assert_eq!(app.proc_view.selected().map(|p| p.pid), Some(2301));
  }

  #[test]
  fn click_on_header_sorts_and_again_reverses() {
    use ProcSort::*;
    let file = TempConfig::new("click_on_header_sorts_and_again_reverses");
    let mut app = with_procs(saving_app(&file), varied_procs());
    let header = |app: &mut App| proc_row(&render_buffer(app, 200, 50), 1);

    // (header, sort key, its first direction, pids then, pids reversed): numbers sort largest
    // first, PIDs and text from the start
    let cases = [
      ("MEM", Mem, true, [2301, 631, 1], [1, 631, 2301]),
      ("PID", Pid, false, [1, 631, 2301], [2301, 631, 1]),
    ];
    for (column, sort, desc, first, reversed) in cases {
      let line = header(&mut app);
      click(&mut app, x_of(&line, column), PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, desc), "{column}");
      assert_eq!(shown_pids(&app), first, "{column}");

      // again, on the arrow this time: the whole header cell counts
      let line = header(&mut app);
      click(&mut app, x_of(&line, column) + column.len() as u16 + 1, PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, !desc), "{column}");
      assert_eq!(shown_pids(&app), reversed, "{column}");
      // saved like `s` / `S`
      assert_eq!(file.saved()["proc_sort"], format!("{sort:?}"), "{column}");
      assert_eq!(file.saved()["proc_sort_desc"], !desc, "{column}");
    }
  }

  #[test]
  fn click_on_row_selects_and_again_clears() {
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    render_buffer(&mut app, 200, 50);

    // anywhere across the row, the blank cells at the borders too
    for (x, row, pid) in [(100, 1, 2301), (1, 2, 1), (198, 0, 631)] {
      click(&mut app, x, PROC_Y + 2 + row);
      assert_eq!(app.proc_view.selected_pid(), Some(pid), "x {x}, row {row}");
    }

    // blank rows below the last process keep the selection, the selected row clears it
    click(&mut app, 100, PROC_Y + 10);
    assert_eq!(app.proc_view.selected_pid(), Some(631));
    click(&mut app, 30, PROC_Y + 2);
    assert_eq!(app.proc_view.selected_pid(), None);
  }

  #[test]
  fn wheel_scrolls_and_moves_the_selection() {
    use MouseEventKind::{ScrollDown, ScrollUp};
    let mut app = hundred_procs_app();
    let wheel = |app: &mut App, kind| app.handle_mouse(mouse(kind, 100, PROC_Y + 10));
    let top = |app: &mut App| proc_row(&render_buffer(app, 200, 50), 2);

    // without a selection it only scrolls; a click selects the row on screen
    wheel(&mut app, ScrollDown);
    wheel(&mut app, ScrollDown);
    assert!(top(&mut app).contains(" proc6 "), "{}", top(&mut app));
    click(&mut app, 50, PROC_Y + 2);
    assert_eq!(app.proc_view.selected_pid(), Some(1006));

    // with one, the selection moves along and keeps its place on screen
    wheel(&mut app, ScrollDown);
    assert_eq!(app.proc_view.selected_pid(), Some(1009));
    assert!(top(&mut app).contains(" proc9 "), "{}", top(&mut app));
    wheel(&mut app, ScrollUp);
    assert_eq!(app.proc_view.selected_pid(), Some(1006));

    // the wheel over the metrics box doesn't move the list
    app.handle_mouse(mouse(ScrollUp, 100, 3));
    assert_eq!(app.proc_view.selected_pid(), Some(1006));
  }

  #[test]
  fn mouse_does_nothing_while_process_list_hidden() {
    let mut app = app_with_procs(varied_procs());
    let mem = (x_of(&proc_row(&render_buffer(&mut app, 200, 50), 1), "MEM"), PROC_Y + 1);
    let try_all = |app: &mut App| {
      for (x, y) in [mem, (100, PROC_Y + 2)] {
        click(app, x, y);
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, x, y));
      }
      assert_eq!((app.cfg.proc_sort, app.proc_view.selected_pid()), (ProcSort::Cpu, None));
    };

    // hidden with `p`, then auto-hidden in a small window
    assert!(app.handle_key(key('p')).is_continue());
    render_buffer(&mut app, 200, 50);
    try_all(&mut app);
    assert!(app.handle_key(key('p')).is_continue());
    let mut app = with_procs(app, varied_procs());
    render_buffer(&mut app, 200, 50);
    render_buffer(&mut app, 60, 12);
    try_all(&mut app);

    // back on screen, the same click sorts again
    let mut app = with_procs(app, varied_procs());
    render_buffer(&mut app, 200, 50);
    click(&mut app, mem.0, mem.1);
    assert_eq!(app.cfg.proc_sort, ProcSort::Mem);
  }

  #[test]
  fn renders_every_box_at_any_size() {
    use Metric::*;
    let every_box = [Cluster(0), Cluster(1), Gpu, Ram, CpuPower, GpuPower, AnePower];
    let sizes = [
      (400, 120),
      (200, 50),
      (80, 24),
      (60, 15),
      (60, 12),
      (30, 8),
      (5, 3),
      (1, 1),
      (5, 0),
      (0, 5),
    ];
    for (width, height) in sizes {
      let bare = test_app_with(|m| {
        m.memory.swap_total = 0;
        m.fans.clear();
        m.sys_power = 0.0;
      });
      // with every sensor, without swap, fans and system power, and before the first sample
      for (i, mut app) in [test_app(), bare, bare_app()].into_iter().enumerate() {
        app.proc_view.set_procs(varied_procs());
        let buf = render_buffer(&mut app, width, height);
        let plan = app.layout(buf.area);
        let ctx = format!("app {i} at {width}x{height}");
        if i < 2 && height >= 8 {
          let boxes: Vec<Metric> = plan.boxes.iter().map(|(metric, _)| *metric).collect();
          assert_eq!(boxes, every_box, "{ctx}");
        }

        // every box drawn where the layout puts it: no title, summary, hint, graph or table cell
        // lands on a corner
        let boxes = plan.top.into_iter().chain(plan.proc).chain(plan.boxes.iter().map(|b| b.1));
        for r in boxes.filter(|r| r.width >= 2 && r.height >= 2) {
          let (right, bottom) = (r.right() - 1, r.bottom() - 1);
          let corners =
            [(r.x, r.y, "╭"), (right, r.y, "╮"), (r.x, bottom, "╰"), (right, bottom, "╯")];
          for (x, y, corner) in corners {
            assert_eq!(buf[(x, y)].symbol(), corner, "{ctx}: {r:?}");
          }
        }
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

  /// Top border of the metric box `metric` in a rendered frame.
  fn box_top(app: &App, buf: &Buffer, metric: Metric) -> String {
    let boxes = app.layout(buf.area).boxes;
    let (_, area) = boxes.into_iter().find(|(m, _)| *m == metric).expect("metric box");
    (area.left()..area.right()).map(|x| buf[(x, area.y)].symbol()).collect()
  }

  #[test]
  fn renders_three_cpu_tiers() {
    // M6 of issue #80: 6E + 4P + 2S
    let tier = |label: &str, cores| CpuTierInfo { label: label.into(), cores, freqs: vec![] };
    let soc = SocInfo {
      chip_name: "Apple M6".to_string(),
      memory_gb: 32,
      cpu_tiers: vec![tier("E", 6), tier("P", 4), tier("S", 2)],
      gpu_cores: 12,
      ..Default::default()
    };
    let mut app = App {
      kill: Kill::new(Box::new(FakeSys::default())),
      ..App::from_parts(soc, Config::default())
    };
    let cpu_tiers = vec![
      cpu_tier("E", 2940, 0.71, 0.71),
      cpu_tier("P", 2394, 0.25, 0.25),
      cpu_tier("S", 4776, 0.5, 0.5),
    ];
    app.update_metrics(Metrics { cpu_tiers, ..test_metrics() });

    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 0).starts_with("╭─ Apple M6 (6E+4P+2S+12GPU 32GB) ─"), "{}", row(&buf, 0));
    let screen = screen_text(&buf);
    for title in ["E-CPU 71% @ 2940 MHz", "P-CPU 25% @ 2394 MHz", "S-CPU 50% @ 4776 MHz"] {
      assert!(screen.contains(&format!("╭─ {title} ─")), "missing {title}");
    }
  }

  #[test]
  fn renders_original_titles() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    let screen = screen_text(&buf);
    for title in TITLES {
      assert!(screen.contains(&format!("╭─ {title} ─")), "missing {title}");
    }
    // the chip with its core counts left on the metrics box, the version right
    let version = format!("─ macmon v{} ─╮", env!("CARGO_PKG_VERSION"));
    let top = row(&buf, 0);
    assert!(top.starts_with("╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ─") && top.ends_with(&version));
    // whole-degree temperatures right on the CPU / GPU power boxes, none for ANE
    assert!(box_top(&app, &buf, Metric::CpuPower).ends_with("─ 45°C ─╮"));
    assert!(box_top(&app, &buf, Metric::GpuPower).ends_with("─ 40°C ─╮"));
    assert!(box_top(&app, &buf, Metric::AnePower).ends_with("───╮"));
    // the power summary on the bottom border of the metrics box
    let summary = "╰─ Power 6.60W (6.60, 6.60) | Fan 1200 RPM | Total 12.00W (12.00, 12.00) ─";
    assert!(row(&buf, PROC_Y - 1).starts_with(summary), "{}", row(&buf, PROC_Y - 1));
  }

  #[test]
  fn ram_title_keeps_swap_as_long_as_it_fits_and_never_cuts_a_number() {
    const GIB: f64 = (1u64 << 30) as f64;
    // 16.81 of 24 GB RAM in use (70.0 %) and 2.37 of 3 GB swap (79.0 %), or no swap
    let ram_app = |swap: bool| {
      test_app_with(|m| {
        m.memory = MemMetrics {
          ram_total: 24 << 30,
          ram_usage: (16.81 * GIB) as u64,
          swap_total: if swap { 3 << 30 } else { 0 },
          swap_usage: if swap { (2.37 * GIB) as u64 } else { 0 },
        }
      })
    };
    // (box width, title): each from the width where it fits whole (its text + 6 cells); the swap
    // part drops whole, then the title
    let swap = [
      (47, "RAM 16.81 GB (70.0%) · SWAP 2.37 / 3.0 GB"),
      (46, "RAM 70% · SWAP 79%"),
      (24, "RAM 70% · SWAP 79%"),
      (23, "RAM 70% SW 79%"),
      (20, "RAM 70% SW 79%"),
      (19, "RAM 70%"),
      (13, "RAM 70%"),
      (12, ""),
    ];
    let no_swap = [(26, "RAM 16.81 GB (70.0%)"), (25, "RAM 70%"), (13, "RAM 70%"), (12, "")];

    for (has_swap, cases) in [(true, &swap[..]), (false, &no_swap[..])] {
      let mut app = ram_app(has_swap);
      for &(width, title) in cases {
        // the clusters, GPU and RAM boxes `width` cells each, side by side
        let buf = render_buffer(&mut app, width * 4 + 2, 50);
        let title = if title.is_empty() { String::new() } else { format!("─ {title} ") };
        let dashes = "─".repeat(usize::from(width) - 2 - title.chars().count());
        let top = box_top(&app, &buf, Metric::Ram);
        assert_eq!(top, format!("╭{title}{dashes}╮"), "swap {has_swap} at {width}");
      }
    }
  }

  #[test]
  fn proc_table_renders_rows() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);

    // the count and the filter and sort hints on the title, the sort arrow by the sorted column
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 3 ─ / filter ─ s sort ─"), "{title}");
    let header = proc_row(&buf, 1);
    assert!(header.starts_with("│   PID NAME "), "{header}");
    assert!(header.ends_with(" CPU% ↓    MEM   POWER   GPU% │"), "{header}");

    // sorted by CPU, descending; numbers right-aligned, a dash for a missing power reading
    let words = |y| proc_row(&buf, y).split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
      words(2),
      ["│", "631", "WindowServer", "vlad", "25.0", "300M", "1.50W", "40.0", "│"]
    );
    assert_eq!(words(3), ["│", "2301", "Safari", "vlad", "12.0", "1.5G", "0.80W", "5.0", "│"]);
    assert_eq!(words(4), ["│", "1", "launchd", "root", "0.0", "20M", "-", "0.0", "│"]);
    assert!(proc_row(&buf, 2).ends_with("  25.0   300M   1.50W   40.0 │"), "{}", proc_row(&buf, 2));

    // load values on the gradient
    let cpu = x_of(&proc_row(&buf, 2), "25.0");
    assert_eq!(buf[(cpu, PROC_Y + 2)].fg, gradient(0.25));
  }

  #[test]
  fn filter_terms_show_their_totals_above_the_processes() {
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    for c in "/a, w".chars() {
      assert!(press(&mut app, KeyCode::Char(c)).is_continue());
    }
    let buf = render_buffer(&mut app, 200, 50);
    let words = |y| proc_row(&buf, y).split_whitespace().map(str::to_string).collect::<Vec<_>>();

    // sorted by CPU like the processes; `a`: Safari and launchd, whose missing power reading
    // makes the sum a lower bound
    assert_eq!(words(2), ["│", "×1", "w", "25.0", "300M", "1.50W", "40.0", "│"]);
    assert_eq!(words(3), ["│", "×2", "a", "12.0", "1.5G", "≥0.80W", "5.0", "│"]);
    assert!(proc_row(&buf, 4).starts_with("│ ────"), "{}", proc_row(&buf, 4));
    assert_eq!(words(5)[1..3], ["631", "WindowServer"]);
    let total = x_of(&proc_row(&buf, 3), "×2");
    assert!(buf[(total, PROC_Y + 3)].modifier.contains(ratatui::style::Modifier::BOLD));

    // a click on the first process below the totals selects it
    click(&mut app, 10, PROC_Y + 5);
    assert_eq!(app.proc_view.selected().map(|p| p.pid), Some(631));
  }

  #[test]
  fn selected_process_and_its_path_on_the_bottom_border() {
    let bottom = |app: &mut App, width| row(&render_buffer(app, width, 50), 49);
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    // nothing selected: a note on POWER, as launchd has no reading next to processes with one
    assert!(bottom(&mut app, 200).starts_with("╰─ POWER: own processes only ─"));

    // the PID and the full path, cut from the left to the room the key hints leave
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(bottom(&mut app, 200).starts_with(&format!("╰─ 631 {WINDOW_SERVER} ─")));
    let cut = format!("╰─ 631 …{} ─ q quit |", &WINDOW_SERVER[WINDOW_SERVER.len() - 47..]);
    assert!(bottom(&mut app, 120).starts_with(&cut), "{}", bottom(&mut app, 120));

    // without a readable path: the name
    let mut app = app_with_procs(test_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(bottom(&mut app, 200).starts_with("╰─ 1 launchd ─"));
  }

  #[test]
  fn k_asks_and_y_kills_the_process_asked_about() {
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    let bottom = |app: &mut App| row(&render_buffer(app, 200, 50), 49);

    // nothing selected: nothing to ask about
    assert!(app.handle_key(key('k')).is_continue());
    assert!(!app.kill.asking());

    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    let line = bottom(&mut app);
    assert!(line.starts_with("╰─ Kill 631 WindowServer? y/n ─"), "{line}");
    assert!(line.ends_with("─ y kill | any key cancel ─╯"), "{line}");

    // a new sample without WindowServer drops the selection: `y` still kills what it asked about
    let mut procs = varied_procs();
    procs.remove(1);
    put_procs(&mut app, procs);
    assert_eq!(app.proc_view.selected_pid(), None);
    assert!(app.handle_key(key('y')).is_continue());
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
    let line = bottom(&mut app);
    assert!(line.starts_with("╰─ SIGTERM sent to 631 WindowServer ─"), "{line}");
    assert!(line.ends_with(" q quit | ? help | p procs | v graph | r scaled | -/+ 1000ms ─╯"));

    // ticks follow it until it exits
    let msec = RwLock::new(TUI_MIN_MS);
    fake.exit(631);
    assert!(app.handle_event(Event::Tick, &msec).is_continue());
    assert!(bottom(&mut app).starts_with("╰─ 631 WindowServer exited ─"));
  }

  /// Name of `long_name_app`'s selected process.
  const LONG_NAME: &str = "com.apple.WebKit.WebContent";

  /// `app_with_procs` of `varied_procs` with Safari renamed to `LONG_NAME` and selected, killing
  /// through a fake.
  fn long_name_app() -> (App, FakeSys) {
    let mut procs = varied_procs();
    procs[2].name = LONG_NAME.to_string();
    let (mut app, fake) = faking_kill(app_with_procs(procs)); // [631, 2301, 1]
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    (app, fake)
  }

  #[test]
  fn tracking_lines_stay_readable_in_a_narrow_window() {
    let bottom = |app: &mut App, width| row(&render_buffer(app, width, 50), 49);
    let (mut app, fake) = long_name_app();
    assert!(app.handle_key(key('k')).is_continue());
    assert!(app.handle_key(key('y')).is_continue());

    // hints drop from the end until the name gets 16 cells, then the name takes the room left
    assert_eq!(
      bottom(&mut app, 80),
      "╰─ SIGTERM sent to 2301 com.apple.WebKit.WebCont… ─ q quit | ? help | p procs ─╯"
    );
    app.kill.tick(Instant::now() + Duration::from_secs(3));
    let line = bottom(&mut app, 200);
    assert!(line.starts_with(&format!("╰─ 2301 {LONG_NAME} still running · k force kill ─")));
    assert_eq!(
      bottom(&mut app, 80),
      "╰─ 2301 com.apple.WebKit.WebC… still running · k force kill ─ q quit | ? help ─╯"
    );
    // no hint left: the name is cut further
    assert_eq!(bottom(&mut app, 46), "╰─ 2301 com.a… still running · k force kill ─╯");

    // the force-kill prompt answers with its own hint
    assert!(app.handle_key(key('k')).is_continue());
    let line = bottom(&mut app, 200);
    assert!(line.starts_with(&format!("╰─ Force kill 2301 {LONG_NAME}? y/n ─")), "{line}");
    assert!(line.ends_with("─ y force kill | any key cancel ─╯"), "{line}");
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert_eq!(fake.sent(), [(2301, libc::SIGTERM)]);
  }

  #[test]
  fn prompts_and_messages_stay_readable_in_a_narrow_window() {
    let bottom = |app: &mut App, width| row(&render_buffer(app, width, 50), 49);
    let (mut app, fake) = long_name_app();
    assert!(app.handle_key(key('k')).is_continue());

    // the whole name with both hints, then the hints left out and the name cut
    let line = bottom(&mut app, 80);
    assert!(line.starts_with(&format!("╰─ Kill 2301 {LONG_NAME}? y/n ─")), "{line}");
    assert!(line.ends_with("─ y kill | any key cancel ─╯"), "{line}");
    assert_eq!(bottom(&mut app, 40), "╰─ Kill 2301 com.apple.WebKit.W…? y/n ─╯");
    assert!(app.kill.asking());
    assert!(press(&mut app, KeyCode::Esc).is_continue());

    fake.fail(2301, libc::EPERM);
    assert!(app.handle_key(key('k')).is_continue());
    assert_eq!(
      bottom(&mut app, 80),
      "╰─ Not permitted to kill 2301 com.apple.WebKit.W… ─ q quit | ? help | p procs ─╯"
    );
    // a line without a name gets its whole width: `q quit` would leave it a cell short
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    assert_eq!(bottom(&mut app, 40), "╰─ Won't kill launchd (pid 1) ─────────╯");
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn the_signalled_process_shows_while_it_or_nothing_is_selected() {
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    let bottom = |app: &mut App| row(&render_buffer(app, 200, 50), 49);
    let sent = "╰─ SIGTERM sent to 631 WindowServer ─";
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    assert!(app.handle_key(key('y')).is_continue());
    assert!(bottom(&mut app).starts_with(sent));

    // another process selected: its path, as before; nothing selected: the signalled process
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let safari = "╰─ 2301 /Applications/Safari.app/Contents/MacOS/Safari ─";
    assert!(bottom(&mut app).starts_with(safari), "{}", bottom(&mut app));
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(bottom(&mut app).starts_with(sent));

    // a prompt and a message show whatever is selected
    for _ in 0..2 {
      assert!(press(&mut app, KeyCode::Down).is_continue());
    }
    assert!(app.handle_key(key('k')).is_continue());
    assert!(bottom(&mut app).starts_with("╰─ Kill 2301 Safari? y/n ─"));
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    assert!(bottom(&mut app).starts_with("╰─ Won't kill launchd (pid 1) ─"));
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn k_on_a_row_whose_pid_was_reused_since_the_sample_says_exited() {
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    assert!(press(&mut app, KeyCode::Down).is_continue());
    // the row still names the process sampled before
    fake.restart(631);
    assert!(app.handle_key(key('k')).is_continue());
    assert!(!app.kill.asking());
    let line = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(line.starts_with("╰─ 631 WindowServer exited ─"), "{line}");
    assert!(app.handle_key(key('y')).is_continue());
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn k_does_nothing_while_the_list_is_hidden_or_the_help_is_open() {
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    assert!(press(&mut app, KeyCode::Down).is_continue());

    assert!(app.handle_key(key('?')).is_continue());
    for c in ['k', 'y'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(!app.kill.asking() && app.help.is_some());
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert_eq!(app.proc_view.selected_pid(), Some(631));

    // hidden with `p`, then in a window too small for the list
    assert!(app.handle_key(key('p')).is_continue());
    render_buffer(&mut app, 200, 50);
    for c in ['k', 'y'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(app.handle_key(key('p')).is_continue());
    let mut app = with_procs(app, varied_procs());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    render_buffer(&mut app, 60, 12);
    for c in ['k', 'y'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(!app.kill.asking());
    assert_eq!(fake.calls(), []);
  }

  #[test]
  fn ticks_follow_the_signalled_process_while_the_list_is_hidden() {
    let msec = RwLock::new(TUI_MIN_MS);
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    let note = |app: &App| app.kill.note(None).map(|note| note.to_string());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    assert!(app.handle_key(key('y')).is_continue());

    render_buffer(&mut app, 60, 12);
    assert!(!app.procs_visible());
    assert!(app.handle_event(Event::Tick, &msec).is_continue());
    assert_eq!(note(&app).as_deref(), Some("SIGTERM sent to 631 WindowServer"));
    fake.exit(631);
    assert!(app.handle_event(Event::Tick, &msec).is_continue());
    assert_eq!(note(&app).as_deref(), Some("631 WindowServer exited"));

    // on screen again: the message is there
    let mut app = with_procs(app, varied_procs());
    let line = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(line.starts_with("╰─ 631 WindowServer exited ─"), "{line}");
    assert_eq!(fake.sent(), [(631, libc::SIGTERM)]);
  }

  #[test]
  fn clicks_the_wheel_focus_loss_and_hiding_close_the_kill_prompt() {
    let msec = RwLock::new(TUI_MIN_MS);
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs())); // [631, 2301, 1]
    render_buffer(&mut app, 200, 50);
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let ask = |app: &mut App| {
      assert!(app.handle_key(key('k')).is_continue());
      assert!(app.kill.asking());
    };

    // a click on another row only closes it
    ask(&mut app);
    click(&mut app, 10, PROC_Y + 3);
    assert!(!app.kill.asking());
    assert_eq!(app.proc_view.selected_pid(), Some(631));
    assert!(app.handle_key(key('y')).is_continue());

    // the wheel too, without moving the selection
    ask(&mut app);
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 10));
    assert!(!app.kill.asking());
    assert_eq!(app.proc_view.selected_pid(), Some(631));
    assert!(app.handle_key(key('y')).is_continue());

    ask(&mut app);
    assert!(app.handle_event(super::Event::FocusLost, &msec).is_continue());
    assert!(!app.kill.asking());
    assert!(app.handle_key(key('y')).is_continue());

    // the window too small for the list
    ask(&mut app);
    render_buffer(&mut app, 60, 12);
    assert!(!app.kill.asking());
    assert!(app.handle_key(key('y')).is_continue());
    assert_eq!(fake.sent(), []);
  }

  #[test]
  fn k_typed_into_the_filter_stays_text() {
    let (mut app, fake) = faking_kill(app_with_procs(varied_procs()));
    assert!(press(&mut app, KeyCode::Down).is_continue());
    for c in "/k".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert_eq!((app.proc_view.filter(), app.kill.asking()), ("k", false));
    assert!(app.handle_key(key('y')).is_continue());
    assert_eq!(fake.calls(), []);
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
    assert!(app.procs_visible());
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
