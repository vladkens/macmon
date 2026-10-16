//! Terminal user interface.

mod boxes;
mod help;
mod info;
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

use info::Info;
use kill::Kill;
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
  /// The sampler failed: the app ends with this error.
  Failed(String),
  /// A process sample with the number of the panel showing it was started in.
  Procs {
    showing: u64,
    procs: Vec<ProcInfo>,
  },
  Key(KeyEvent),
  Mouse(MouseEvent),
  /// The terminal window lost focus.
  FocusLost,
  /// A resize: a redraw, so the mouse targets follow the new layout at once.
  Resize,
}

/// App event of a terminal event: keys, left clicks and the wheel, focus loss, and a resize.
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
    event::Event::Resize(..) => Some(Event::Resize),
    _ => None,
  }
}

/// Window of the first process sample after the panel shows up, so the list fills in quickly.
const PROCS_WARMUP: Duration = Duration::from_millis(TUI_MIN_MS as u64);

/// Sends input events; stops once the app is gone. Nothing redraws on a timer: every frame
/// follows an event (a sample, an input, a resize), as nothing on screen changes without one.
fn run_inputs_thread(tx: mpsc::Sender<Event>) {
  thread::spawn(move || {
    loop {
      if let Some(event) = input_event(event::read().unwrap())
        && tx.send(event).is_err()
      {
        return;
      }
    }
  });
}

/// Sends metrics: the first sample after 100 ms, then one per interval; stops once the app is
/// gone or after an error.
fn run_sampler_thread(tx: mpsc::Sender<Event>, msec: Arc<RwLock<u32>>) {
  thread::spawn(move || {
    let sample = || -> WithError<()> {
      let mut sampler = Sampler::new()?;
      let mut window = 100;

      loop {
        let metrics = sampler.get_metrics(window)?;
        if tx.send(Event::Update(Box::new(metrics))).is_err() {
          return Ok(());
        }
        window = msec.read().map_or(TUI_MIN_MS, |x| *x).max(TUI_MIN_MS);
      }
    };
    if let Err(err) = sample() {
      let _ = tx.send(Event::Failed(err.to_string()));
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
  /// The info overlay (`i`) of the selected process, while it is open.
  info: Option<Info>,
  /// The kill popup (`k`) for the selected process.
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
  /// filter input and closes the overlays, so keys don't go to something that isn't on screen.
  fn set_procs_visible(&mut self, visible: bool) {
    self.procs_shown.set(visible);
    if !visible {
      self.proc_view.clear();
      self.info = None;
      self.kill.close();
    }
  }

  /// Stores a process sample started in the panel's `showing`. Only one of the current showing
  /// gets in: one still in flight when the panel got hidden is dropped, also when the panel is
  /// back by now, so a re-shown panel reads "collecting…" until its own first sample. In a VM no
  /// process has a power reading: process energy doesn't move there.
  fn update_procs(&mut self, showing: u64, mut procs: Vec<ProcInfo>) {
    if self.soc.vm {
      procs.iter_mut().for_each(|proc| proc.power_w = None);
    }
    if self.procs_shown.showing() == Some(showing) {
      self.proc_view.set_procs(procs);
      if let Some(info) = &mut self.info
        && !self.proc_view.paused
      {
        info.update_proc(self.proc_view.procs().unwrap_or_default());
        info.refresh(&*self.kill.sys);
      }
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
      // switching away is done typing: keys back as shortcuts, the filter stays
      Event::FocusLost => {
        self.proc_view.end_typing();
        self.info.iter_mut().for_each(|info| info.typing = false);
        return ControlFlow::Continue(());
      }
      Event::Resize => return ControlFlow::Continue(()),
      Event::Failed(_) => return ControlFlow::Break(()),
    };

    *msec.write().unwrap() = self.cfg.interval();
    flow
  }

  /// Applies a key press to the app state. Returns `Break` when the app should quit. The help
  /// overlay takes every key while it is open, then the info overlay (`?` opens the help over it)
  /// and the kill popup; then keys of the process panel (only while it is on screen) take
  /// precedence, and while a filter is typed every key except Ctrl-C goes to it.
  fn handle_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
      return ControlFlow::Break(());
    }
    // `copied` shows until the next key
    self.proc_view.copied = false;

    if let Some(scroll) = self.help {
      self.help = match key.code {
        KeyCode::Esc | KeyCode::Char('?' | 'q') => None,
        KeyCode::Up => Some(scroll.saturating_sub(1)),
        KeyCode::Down => Some(scroll + 1),
        _ => Some(scroll),
      };
      return ControlFlow::Continue(());
    }

    if let Some(mut info) = self.info.take() {
      // the help opens over it, unless `?` is typed into its filter; the overlay sees the key
      // too (it clears `copied`)
      if key.code == KeyCode::Char(' ') && !info.typing {
        self.proc_view.toggle_pause();
      }
      let help = key.code == KeyCode::Char('?') && !info.typing;
      self.help = help.then_some(0);
      self.info = info.handle_key(key, &*self.kill.sys).then_some(info);
      return ControlFlow::Continue(());
    }
    if self.kill.is_open() {
      self.kill.handle_key(key);
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
      // the last `Char` arm: it takes every character left while the list is on screen
      KeyCode::Char(c) if self.procs_visible() => match (c, self.proc_view.selected()) {
        ('i', Some(proc)) if !self.proc_view.exited(proc) => {
          self.info = Some(Info::new(proc, &*self.kill.sys))
        }
        ('k', Some(proc)) if !self.proc_view.exited(proc) => self.kill.open(proc),
        ('e', Some(proc)) if !proc.path.is_empty() => self.kill.sys.reveal(&proc.path),
        ('c', Some(proc)) if !proc.path.is_empty() => {
          self.kill.sys.copy(&proc.path);
          self.proc_view.copied = true;
        }
        _ => {}
      },
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
  /// mouse is captured (events still on the way when capture turns off are dropped). While an
  /// overlay is open, they go to it; under the help to nothing.
  fn handle_mouse(&mut self, mouse: MouseEvent) {
    if self.help.is_some() {
      return;
    }
    self.proc_view.copied = false;
    if let Some(info) = &mut self.info {
      info.handle_mouse(mouse);
    } else if self.kill.is_open() {
      self.kill.handle_mouse(mouse);
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
    let (procs, clusters) = (self.cfg.show_procs, self.clusters.items.len());
    compute_layout(area, procs, clusters, !self.soc.vm)
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
      self.info.iter_mut().for_each(|info| info.render(f, r, self.proc_view.paused));
      self.kill.render(f, r);
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
    run_inputs_thread(tx.clone());
    self.three_level_bars = theme::detect_three_level_bars();

    let mut mouse = false;
    loop {
      term.draw(|f| self.render(f))?;
      mouse = set_mouse_capture(&mut stdout(), mouse, self.wants_mouse())?;
      let event = rx.recv()?;
      if let Event::Failed(err) = event {
        return Err(err.into());
      }
      if self.handle_event(event, &msec).is_break() {
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
  use std::time::Duration;

  use macmon::{CpuTierInfo, CpuTierMetrics, FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::{Buffer, CellDiffOption};
  use ratatui::crossterm::ExecutableCommand;
  use ratatui::crossterm::event::{
    EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
  };
  use ratatui::layout::Margin;
  use ratatui::style::Modifier;

  use super::kill::tests::FakeSys;
  use super::layout::Metric;
  use super::theme::{self, gradient};
  use super::{App, Event, restore_term_once, run_procs_thread};
  use crate::config::{Config, ProcSort, RatioMode, TUI_MAX_MS, TUI_MIN_MS, TempConfig, ViewType};
  use crate::fds::{Kind, OpenFile};
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

  /// App for `test_soc` with a few samples of `test_metrics` changed by `edit`.
  fn test_app_with(edit: impl Fn(&mut Metrics)) -> App {
    let mut app = App::from_parts(test_soc(), Config::default());
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
      cpu_time_ns: 0,
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
    let proc = |pid: i32, name: &str, cpu, secs: u64, mem_mb: u64, power_w, gpu_pct| ProcInfo {
      pid,
      name: name.to_string(),
      path: path(name),
      user: if pid < 100 { "root" } else { "vlad" }.to_string(),
      cpu_pct: cpu,
      cpu_time_ns: secs * 1_000_000_000,
      mem_bytes: mem_mb << 20,
      power_w,
      gpu_pct,
      started: (pid >= 100).then_some((pid as u64, 0)),
    };
    vec![
      proc(1, "launchd", 0.0, 3 * 3600 + 25 * 60 + 7, 20, None, 0.0),
      proc(631, "WindowServer", 25.0, 2 * 86_400 + 5 * 3600, 300, Some(1.5), 40.0),
      proc(2301, "Safari", 12.0, 754, 1536, Some(0.8), 5.0),
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
    let mut app = App::default();
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
    let hints = " view | raw  | -/+  1250ms ─╯";
    assert!(row(&buf, 49).ends_with(hints), "{}", row(&buf, 49));

    assert!(app.handle_key(key('v')).is_continue());
    assert_eq!(app.cfg.view_type, ViewType::Graph);
    assert_eq!(file.saved()["view_type"], "Sparkline");
  }

  #[test]
  fn the_footer_keeps_its_width_through_the_modes_and_intervals() {
    let mut app = test_app();
    let width = |app: &App| {
      app.footer_hints().iter().flatten().map(ratatui::text::Span::width).sum::<usize>()
    };
    let start = width(&app);
    app.cfg.ratio_mode = RatioMode::Active;
    assert_eq!(width(&app), start);
    for interval in [TUI_MIN_MS, TUI_MAX_MS] {
      app.cfg.set_run_interval(interval);
      assert_eq!(width(&app), start, "{interval}");
    }
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
    assert!(row(&buf, 11).contains(" ? | quit | view "), "{}", row(&buf, 11));
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
  fn disabling_a_saved_filter_does_not_report_no_match_for_an_empty_sample() {
    let mut app = app_with_procs(varied_procs());
    for c in "/nothing-matches".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    assert!(screen_text(&render_buffer(&mut app, 200, 50)).contains("no process matches"));
    assert!(app.handle_key(key('f')).is_continue());
    app.proc_view.set_procs(vec![]);
    let screen = screen_text(&render_buffer(&mut app, 200, 50));
    assert!(!screen.contains("no process matches"));
    assert!(screen.contains("proc 0 ─ /nothing-matches"));
    assert!(app.handle_key(key('f')).is_continue());
    assert!(screen_text(&render_buffer(&mut app, 200, 50)).contains("no process matches"));
  }

  #[test]
  fn typing_filter_ignores_global_keys() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());

    // only the filter keys on the bottom border: the arrows end the input as Enter does
    let footer = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(footer.ends_with("─ Enter keep | Esc clear ─╯"), "{footer}");

    for c in ['q', 'v', 'r', 'p', '+', '-', 's', '?', 'c'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?} while typing");
    }
    assert_eq!(app.proc_view.filter(), "qvrp+-s?c");
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
      ("TIME", Time, true, [631, 1, 2301], [2301, 1, 631]),
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
  fn a_scrollbar_on_the_right_border_shows_where_the_rows_on_screen_are() {
    // the right border from the header to the corner: beside the 27 rows of 100 processes `↑`, a
    // track of 25 cells with a thumb of 7 (27 / 100 of it), `↓`
    let mut app = hundred_procs_app();
    let border = |app: &mut App| {
      let buf = render_buffer(app, 200, 50);
      (PROC_Y + 1..50).map(|y| buf[(199, y)].symbol().to_string()).collect::<String>()
    };
    let bar = |above: usize, below: usize| {
      format!("│↑{}{}{}↓╯", "│".repeat(above), "┃".repeat(7), "│".repeat(below))
    };
    assert_eq!(border(&mut app), bar(0, 18));
    // scrolled to row 36 of the 73 it can start at, then to the end
    for _ in 0..12 {
      app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 10));
    }
    assert_eq!(border(&mut app), bar(9, 9));
    assert!(press(&mut app, KeyCode::End).is_continue());
    assert_eq!(border(&mut app), bar(18, 0));
    // the arrows in the border color, the thumb in the text color
    let buf = render_buffer(&mut app, 200, 50);
    let fg = |y: u16| buf[(199, PROC_Y + y)].fg;
    assert_eq!([fg(2), fg(27), fg(28)], [theme::BORDER, theme::TEXT, theme::BORDER]);

    // the 11 rows of `proc9` fit: the plain border
    for c in "/proc9".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert_eq!(border(&mut app), format!("{}╯", "│".repeat(28)));

    // in any window the box keeps its corners
    let widths = [1, 2, 3, 5, 10, 20, 40, 60, 80, 100, 119];
    for (width, height) in widths.into_iter().flat_map(|w| (1..40).map(move |h| (w, h))) {
      let mut app = hundred_procs_app();
      let buf = render_buffer(&mut app, width, height);
      let Some(r) = app.layout(buf.area).proc.filter(|r| r.width >= 2 && r.height >= 2) else {
        continue;
      };
      let (right, bottom) = (r.right() - 1, r.bottom() - 1);
      let corners = [(right, r.y, "╮"), (right, bottom, "╯")];
      for (x, y, corner) in corners {
        assert_eq!(buf[(x, y)].symbol(), corner, "{width}x{height}");
      }
    }
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
    let every_box = [Cluster(0), Cluster(1), Gpu, Ram, CpuPower, GpuPower, AnePower, TotalPower];
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
      for (i, mut app) in [test_app(), bare, App::default()].into_iter().enumerate() {
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
  const TITLES: [&str; 8] = [
    "E-CPU 42% @ 1800 MHz",
    "P-CPU 77% @ 3200 MHz",
    "GPU 23% @ 1400 MHz",
    "RAM 20.00 GB (55.6%) · SWAP 1.00 / 2.0 GB",
    "CPU 4.50W (4.50, 4.50)",
    "GPU 2.00W (2.00, 2.00)",
    "ANE 0.10W (0.10, 0.10)",
    "Total 12.00W (12.00, 12.00)",
  ];

  /// Top border of the metric box `metric` in a rendered frame.
  fn box_top(app: &App, buf: &Buffer, metric: Metric) -> String {
    box_row(app, buf, metric, false)
  }

  /// Top or bottom border of the metric box `metric` in a rendered frame.
  fn box_row(app: &App, buf: &Buffer, metric: Metric, bottom: bool) -> String {
    let boxes = app.layout(buf.area).boxes;
    let (_, area) = boxes.into_iter().find(|(m, _)| *m == metric).expect("metric box");
    let y = if bottom { area.bottom() - 1 } else { area.y };
    (area.left()..area.right()).map(|x| buf[(x, y)].symbol()).collect()
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
    let mut app = App::from_parts(soc, Config::default());
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
  fn renders_a_single_cpu_tier_without_frequencies() {
    // a macOS VM: one perflevel, no frequency tables, no GPU cores
    let tier = CpuTierInfo { label: "_".into(), cores: 4, freqs: vec![] };
    let soc = SocInfo {
      chip_name: "Apple M2 (Virtual)".to_string(),
      memory_gb: 8,
      cpu_tiers: vec![tier],
      vm: true,
      ..Default::default()
    };
    let mut app = App::from_parts(soc, Config::default());
    let metrics = Metrics { cpu_tiers: vec![cpu_tier("_", 0, 0.63, 0.63)], ..Default::default() };
    app.update_metrics(Metrics { memory: test_metrics().memory, ..metrics });
    // own processes have an energy counter, but it does not move in a VM; launchd has none
    let app = &mut with_procs(app, varied_procs());

    let buf = render_buffer(app, 200, 50);
    assert!(row(&buf, 0).starts_with("╭─ Apple M2 (Virtual) (4CPU 8GB) ─"), "{}", row(&buf, 0));
    let screen = screen_text(&buf);
    for title in ["CPU 63% ─", "GPU 0% ─"] {
      assert!(screen.contains(&format!("╭─ {title}")), "missing {title}");
    }
    assert!(!screen.contains("MHz") && !screen.contains("_-CPU"));

    // no power row: CPU, GPU and RAM take the whole metrics box, the process box stays
    let boxes = app.layout(buf.area).boxes;
    let rows = boxes.iter().map(|(m, r)| (*m, r.y, r.height)).collect::<Vec<_>>();
    assert_eq!(rows, [(Metric::Cluster(0), 1, 18), (Metric::Gpu, 1, 18), (Metric::Ram, 1, 18)]);
    assert!(!screen.contains("0.00W") && !screen.contains("ANE"), "{screen}");
    assert!(proc_row(&buf, 0).starts_with("╭─ proc 3 "), "{}", proc_row(&buf, 0));
    // POWER reads `-` for every process, without the note on own processes
    for y in [2, 3, 4] {
      let words = proc_row(&buf, y).split_whitespace().map(str::to_string).collect::<Vec<_>>();
      assert_eq!(words[words.len() - 2], "-", "{words:?}");
    }
    assert!(!row(&buf, 49).contains("POWER"), "{}", row(&buf, 49));
  }

  #[test]
  fn only_a_vm_drops_the_power_row_and_process_power() {
    // processes sampled before the first metrics sample: the chip already tells a VM
    for (vm, boxes, power, note) in [(false, 8, "1.50W", true), (true, 4, "-", false)] {
      let soc = SocInfo { vm, ..test_soc() };
      let app = &mut with_procs(App::from_parts(soc, Config::default()), varied_procs());
      let buf = render_buffer(app, 200, 50);
      assert_eq!(app.layout(buf.area).boxes.len(), boxes, "vm {vm}");
      let words = proc_row(&buf, 2).split_whitespace().map(str::to_string).collect::<Vec<_>>();
      assert_eq!((words[2].as_str(), words[8].as_str()), ("WindowServer", power));
      assert_eq!(row(&buf, 49).contains("POWER: own processes only"), note);
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
    // nothing right on the total power box; no summary on the bottom border of the metrics box
    assert!(box_top(&app, &buf, Metric::TotalPower).ends_with("───╮"));
    assert_eq!(row(&buf, PROC_Y - 1), format!("╰{}╯", "─".repeat(198)));
  }

  #[test]
  fn fans_on_the_bottom_border_of_the_total_power_box() {
    let cases: [(&[u32], &str); 3] = [
      (&[1200], "─ 1200 rpm ─╯"),
      (&[1200, 1300], "─ 1200/1300 rpm ─╯"),
      (&[], "─────────────────────────╯"),
    ];
    for (rpms, end) in cases {
      let fan = |&rpm: &u32| FanMetric { name: "fan".to_string(), rpm, max_rpm: None };
      let mut app = test_app_with(|m| m.fans = rpms.iter().map(fan).collect());
      let buf = render_buffer(&mut app, 200, 50);
      let bottom = box_row(&app, &buf, Metric::TotalPower, true);
      assert!(bottom.starts_with("╰───") && bottom.ends_with(end), "{bottom}");
    }
  }

  #[test]
  fn total_power_falls_back_to_the_soc_sum_without_the_system_sensor() {
    // without the system power sensor and fans: CPU + GPU + ANE, nothing on the right
    let mut app = test_app_with(|m| {
      m.sys_power = 0.0;
      m.fans.clear();
    });
    let buf = render_buffer(&mut app, 200, 50);
    let top = box_top(&app, &buf, Metric::TotalPower);
    assert!(top.starts_with("╭─ Power 6.60W (6.60, 6.60) ───") && top.ends_with("───╮"), "{top}");

    // a sensor that stops reading falls back with its first zero sample
    let mut app = test_app();
    app.update_metrics(Metrics { sys_power: 0.0, ..test_metrics() });
    let buf = render_buffer(&mut app, 200, 50);
    let top = box_top(&app, &buf, Metric::TotalPower);
    assert!(top.starts_with("╭─ Power 6.60W (6.60, 6.60) ───"), "{top}");
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

    // the count and the filter and sort hints on the title (no `kill` without a selection), the
    // sort arrow by the sorted column
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 3 ─ / filter ─ sort ───"), "{title}");
    let header = proc_row(&buf, 1);
    assert!(header.starts_with("│   PID NAME "), "{header}");
    assert!(header.ends_with(" TIME CPU% ↓    MEM   GPU%   POWER │"), "{header}");

    // sorted by CPU, descending; numbers right-aligned, a dash for a missing power reading
    let words = |y| proc_row(&buf, y).split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
      words(2),
      ["│", "631", "WindowServer", "vlad", "2d05h", "25.0", "300M", "40.0", "1.50W", "│"]
    );
    let safari = ["│", "2301", "Safari", "vlad", "12:34.00", "12.0", "1.5G", "5.0", "0.80W", "│"];
    assert_eq!(words(3), safari);
    let launchd = ["│", "1", "launchd", "root", "3:25:07", "0.0", "20M", "0.0", "-", "│"];
    assert_eq!(words(4), launchd);
    let window_server = proc_row(&buf, 2);
    assert!(window_server.ends_with("   2d05h   25.0   300M   40.0   1.50W │"), "{window_server}");

    // load values on the gradient
    let cpu = x_of(&proc_row(&buf, 2), "25.0");
    assert_eq!(buf[(cpu, PROC_Y + 2)].fg, gradient(0.25));
  }

  #[test]
  fn info_finder_kill_are_on_the_process_title_with_a_selection_while_not_typing() {
    let title = |app: &mut App, width| proc_row(&render_buffer(app, width, 50), 0);
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    assert!(!title(&mut app, 200).contains("kill"), "no selection");
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let wide = title(&mut app, 200);
    assert!(wide.starts_with("╭─ proc 3 ─ / filter ─ sort ───"), "{wide}");
    assert!(wide.ends_with("─ info | finder | copy | kill ─╮"), "{wide}");
    // the count bold and plain, the hint keys cyan, their labels faint, the separators dim
    let buf = render_buffer(&mut app, 200, 50);
    let style = |text| {
      let cell = &buf[(x_of(&wide, text), PROC_Y)];
      (cell.fg, cell.modifier)
    };
    let (bold, plain) = ((theme::TEXT, Modifier::BOLD), (theme::TEXT, Modifier::empty()));
    let (cyan, faint) = ((theme::KEY, Modifier::empty()), (theme::TEXT, Modifier::DIM));
    let sep = (theme::DIM, Modifier::empty());
    let cells = ["proc", " 3", "/ ", " filter", "sort", "ort", "info", "nfo", " | "];
    assert_eq!(cells.map(style), [bold, plain, cyan, faint, cyan, faint, cyan, faint, sep]);
    // `e` inside `finder`
    let cells = ["finder", "er | ", "r | ", "| copy", "copy", "opy", "| kill", "kill", "ill"];
    assert_eq!(cells.map(style), [faint, cyan, faint, sep, cyan, faint, sep, cyan, faint]);
    // on a narrow box they go first, all together
    let narrow = "╭─ proc 3 ─ / filter ─ sort ─ info | finder | copy | kill ─╮";
    assert_eq!(title(&mut app, 60), narrow);
    assert_eq!(title(&mut app, 59), format!("╭─ proc 3 ─ / filter ─ sort {}╮", "─".repeat(30)));

    // typing: keys are text, no hints; then a set filter with the selection in it
    for c in "/saf".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    let typing = title(&mut app, 200);
    assert!(typing.starts_with("╭─ proc 1/3 ─ /saf█ ───") && !typing.contains("sort"), "{typing}");
    assert!(!typing.contains("kill"), "{typing}");
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let filtered = title(&mut app, 200);
    assert!(
      filtered.starts_with("╭─ proc 1/3 ─ /saf ─ sort ───")
        && filtered.ends_with(" info | finder | copy | kill ─╮")
    );
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
    assert_eq!(words(2), ["│", "×1", "w", "2d05h", "25.0", "300M", "40.0", "1.50W", "│"]);
    assert_eq!(words(3), ["│", "×2", "a", "3:37:41", "12.0", "1.5G", "5.0", "≥0.80W", "│"]);
    assert!(proc_row(&buf, 4).starts_with("│ ────"), "{}", proc_row(&buf, 4));
    assert_eq!(words(5)[1..3], ["631", "WindowServer"]);
    let total = x_of(&proc_row(&buf, 3), "×2");
    assert!(buf[(total, PROC_Y + 3)].modifier.contains(ratatui::style::Modifier::BOLD));

    // a click on the first process below the totals selects it
    click(&mut app, 10, PROC_Y + 5);
    assert_eq!(app.proc_view.selected().map(|p| p.pid), Some(631));
  }

  #[test]
  fn the_bottom_border_has_the_power_note_and_not_the_selected_process() {
    let bottom = |app: &mut App, width| row(&render_buffer(app, width, 50), 49);
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    // a note on POWER, as launchd has no reading next to processes with one
    assert!(bottom(&mut app, 200).starts_with("╰─ POWER: own processes only ─"));
    // with a selection too: no PID or path
    assert!(press(&mut app, KeyCode::Down).is_continue());
    let shown = bottom(&mut app, 200);
    assert!(
      shown.starts_with("╰─ POWER: own processes only ─") && !shown.contains("631"),
      "{shown}"
    );
  }

  /// `app_with_procs` of `varied_procs` with WindowServer selected, killing through a fake with
  /// their processes.
  fn kill_app() -> (App, FakeSys) {
    let mut app = app_with_procs(varied_procs()); // [631, 2301, 1]
    let (kill, fake) = FakeSys::kill(&[1, 631, 2301]);
    app.kill = kill;
    assert!(press(&mut app, KeyCode::Down).is_continue());
    (app, fake)
  }

  #[test]
  fn k_opens_a_popup_and_t_kills_its_process() {
    let (mut app, fake) = kill_app();
    assert!(app.handle_key(key('k')).is_continue());
    // centered over the process box
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 31).contains("╭─ Kill WindowServer? ──"), "{}", row(&buf, 31));
    assert!(row(&buf, 33).contains("│  pid 631 · vlad "), "{}", row(&buf, 33));

    // a new sample without WindowServer drops the selection: `t` still kills the popup's process
    let mut procs = varied_procs();
    procs.remove(1);
    put_procs(&mut app, procs);
    assert_eq!(app.proc_view.selected_pid(), None);
    assert!(app.handle_key(key('t')).is_continue());
    assert!(!app.kill.is_open());
    assert_eq!(fake.calls(), [(631, 0), (631, libc::SIGTERM)]);
  }

  #[test]
  fn q_closes_the_kill_popup_without_quitting() {
    let (mut app, fake) = kill_app();
    assert!(app.handle_key(key('k')).is_continue());
    assert!(app.handle_key(key('q')).is_continue());
    assert!(!app.kill.is_open());
    assert_eq!(fake.calls(), [(631, 0)]);
    // closed: `q` quits again
    assert!(app.handle_key(key('q')).is_break());
  }

  #[test]
  fn the_kill_popup_takes_clicks_on_its_buttons_and_closes_with_the_list() {
    let (mut app, fake) = kill_app();
    assert!(app.handle_key(key('k')).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    // a click on the list, the wheel and focus loss leave the popup and the list as they are
    click(&mut app, 10, PROC_Y + 3);
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 10));
    assert!(app.handle_event(Event::FocusLost, &RwLock::new(TUI_MIN_MS)).is_continue());
    assert!(app.kill.is_open() && app.proc_view.selected_pid() == Some(631));

    let y = (0..50).find(|&y| row(&buf, y).contains("[ Force kill ]")).unwrap();
    click(&mut app, x_of(&row(&buf, y), "[ Force kill ]"), y);
    assert!(!app.kill.is_open());
    assert_eq!(fake.calls(), [(631, 0), (631, libc::SIGKILL)]);

    // the window too small for the list
    assert!(app.handle_key(key('k')).is_continue());
    drop(render_buffer(&mut app, 60, 12));
    assert!(!app.kill.is_open());
  }

  /// `kill_app` with the cwd and a log file open in WindowServer.
  fn info_app() -> (App, FakeSys) {
    let (app, fake) = kill_app();
    let cwd = OpenFile { fd: None, kind: Kind::Cwd, text: "/".into(), mode: "" };
    let log =
      OpenFile { fd: Some(3), kind: Kind::File, text: "/var/log/system.log".into(), mode: "r-" };
    fake.0.borrow_mut().files.insert(631, Ok(vec![cwd, log]));
    (app, fake)
  }

  #[test]
  fn paused_overlay_holds_files_and_metrics_keep_updating() {
    let (mut app, fake) = info_app();
    assert!(app.handle_key(key('i')).is_continue());
    assert!(app.handle_key(key(' ')).is_continue());
    let before = proc_row(&render_buffer(&mut app, 200, 50), 1);
    fake.0.borrow_mut().files.get_mut(&631).unwrap().as_mut().unwrap().clear();
    let mut procs = varied_procs();
    procs[1].cpu_pct = 99.0;
    put_procs(&mut app, procs);
    assert_eq!(proc_row(&render_buffer(&mut app, 200, 50), 1), before);
    let metrics = Metrics { sys_power: 42.0, ..test_metrics() };
    assert!(
      app.handle_event(Event::Update(Box::new(metrics)), &RwLock::new(TUI_MIN_MS)).is_continue()
    );
    assert_eq!(app.sys_power.items[0], 42000);
    assert!(app.handle_key(key('q')).is_continue());
    assert_eq!(app.proc_view.selected().unwrap().cpu_pct, 99.0);
    assert!(app.handle_key(key('i')).is_continue());
    assert!(proc_row(&render_buffer(&mut app, 200, 50), 1).contains("0 fds"));
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.handle_key(key(' ')).is_continue());
    assert!(app.proc_view.paused, "Space in a filter is text");
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    for width in [200, 60, 20] {
      let mut term = Terminal::new(TestBackend::new(width, 20)).unwrap();
      term.draw(|f| app.info.as_mut().unwrap().render(f, f.area(), true)).unwrap();
      assert!(screen_text(term.backend().buffer()).contains("paused"));
    }
    assert!(app.handle_key(key(' ')).is_continue());
    put_procs(&mut app, varied_procs());
    assert!(!app.proc_view.paused);
  }

  #[test]
  fn paused_rows_show_markers_safe_actions_and_indicator_with_filters() {
    let (mut app, fake) = info_app();
    assert!(app.handle_key(key(' ')).is_continue());
    let mut procs = varied_procs();
    procs.retain(|p| p.pid != 631);
    let mut new = procs[1].clone();
    new.pid = 9000;
    new.name = "new worker".into();
    procs.push(new);
    put_procs(&mut app, procs);
    let buf = render_buffer(&mut app, 200, 50);
    let exited = proc_row(&buf, 2);
    assert!(exited.contains("(exited) WindowServer"), "{exited}");
    assert_eq!(exited.split_whitespace().filter(|s| *s == "-").count(), 5);
    assert!(proc_row(&buf, 5).contains("(new) new worker"));
    for c in ['i', 'k'] {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(app.info.is_none() && !app.kill.is_open() && fake.calls().is_empty());
    assert!(app.handle_key(key('c')).is_continue());
    assert_eq!(fake.0.borrow().copied, [WINDOW_SERVER]);
    for c in "/this is a very long filter".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(app.handle_key(key(' ')).is_continue());
    assert!(app.proc_view.paused && app.proc_view.filter().ends_with(' '));
    assert!(proc_row(&render_buffer(&mut app, 30, 50), 0).contains("paused"));
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    assert!(app.handle_key(key('?')).is_continue());
    assert!(app.handle_key(key(' ')).is_continue() && app.proc_view.paused);
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    render_buffer(&mut app, 60, 12);
    assert!(!app.proc_view.paused);
  }

  #[test]
  fn i_opens_the_files_of_the_selected_process_over_the_list() {
    let (mut app, fake) = info_app();
    assert!(app.handle_key(key('i')).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ WindowServer 631 ───"), "{title}");
    assert!(title.ends_with("─ copy | finder ─╮"), "{title}");
    assert!(proc_row(&buf, 1).starts_with("│ 1 fds  1 files "), "{}", proc_row(&buf, 1));
    assert!(proc_row(&buf, 2).starts_with("├─ / filter ───"), "{}", proc_row(&buf, 2));
    assert!(proc_row(&buf, 3).starts_with(&format!("│ exe       {WINDOW_SERVER} ")));
    assert!(proc_row(&buf, 4).starts_with("│ cwd       / "), "{}", proc_row(&buf, 4));
    assert!(proc_row(&buf, 5).starts_with("│ file  r-  /var/log/system.log "));
    assert_eq!(proc_row(&buf, 29), format!("╰{} ? | q back ─╯", "─".repeat(185)));

    // it takes every key, the list's and the global ones (`q` closes it); Ctrl-C still quits
    for c in ['k', 'i', 'p', 's', 'v'] {
      assert!(app.handle_key(key(c)).is_continue(), "{c}");
    }
    assert!(!app.kill.is_open() && app.cfg.show_procs && app.cfg.view_type == ViewType::Graph);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
    // the wheel moves its selection, not the list's; `e` reveals the selected file, `f` nothing
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 5));
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 5));
    assert!(app.handle_key(key('f')).is_continue() && fake.0.borrow().revealed.is_empty());
    assert!(app.handle_key(key('e')).is_continue());
    assert_eq!(fake.0.borrow().revealed, ["/var/log/system.log"]);
    assert_eq!(app.proc_view.selected_pid(), Some(631));

    // each sample reads the files again, and the name and path of the process (an exec)
    let pipe = OpenFile { fd: Some(4), kind: Kind::Pipe, text: String::new(), mode: "" };
    fake.0.borrow_mut().files.get_mut(&631).unwrap().as_mut().unwrap().push(pipe);
    let mut procs = varied_procs();
    (procs[1].name, procs[1].path) = ("zsh".into(), "/bin/zsh".into());
    put_procs(&mut app, procs);
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 0).starts_with("╭─ zsh 631 ─"), "{}", proc_row(&buf, 0));
    assert!(proc_row(&buf, 1).starts_with("│ 2 fds  1 files  1 pipes "));
    assert!(proc_row(&buf, 3).starts_with("│ exe       /bin/zsh "));
    // focus loss ends filter input, Esc then closes it and the list has its keys back
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.handle_event(Event::FocusLost, &RwLock::new(TUI_MIN_MS)).is_continue());
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(app.info.is_none());
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert_eq!(app.proc_view.selected_pid(), Some(2301));

    // it closes with the list
    assert!(press(&mut app, KeyCode::Up).is_continue() && app.handle_key(key('i')).is_continue());
    render_buffer(&mut app, 60, 12);
    assert!(app.info.is_none());
  }

  #[test]
  fn the_help_opens_over_the_info_overlay_and_closes_back_to_it() {
    let (mut app, _fake) = info_app();
    assert!(app.handle_key(key('i')).is_continue() && press(&mut app, KeyCode::Down).is_continue());
    let cwd_selected = |app: &mut App| {
      let buf = render_buffer(app, 200, 50);
      buf[(2, PROC_Y + 4)].modifier.contains(Modifier::REVERSED)
    };
    assert!(cwd_selected(&mut app));

    // over it the help takes the keys and the wheel; Esc closes the help, not the overlay, and
    // `copied` went with the `?`
    assert!(app.handle_key(key('c')).is_continue());
    assert!(row(&render_buffer(&mut app, 200, 50), 49).starts_with("╰─ copied ─"));
    assert!(app.handle_key(key('?')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("Info overlay"));
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, 100, PROC_Y + 5));
    assert!(press(&mut app, KeyCode::Esc).is_continue());
    assert!(app.help.is_none() && app.info.is_some() && cwd_selected(&mut app));
    assert!(!row(&render_buffer(&mut app, 200, 50), 49).contains("copied"));

    // typed into its filter `?` is text
    assert!(app.handle_key(key('/')).is_continue() && app.handle_key(key('?')).is_continue());
    assert!(app.help.is_none());
    assert!(proc_row(&render_buffer(&mut app, 200, 50), 2).starts_with("├─ /?█ "));
  }

  #[test]
  fn q_closes_the_info_overlay_without_quitting() {
    let (mut app, _fake) = info_app();
    let row = |app: &mut App, y| proc_row(&render_buffer(app, 200, 50), y);
    assert!(app.handle_key(key('i')).is_continue());
    // typed into its filter `q` is text
    assert!(app.handle_key(key('/')).is_continue() && app.handle_key(key('q')).is_continue());
    assert!(press(&mut app, KeyCode::Enter).is_continue());
    assert!(row(&mut app, 2).starts_with("├─ /q ───"), "{}", row(&mut app, 2));
    // then it closes the overlay, with the filter set too
    assert!(app.handle_key(key('q')).is_continue());
    assert!(app.info.is_none());
    assert!(row(&mut app, 0).starts_with("╭─ proc 3 ─"), "{}", row(&mut app, 0));
    // closed: `q` quits again
    assert!(app.handle_key(key('q')).is_break());
  }

  #[test]
  fn e_shows_the_selected_process_in_finder() {
    let (mut app, fake) = info_app();
    // `f` is not bound
    assert!(app.handle_key(key('f')).is_continue() && fake.0.borrow().revealed.is_empty());
    assert!(app.handle_key(key('e')).is_continue());
    assert_eq!(fake.0.borrow().revealed, [WINDOW_SERVER]);

    // typed into the filter it is text; without a selection `i` and `e` do nothing
    assert!(app.handle_key(key('/')).is_continue() && app.handle_key(key('e')).is_continue());
    assert_eq!(app.proc_view.filter(), "e");
    for code in [KeyCode::Esc, KeyCode::Esc, KeyCode::Char('e'), KeyCode::Char('i')] {
      assert!(press(&mut app, code).is_continue());
    }
    assert_eq!((app.proc_view.selected_pid(), app.info.is_none()), (None, true));
    assert_eq!(fake.0.borrow().revealed.len(), 1);

    // nothing for a process without a readable path
    let mut app = app_with_procs(test_procs());
    let (kill, fake) = FakeSys::kill(&[]);
    app.kill = kill;
    assert!(press(&mut app, KeyCode::Down).is_continue() && app.handle_key(key('e')).is_continue());
    assert!(fake.0.borrow().revealed.is_empty());
  }

  #[test]
  fn c_copies_the_path_of_the_selected_process() {
    let (mut app, fake) = info_app();
    let bottom = |app: &mut App| row(&render_buffer(app, 200, 50), 49);
    assert!(app.handle_key(key('c')).is_continue());
    assert_eq!(fake.0.borrow().copied, [WINDOW_SERVER]);
    // `copied` faint in place of the POWER note until the next key
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰─ copied ─"), "{}", row(&buf, 49));
    assert_eq!(buf[(3, 49)].modifier, Modifier::DIM);
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(bottom(&mut app).starts_with("╰─ POWER: own processes only ─"));

    // typed into the filter it is text; nothing for a process without a readable path
    assert!(app.handle_key(key('/')).is_continue() && app.handle_key(key('c')).is_continue());
    assert_eq!((app.proc_view.filter(), fake.0.borrow().copied.len()), ("c", 1));
    let mut app = app_with_procs(test_procs());
    let (kill, fake) = FakeSys::kill(&[]);
    app.kill = kill;
    assert!(press(&mut app, KeyCode::Down).is_continue() && app.handle_key(key('c')).is_continue());
    assert!(fake.0.borrow().copied.is_empty() && !bottom(&mut app).contains("copied"));
  }

  #[test]
  fn k_typed_into_the_filter_stays_text() {
    let (mut app, fake) = kill_app();
    assert!(app.handle_key(key('/')).is_continue() && app.handle_key(key('k')).is_continue());
    assert_eq!((app.proc_view.filter(), app.kill.is_open(), fake.calls()), ("k", false, vec![]));
  }

  #[test]
  fn k_after_moving_from_the_filter_into_the_list_opens_the_popup() {
    let mut app = app_with_procs(varied_procs());
    for c in "/saf".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(press(&mut app, KeyCode::Down).is_continue());
    assert!(app.handle_key(key('k')).is_continue());
    assert_eq!((app.proc_view.filter(), app.kill.is_open()), ("saf", true));
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
