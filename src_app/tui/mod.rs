//! Terminal user interface.

mod boxes;
mod layout;
mod palette;
mod proc_view;
mod store;
mod theme;
mod widgets;

use std::io::{self, Stdout, Write, stdout};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::{sync::mpsc, time::Duration};

use ratatui::crossterm::{
  ExecutableCommand, cursor,
  event::{
    self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
  },
  terminal,
};
use ratatui::prelude::*;

use crate::config::{Config, TUI_MAX_MS, TUI_MIN_MS};
use crate::procs::{ProcInfo, ProcSampler};
use macmon::{Metrics, Sampler, SocInfo};
use palette::Palette;
use proc_view::ProcView;
use store::{CpuClusters, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};
use theme::Theme;

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

/// Raw mode and the alternate screen; `run_loop` turns mouse capture on after the palette query.
/// Whatever happens next, the terminal is restored by the guard, the panic hook or `leave_term`.
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

/// Starts reading input once the terminal is in raw mode: `query` asks for the palette first,
/// while nothing else reads the terminal (so its replies can't turn into key presses), then mouse
/// capture goes on (so mouse reports can't mix with the replies), then `start` runs the input
/// thread. Returns the palette.
fn start_input(
  out: &mut impl Write,
  query: impl FnOnce() -> Option<Palette>,
  start: impl FnOnce(),
) -> io::Result<Option<Palette>> {
  let palette = query();
  out.execute(EnableMouseCapture)?;
  start();
  Ok(palette)
}

// MARK: Threads

enum Event {
  Update(Box<Metrics>),
  Procs(Vec<ProcInfo>),
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

/// How often the paused process thread checks whether the panel is back, and the sleeping one
/// whether it is still on screen.
const PROCS_PAUSE_POLL: Duration = Duration::from_millis(100);
/// Window of the first process sample after the panel shows up, so the list fills in quickly.
const PROCS_WARMUP: Duration = Duration::from_millis(TUI_MIN_MS as u64);

/// Sends input events and a `Tick` every `tick` ms; stops once the app is gone.
fn run_inputs_thread(tx: mpsc::Sender<Event>, tick: u64) {
  let tick_rate = Duration::from_millis(tick);

  std::thread::spawn(move || {
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
  std::thread::spawn(move || {
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

/// Sleeps `duration` in `PROCS_PAUSE_POLL` steps, and stops early once `active` is cleared.
fn sleep_while(active: &AtomicBool, duration: Duration) {
  let deadline = Instant::now() + duration;
  while active.load(Ordering::Relaxed) {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
      return;
    }
    thread::sleep(left.min(PROCS_PAUSE_POLL));
  }
}

/// Sends `Event::Procs` every `msec` while `active` is set (the process panel is on screen) and
/// sleeps otherwise. A pause drops the sampler, so rates after it don't average over the hidden
/// time; it is noticed within `PROCS_PAUSE_POLL`, also during a long interval. Exits when the
/// receiver is gone.
fn run_procs_thread(
  tx: mpsc::Sender<Event>,
  msec: Arc<RwLock<u32>>,
  active: Arc<AtomicBool>,
) -> JoinHandle<()> {
  thread::spawn(move || {
    let mut sampler: Option<ProcSampler> = None;

    loop {
      if !active.load(Ordering::Relaxed) {
        sampler = None;
        thread::sleep(PROCS_PAUSE_POLL);
        continue;
      }

      let started = Instant::now();
      let delay = match sampler.as_mut() {
        Some(sampler) => {
          if tx.send(Event::Procs(sampler.sample())).is_err() {
            return;
          }
          Duration::from_millis((*msec.read().unwrap()).max(TUI_MIN_MS).into())
        }
        // the first sample only sets the baseline: its CPU and power rates are zero
        None => {
          sampler.insert(ProcSampler::new()).sample();
          PROCS_WARMUP
        }
      };
      // a pause during the wait goes back to the top, which drops the sampler
      sleep_while(&active, delay.saturating_sub(started.elapsed()));
    }
  })
}

// MARK: App

#[derive(Debug, Default)]
pub struct App {
  cfg: Config,
  /// Gradient and bar glyphs of the terminal; the gradient steps through ANSI colors until
  /// `run_loop` queries the palette.
  theme: Theme,

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
  /// Set while the process panel is on screen; the process thread samples only then.
  procs_active: Arc<AtomicBool>,
}

impl App {
  pub fn new() -> WithError<Self> {
    Ok(Self::with(SocInfo::new()?, Config::load()))
  }

  /// App for the chip `soc` with the settings `cfg`. The CPU clusters come from the chip, so the
  /// first frame already has their boxes and the chip title its core counts.
  fn with(soc: SocInfo, cfg: Config) -> Self {
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
    self.procs_active.load(Ordering::Relaxed)
  }

  /// Follows the process list visibility (`p` or auto-hidden). A hidden panel drops its
  /// list, so it reads "collecting…" when shown again instead of showing stale rows, and ends
  /// filter input, so keys don't go to a filter that isn't on screen.
  fn set_procs_visible(&mut self, visible: bool) {
    self.procs_active.store(visible, Ordering::Relaxed);
    if !visible {
      self.proc_view.clear();
    }
  }

  /// Stores a process sample; one still in flight when the panel got hidden is dropped.
  fn update_procs(&mut self, procs: Vec<ProcInfo>) {
    if self.procs_visible() {
      self.proc_view.set_procs(procs);
    }
  }

  /// Applies one event. Returns `Break` when the app should quit. Keys can change the interval,
  /// which `msec` hands on to the sampling threads.
  fn handle_event(&mut self, event: Event, msec: &RwLock<u32>) -> ControlFlow<()> {
    match event {
      Event::Update(data) => self.update_metrics(*data),
      Event::Procs(procs) => self.update_procs(procs),
      Event::Key(key) => {
        if self.handle_key(key).is_break() {
          return ControlFlow::Break(());
        }
        *msec.write().unwrap() = self.cfg.interval;
      }
      Event::Mouse(mouse) => self.handle_mouse(mouse),
      Event::Tick => {}
    }

    ControlFlow::Continue(())
  }

  /// Applies a key press to the app state. Returns `Break` when the app should quit.
  /// Keys of the process panel (only while it is on screen) take precedence; while a filter is
  /// typed every key except Ctrl-C goes to it.
  fn handle_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
      return ControlFlow::Break(());
    }

    if self.procs_visible() && self.update_proc_view(|view| view.handle_key(key)) {
      return ControlFlow::Continue(());
    }

    match key.code {
      KeyCode::Char('q') => return ControlFlow::Break(()),
      KeyCode::Char('r') => self.cfg.toggle_ratio_mode(),
      KeyCode::Char('+') => self.cfg.inc_interval(),
      KeyCode::Char('=') => self.cfg.inc_interval(), // fallback to press without shift
      KeyCode::Char('-') => self.cfg.dec_interval(),
      KeyCode::Char('p') => self.cfg.toggle_procs(),
      KeyCode::Char('v') => self.cfg.toggle_view_type(),
      _ => {}
    }

    ControlFlow::Continue(())
  }

  /// Applies a mouse event to the process list (only while it is on screen), at the cells of the
  /// last frame. Only the process list reacts to the mouse.
  fn handle_mouse(&mut self, mouse: MouseEvent) {
    if self.procs_visible() {
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

  fn render(&mut self, f: &mut Frame) {
    let plan = self.layout(f.area());
    self.set_procs_visible(plan.proc.is_some());

    // the key hints go on the lowest box: the process box, or the metrics box without it
    self.render_metrics_box(f, &plan);
    if let Some(r) = plan.proc {
      self.render_proc_box(f, r);
      self.render_key_hints(f, r);
    }
  }

  pub fn run_loop(&mut self, interval: Option<u32>) -> WithError<()> {
    // use from arg if provided, otherwise use config restored value
    self.cfg.interval = interval.unwrap_or(self.cfg.interval).clamp(TUI_MIN_MS, TUI_MAX_MS);
    let msec = Arc::new(RwLock::new(self.cfg.interval));

    let (tx, rx) = mpsc::channel::<Event>();
    run_sampler_thread(tx.clone(), msec.clone());
    run_procs_thread(tx.clone(), msec.clone(), self.procs_active.clone());

    // the guard restores the terminal on every way out of here, `?` included
    let (mut term, _guard) = enter_term()?;

    // the palette only matters for a smooth (truecolor) gradient, and SSH sessions skip the query
    // (late replies)
    let truecolor = theme::detect_truecolor();
    let query = || if palette::should_query(truecolor) { palette::query_terminal() } else { None };
    let palette = start_input(&mut stdout(), query, || run_inputs_thread(tx.clone(), 250))?;
    self.theme = Theme::new(palette).with_three_level_bars(theme::detect_three_level_bars());

    loop {
      term.draw(|f| self.render(f))?;
      if self.handle_event(rx.recv()?, &msec).is_break() {
        break;
      }
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::io::{self, Write};
  use std::ops::ControlFlow;
  use std::rc::Rc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::{Arc, RwLock, mpsc};
  use std::thread;
  use std::time::{Duration, Instant};

  use macmon::{FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::ExecutableCommand;
  use ratatui::crossterm::event::{
    self as term_event, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
  };
  use ratatui::layout::{Margin, Rect};
  use ratatui::style::{Color, Modifier};

  use super::layout::Metric;
  use super::palette::{Palette, Rgb};
  use super::store::{CpuClusters, FreqSample};
  use super::theme::{self, Theme};
  use super::{
    App, Event, PROCS_PAUSE_POLL, PROCS_WARMUP, input_event, restore_term_once, run_procs_thread,
    start_input,
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
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
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
  fn input_starts_after_the_palette_query_and_mouse_capture() {
    // one log for the query, the terminal output and the input thread
    #[derive(Clone, Default)]
    struct Log(Rc<RefCell<Vec<String>>>);
    impl Write for Log {
      fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().push(String::from_utf8_lossy(buf).into_owned());
        Ok(buf.len())
      }
      fn flush(&mut self) -> io::Result<()> {
        Ok(())
      }
    }

    let log = Log::default();
    let mut out = log.clone();
    let query = || {
      log.0.borrow_mut().push("query".into());
      Some(PALETTE)
    };
    let palette = start_input(&mut out, query, || log.0.borrow_mut().push("input".into()));
    assert_eq!(palette.unwrap(), Some(PALETTE));

    let mut capture = vec![];
    capture.execute(EnableMouseCapture).unwrap();
    let steps = log.0.borrow().clone();
    assert_eq!(steps.first().map(String::as_str), Some("query"), "{steps:?}");
    assert_eq!(steps.last().map(String::as_str), Some("input"), "{steps:?}");
    assert_eq!(steps[1..steps.len() - 1].concat().into_bytes(), capture, "{steps:?}");

    // no mouse capture: no input thread either
    struct Closed;
    impl Write for Closed {
      fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
      }
      fn flush(&mut self) -> io::Result<()> {
        Ok(())
      }
    }
    let mut started = false;
    assert!(start_input(&mut Closed, || None, || started = true).is_err());
    assert!(!started);
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
    assert!(handle(&mut app, Event::Procs(varied_procs())).is_continue());
    assert_eq!(app.proc_view.row_count(), 3);
    assert!(handle(&mut app, Event::Tick).is_continue());

    // a key changing the interval hands it on to the sampling threads
    assert!(handle(&mut app, Event::Key(key('+'))).is_continue());
    assert_eq!(*msec.read().unwrap(), 1250);
    assert_eq!(file.saved()["interval"], 1250);

    // the mouse: a click on the MEM header sorts by it
    let header = proc_row(&render_buffer(&mut app, 200, 50), 1);
    let at = mouse(MouseEventKind::Down(MouseButton::Left), x_of(&header, "MEM"), PROC_Y + 1);
    assert!(handle(&mut app, Event::Mouse(at)).is_continue());
    assert_eq!(app.proc_view.sort, ProcSort::Mem);

    assert!(handle(&mut app, Event::Key(key('q'))).is_break());
    assert_eq!(*msec.read().unwrap(), 1250);
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
    let mut app = App::with(test_soc(), Config::default());
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
    let mut app = App::with(test_soc(), Config::load_from(Some(file.path())));
    for _ in 0..3 {
      app.update_metrics(test_metrics());
    }
    app
  }

  /// Theme with a smooth gradient between the colors of `PALETTE`, so close loads differ in color.
  fn smooth() -> Theme {
    Theme::new(Some(PALETTE))
  }

  fn render_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    term.backend().buffer().clone()
  }

  fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    render_buffer(app, width, height).content.iter().map(|cell| cell.symbol()).collect()
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
    assert_eq!(app.cfg.interval, 1000);

    assert_eq!(app.handle_key(key('+')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval, 1250);

    assert_eq!(app.handle_key(key('=')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval, 1500);

    assert_eq!(app.handle_key(key('-')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.interval, 1250);
  }

  #[test]
  fn unknown_keys_are_ignored() {
    let mut app = App::default();
    for code in [KeyCode::Char('x'), KeyCode::Esc, KeyCode::Enter, KeyCode::Up] {
      let event = KeyEvent::new(code, KeyModifiers::NONE);
      assert_eq!(app.handle_key(event), ControlFlow::Continue(()));
    }

    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval, 1000);
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
    assert_eq!(row(&buf, 49), border(200, SUMMARY, &hints(5)));
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!screen.contains(" proc ") && !screen.contains("WindowServer"));
    assert!(!procs_active(&app), "no process sampling while hidden");

    // shown again: collecting until the next sample, its controls in its box
    assert_eq!(app.handle_key(key('p')), ControlFlow::Continue(()));
    assert!(app.cfg.show_procs);
    assert_eq!(file.saved()["show_procs"], true);
    let buf = render_buffer(&mut app, 200, 50);
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains("collecting…"));
    assert!(proc_row(&buf, 0).starts_with("╭─ proc ─ / filter ─"), "{}", proc_row(&buf, 0));
    assert_eq!(row(&buf, PROC_Y - 1), border(200, SUMMARY, ""));
    assert_eq!(row(&buf, 49), hints_border(200, 5));
    assert!(procs_active(&app));
  }

  /// Solarized-like terminal palette, as a terminal would answer the palette query.
  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  /// App with every kind of colored cell on screen: metric boxes, processes with a selected row.
  fn colorful_app(theme: Theme) -> App {
    let mut app = app_with_procs(varied_procs());
    app.theme = theme;
    assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).is_continue());
    app
  }

  fn frame_colors(buf: &Buffer) -> impl Iterator<Item = Color> + '_ {
    buf.content.iter().flat_map(|cell| [cell.fg, cell.bg])
  }

  #[test]
  fn renders_terminal_colors_without_smooth_palette() {
    // no palette (not asked without truecolor, or not answered): ANSI colors only, with either
    // bar set
    let themes = [Theme::new(None), Theme::default().with_three_level_bars(true)];
    let ansi = [Color::Reset, Color::DarkGray, Color::Green, Color::Yellow, Color::Red];
    for theme in themes {
      let mut app = colorful_app(theme);
      for (width, height) in [(200, 50), (80, 24), (60, 15)] {
        let buf = render_buffer(&mut app, width, height);
        let ctx = format!("{theme:?} at {width}x{height}");
        assert_eq!((buf[(0, 0)].symbol(), buf[(0, 0)].fg), ("╭", Color::DarkGray), "{ctx}");
        for color in frame_colors(&buf) {
          assert!(ansi.contains(&color), "{color:?} in {ctx}");
        }
      }

      // all load levels show up: GPU 23%, E-CPU 42%, P-CPU 77%
      let buf = render_buffer(&mut app, 200, 50);
      for color in [Color::Green, Color::Yellow, Color::Red] {
        assert!(frame_colors(&buf).any(|c| c == color), "no {color:?} in {theme:?}");
      }
    }
  }

  #[test]
  fn smooth_gradient_blends_queried_colors() {
    let theme = smooth();
    let mut app = colorful_app(theme);
    let buf = render_buffer(&mut app, 200, 50);

    // every RGB color lies between the terminal's green and yellow or its yellow and red
    let between = |c: Rgb, a: Rgb, b: Rgb| {
      let within = |x: u8, y: u8, z: u8| x.min(y) <= z && z <= x.max(y);
      within(a.0, b.0, c.0) && within(a.1, b.1, c.1) && within(a.2, b.2, c.2)
    };
    let mut rgb = vec![];
    for color in frame_colors(&buf) {
      match color {
        Color::Rgb(r, g, b) => rgb.push((r, g, b)),
        // the rest of the UI stays in terminal colors
        color => assert!([Color::Reset, Color::DarkGray].contains(&color), "{color:?}"),
      }
    }
    assert!(rgb.len() > 50, "{} RGB colors", rgb.len());
    let Palette { green, yellow, red } = PALETTE;
    for color in &rgb {
      assert!(between(*color, green, yellow) || between(*color, yellow, red), "{color:?}");
    }

    // percent values sit exactly on the gradient
    let line = row(&buf, 1);
    let x = line.find(" 42% ").map(|i| line[..i].chars().count() as u16 + 1).unwrap();
    assert_eq!(buf[(x, 1)].fg, theme.gradient(test_metrics().ecpu_scaled_ratio.into()));
    assert_eq!(buf[(0, 0)].fg, Color::DarkGray, "borders stay terminal colors");
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

  /// Power summary of `test_metrics` in the original format, with its blank cells, on the bottom
  /// border of the metrics box.
  const SUMMARY: &str =
    " Power: 6.60W (avg 6.60W, max 6.60W) | Fan 1200 RPM | Total 12.00W (12.00, 12.00) ";

  /// Global key hints, in the order of the original UI.
  const HINTS: [&str; 5] = ["q quit", "p procs", "v chart", "r scaled", "-/+ 1000ms"];

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
  /// right)` titles, longest first, `right` empty for none) that fits uncut, or else the last
  /// one cut to the border.
  fn fitted_top(width: usize, variants: &[(&str, &str)]) -> String {
    let len = |s: &str| s.chars().count();
    // the left title and its blanks start 2 cells in; one cell between titles, 2 at the end
    let fits = |(left, right): (&str, &str)| match right {
      "" => 2 + len(left) + 2 + 2 <= width,
      _ => 2 + len(left) + 2 + 1 + len(right) + 2 + 2 <= width,
    };
    match variants.iter().find(|v| fits(**v)) {
      Some((left, right)) => top_border(width, left, right),
      None => {
        let (left, _) = variants.last().unwrap();
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
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
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
      let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
      let ctx = format!("{width}x{height}");

      // every box, each with its own name
      let boxes: Vec<Metric> = app.layout(buf.area).boxes.iter().map(|(m, _)| *m).collect();
      assert_eq!(boxes, names.map(|(metric, _)| metric), "{ctx}");
      for (metric, name) in names {
        let top = box_top(&app, &buf, metric);
        assert!(top.starts_with(&format!("╭─ {name}")), "{metric:?} ({ctx}): {top}");
      }

      for label in ["╭─ Apple M3 Pro (6E+6P+18GPU 36GB) ", "─ Power: 6.60W", "q quit"] {
        assert!(screen.contains(label), "missing {label:?} ({ctx})");
      }
      assert_eq!(screen.contains(" proc "), proc, "{ctx}");
    }
  }

  #[test]
  fn first_frame_has_every_cluster_box() {
    // before the first metrics sample: the cluster boxes and core counts come from the chip
    let mut app = App::with(test_soc(), Config::default());
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
    app.update_procs(varied_procs());
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

    // the process list in the rest of the screen: count and filter label on its top border, the
    // sort arrow by the sorted column, the global hints on its bottom border
    assert_eq!(rows[13], format!("╭─ proc 3 ─ / filter {}╮", "─".repeat(88)));
    let header = "│   PID NAME";
    let header_end = "USER       CPU% ↓    MEM   POWER   GPU% │";
    assert!(rows[14].starts_with(header) && rows[14].ends_with(header_end), "{}", rows[14]);
    assert!(rows[15].starts_with("│   631 WindowServer "), "{}", rows[15]);
    assert_eq!(rows[31], hints_border(110, 5));
  }

  /// App whose power samples differ, so the current value (the mean of the last two samples),
  /// average and maximum differ too: CPU 5 / 4 / 6 W, GPU 2.5 / 2 / 3, ANE 0.25 / 0.2 / 0.3,
  /// Power 9.5 / 8 / 12, Total 16 / 14 / 20.
  fn varied_power_app() -> App {
    let mut app = App::with(test_soc(), Config::default());
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
    // smooth, so the temperature's color tells 45 °C apart from the graph's low color
    let mut app = App { theme: smooth(), ..varied_power_app() };
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
    let temp = app.theme.gradient((45.0 - 30.0) / 70.0);
    assert_eq!(cell(x_of(&top, "45°C")).fg, temp);
    assert_ne!(temp, app.theme.gradient(0.0));
    let gpu = box_top(&app, &buf, Metric::GpuPower);
    let gpu_x = app.layout(buf.area).boxes[5].1.x + x_of(&gpu, "40°C");
    assert_eq!(buf[(gpu_x, cpu.y)].fg, app.theme.gradient((40.0 - 30.0) / 70.0));

    // the summary: Power (avg / max), the fan, Total (avg / max)
    let summary =
      " Power: 9.50W (avg 8.00W, max 12.00W) | Fan 1200 RPM | Total 16.00W (14.00, 20.00) ";
    let bottom = row(&buf, PROC_Y - 1);
    assert_eq!(bottom, border(200, summary, ""));
    let fg = |text: &str| buf[(x_of(&bottom, text), PROC_Y - 1)].fg;
    assert_eq!(fg("Power:"), theme::TEXT);
    assert_eq!(fg("(avg"), theme::DIM);
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

  /// Top border of the box of `metric` when the top row of boxes is `width` cells per box: the
  /// screen is 4 boxes + 2 borders wide.
  fn top_row_box(app: &mut App, metric: Metric, width: u16) -> String {
    let buf = render_buffer(app, width * 4 + 2, 50);
    let top = box_top(app, &buf, metric);
    assert_eq!(top.chars().count(), usize::from(width), "{metric:?} at {width}: {top}");
    top
  }

  /// Top border of the box of `metric` when the power boxes are `width` cells wide: the screen is
  /// 3 boxes + 2 borders wide.
  fn power_box_top(app: &mut App, metric: Metric, width: u16) -> String {
    let buf = render_buffer(app, width * 3 + 2, 50);
    let top = box_top(app, &buf, metric);
    assert_eq!(top.chars().count(), usize::from(width), "{metric:?} at {width}: {top}");
    top
  }

  /// RAM box title steps with swap, longest first.
  const RAM_SWAP_STEPS: [&str; 4] = [
    "RAM 16.81 GB (70.0%) · SWAP 2.37 / 3.0 GB",
    "RAM 16.8G 70% · SWAP 2.4G 79%",
    "RAM 70% · SWAP 79%",
    "RAM 70% SW 79%",
  ];

  /// RAM box title steps without swap, longest first.
  const RAM_STEPS: [&str; 3] = ["RAM 16.81 GB (70.0%)", "RAM 16.8G 70%", "RAM 70%"];

  #[test]
  fn ram_title_steps_down_with_swap() {
    let mut app = ram_app(true);
    // (box width, title): each step from the width where it fits whole (its text + 6 cells)
    let cases = [
      (60, RAM_SWAP_STEPS[0]),
      (47, RAM_SWAP_STEPS[0]),
      (46, RAM_SWAP_STEPS[1]),
      (35, RAM_SWAP_STEPS[1]),
      (34, RAM_SWAP_STEPS[2]),
      (24, RAM_SWAP_STEPS[2]),
      (23, RAM_SWAP_STEPS[3]),
      (20, RAM_SWAP_STEPS[3]),
    ];
    for (width, title) in cases {
      let top = top_row_box(&mut app, Metric::Ram, width);
      assert_eq!(top, top_border(width.into(), title, ""), "{width}");
    }

    // the last step is cut only when it doesn't fit whole
    assert_eq!(top_row_box(&mut app, Metric::Ram, 19), "╭─ RAM 70% SW 79%─╮");
    assert_eq!(top_row_box(&mut app, Metric::Ram, 14), "╭─ RAM 70% S─╮");
  }

  #[test]
  fn ram_title_steps_down_without_swap() {
    let mut app = ram_app(false);
    let cases = [
      (60, RAM_STEPS[0]),
      (26, RAM_STEPS[0]),
      (25, RAM_STEPS[1]),
      (19, RAM_STEPS[1]),
      (18, RAM_STEPS[2]),
      (13, RAM_STEPS[2]),
    ];
    for (width, title) in cases {
      let top = top_row_box(&mut app, Metric::Ram, width);
      assert_eq!(top, top_border(width.into(), title, ""), "{width}");
    }
    assert_eq!(top_row_box(&mut app, Metric::Ram, 12), "╭─ RAM 70%─╮");
    assert_eq!(top_row_box(&mut app, Metric::Ram, 10), "╭─ RAM 7─╮");

    let screen = render_to_string(&mut app, 240, 60);
    assert!(!screen.contains("SWAP") && !screen.contains("SW "));
  }

  #[test]
  fn ram_title_styles() {
    // smooth, so 70 % and 79 % get colors of their own
    let mut app = App { theme: smooth(), ..ram_app(true) };
    assert_ne!(app.theme.gradient(16.81 / 24.0), app.theme.gradient(2.37 / 3.0));
    let buf = render_buffer(&mut app, 60 * 4 + 2, 50);
    let ram = app.layout(buf.area).boxes[3].1;
    let top = box_top(&app, &buf, Metric::Ram);
    let cell = |text: &str| &buf[(ram.x + x_of(&top, text), ram.y)];

    // names bold, the separator dim, the RAM percent on the gradient
    assert!(cell("RAM").modifier.contains(Modifier::BOLD));
    assert!(cell("SWAP").modifier.contains(Modifier::BOLD));
    assert_eq!(cell("·").fg, theme::DIM);
    assert_eq!(cell("70.0%").fg, app.theme.gradient(16.81 / 24.0));
    assert_eq!(cell("16.81").fg, theme::TEXT);

    // shorter steps: both percents on the gradient
    let buf = render_buffer(&mut app, 30 * 4 + 2, 50);
    let ram = app.layout(buf.area).boxes[3].1;
    let top = box_top(&app, &buf, Metric::Ram);
    assert_eq!(top, top_border(30, RAM_SWAP_STEPS[2], ""));
    let fg = |text: &str| buf[(ram.x + x_of(&top, text), ram.y)].fg;
    assert_eq!(fg("70%"), app.theme.gradient(16.81 / 24.0));
    assert_eq!(fg("79%"), app.theme.gradient(2.37 / 3.0));
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
        assert_eq!(top_row_box(&mut app, *metric, width), fitted_top(width.into(), &steps));
      }
      let steps = RAM_SWAP_STEPS.map(|title| (title, ""));
      assert_eq!(top_row_box(&mut app, Metric::Ram, width), fitted_top(width.into(), &steps));
      let steps = RAM_STEPS.map(|title| (title, ""));
      assert_eq!(top_row_box(&mut dry, Metric::Ram, width), fitted_top(width.into(), &steps));
    }

    // power: the temperature goes first, then average and maximum; the current power stays
    let mut app = test_app();
    let cpu =
      [("CPU 4.50W (4.50, 4.50)", "45°C"), ("CPU 4.50W (4.50, 4.50)", ""), ("CPU 4.50W", "")];
    let ane = [("ANE 0.10W (0.10, 0.10)", ""), ("ANE 0.10W", "")];
    for width in 6..=60 {
      let fitted = |steps: &[(&str, &str)]| fitted_top(width.into(), steps);
      assert_eq!(power_box_top(&mut app, Metric::CpuPower, width), fitted(&cpu));
      assert_eq!(power_box_top(&mut app, Metric::AnePower, width), fitted(&ane));
    }
  }

  #[test]
  fn power_and_cluster_titles_at_their_step_widths() {
    let mut app = test_app();
    let dashes = |n: usize| "─".repeat(n);
    let cpu = |app: &mut App, width| power_box_top(app, Metric::CpuPower, width);
    assert_eq!(cpu(&mut app, 35), "╭─ CPU 4.50W (4.50, 4.50) ─ 45°C ─╮");
    assert_eq!(cpu(&mut app, 34), format!("╭─ CPU 4.50W (4.50, 4.50) {}╮", dashes(7)));
    assert_eq!(cpu(&mut app, 28), "╭─ CPU 4.50W (4.50, 4.50) ─╮");
    assert_eq!(cpu(&mut app, 27), format!("╭─ CPU 4.50W {}╮", dashes(13)));
    assert_eq!(cpu(&mut app, 15), "╭─ CPU 4.50W ─╮");
    assert_eq!(cpu(&mut app, 14), "╭─ CPU 4.50W─╮");

    let ecpu = |app: &mut App, width| top_row_box(app, Metric::Cluster(0), width);
    assert_eq!(ecpu(&mut app, 26), "╭─ E-CPU 42% @ 1800 MHz ─╮");
    assert_eq!(ecpu(&mut app, 25), format!("╭─ E-CPU 42% {}╮", dashes(11)));
    assert_eq!(ecpu(&mut app, 15), "╭─ E-CPU 42% ─╮");
    assert_eq!(ecpu(&mut app, 13), "╭─ E-CPU 42─╮");
  }

  #[test]
  fn power_summary_follows_sensors() {
    let power = " Power: 6.60W (avg 6.60W, max 6.60W)";
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
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), hints_border(200, 5));
    assert!(row(&buf, 49).ends_with("─ q quit | p procs | v chart | r scaled | -/+ 1000ms ─╯"));
    assert_eq!(row(&buf, PROC_Y - 1), border(200, SUMMARY, ""), "no hints on the metrics box");
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(screen.matches("q quit").count(), 1);
    // the process list controls live in its own box
    assert!(!screen.contains("s sort") && screen.matches("/ filter").count() == 1);
    assert!(proc_row(&buf, 0).contains(" / filter "));

    // keys bold, labels plain, separators dim
    let bottom = row(&buf, 49);
    let x = |text: &str| bottom[..bottom.find(text).unwrap()].chars().count() as u16;
    let cell = |x: u16| &buf[(x, 49)];
    assert!(cell(x("q quit")).modifier.contains(Modifier::BOLD));
    assert!(cell(x("p procs")).modifier.contains(Modifier::BOLD));
    assert!(cell(x("v chart")).modifier.contains(Modifier::BOLD));
    assert!(!cell(x("quit")).modifier.contains(Modifier::BOLD));
    assert_eq!(cell(x("quit")).fg, theme::TEXT);
    assert_eq!(cell(x("| p")).fg, theme::DIM);

    // the current ratio mode and interval; the label of `v` doesn't change with the view
    assert!(app.handle_key(key('r')).is_continue());
    assert!(app.handle_key(key('+')).is_continue());
    assert!(app.handle_key(key('v')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    let hints = "─ q quit | p procs | v chart | r active | -/+ 1250ms ─╯";
    assert!(bottom.ends_with(hints), "{bottom}");

    // without the process list: the same hints on the metrics box, after the power summary
    app.cfg.show_procs = false;
    assert!(app.handle_key(key('-')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    let hints = " q quit | p procs | v chart | r active | -/+ 1000ms ";
    assert_eq!(bottom, border(200, SUMMARY, hints));
  }

  #[test]
  fn key_hints_drop_from_the_end_when_narrow() {
    // (width, hints shown): `q quit` 6 cells, then 3 cells between hints and 1 at both ends,
    // plus `╰─` and `─╯`
    let cases =
      [(200, 5), (56, 5), (55, 4), (43, 4), (42, 3), (32, 3), (31, 2), (22, 2), (21, 1), (12, 1)];
    for (width, count) in cases.into_iter().chain([(11, 0), (5, 0)]) {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, 60);
      let proc = app.layout(buf.area).proc.expect("process box");
      assert_eq!(row(&buf, proc.bottom() - 1), hints_border(width.into(), count), "{width}");
    }
  }

  #[test]
  fn footer_and_power_summary_share_the_border() {
    let parts =
      ["Power: 6.60W (avg 6.60W, max 6.60W)", "Fan 1200 RPM", "Total 12.00W (12.00, 12.00)"];
    let summary = |count: usize| format!(" {} ", parts[..count].join(" | "));
    // (width, summary shown, hints shown): `q quit` first, then the summary, then the other hints
    let cases = [
      (200, summary(3), 5),
      (139, summary(3), 5),
      (138, summary(3), 4),
      (125, summary(3), 3),
      (95, summary(3), 1),
      (94, summary(2), 3),
      (65, summary(2), 1),
      (64, summary(1), 2),
      (50, summary(1), 1),
      // the Power part is cut next to `q quit`
      (49, " Power: 6.60W (avg 6.60W, max 6.60W)".to_string(), 1),
      (30, " Power: 6.60W (av".to_string(), 1),
      (12, String::new(), 1),
      // no room for `q quit`
      (11, " Power:".to_string(), 0),
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
    assert_eq!(row(&buf, 9), border(80, &summary(2), ""));
    assert_eq!(row(&buf, 23), hints_border(80, 5));
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
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
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
      app.theme = Theme::default().with_three_level_bars(three_levels);
      for (width, height) in [(200, 50), (120, 40), (60, 15)] {
        let buf = render_buffer(&mut app, width, height);
        let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
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
    for theme in [Theme::default(), smooth()] {
      let mut app = App { theme, ..App::with(test_soc(), Config::default()) };
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
            assert_eq!(cell.fg, theme.gradient(load), "{x}, {y}: {rows:#?}");
          }
        }
      }
      // the terminal's own green / yellow / red without a smooth palette
      if theme == Theme::default() {
        let colors = (graph.right() - 3..graph.right()).map(|x| buf[(x, graph.bottom() - 1)].fg);
        assert_eq!(colors.collect::<Vec<_>>(), [Color::Green, Color::Yellow, Color::Red]);
      }

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
          assert_eq!(cell.fg, theme.gradient(0.0), "{metric:?}");
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
        assert_eq!(buf[(area.x, y)].fg, app.theme.gradient(load), "{metric:?} row {y}");
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
    assert!(screen.contains("Power: 0.00W") && screen.contains("RAM 0.00 GB (0.0%)"));
    assert!(screen.contains("╭─ macmon ─"), "title without chip info");
    assert!(!screen.contains("°C") && !screen.contains("Total") && !screen.contains("Fan"));
  }

  fn test_procs() -> Vec<ProcInfo> {
    let proc = |pid: i32, name: &str| ProcInfo {
      pid,
      name: name.to_string(),
      user: "root".to_string(),
      cpu_pct: 12.5,
      mem_bytes: 64 << 20,
      power_w: Some(0.5),
      gpu_pct: 3.0,
    };
    vec![proc(1, "launchd"), proc(631, "WindowServer"), proc(2301, "Safari")]
  }

  fn procs_active(app: &App) -> bool {
    app.procs_active.load(Ordering::Relaxed)
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

    app.update_procs(test_procs());
    assert_eq!(app.proc_view.procs(), Some(test_procs().as_slice()));
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc 3 ") && screen.contains("WindowServer"));
    assert!(!screen.contains("collecting"));

    app.update_procs(vec![]);
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc 0 ") && !screen.contains("WindowServer"));
  }

  #[test]
  fn hidden_proc_panel_drops_samples() {
    let mut app = test_app();
    // samples arriving before the first frame are dropped
    app.update_procs(test_procs());
    assert_eq!(app.proc_view.procs(), None);

    render_buffer(&mut app, 200, 50);
    app.update_procs(test_procs());
    assert!(app.proc_view.procs().is_some());

    // hiding drops the list and a sample still in flight
    render_buffer(&mut app, 60, 12);
    assert_eq!(app.proc_view.procs(), None);
    app.update_procs(test_procs());
    assert_eq!(app.proc_view.procs(), None);

    // shown again: collecting until the next sample instead of stale rows
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    app.update_procs(test_procs());
    assert!(render_to_string(&mut app, 200, 50).contains(" proc 3 "));
  }

  /// `app` with the process panel on screen at 200x50 and `procs` in it.
  fn with_procs(mut app: App, procs: Vec<ProcInfo>) -> App {
    render_buffer(&mut app, 200, 50);
    app.update_procs(procs);
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

  fn varied_procs() -> Vec<ProcInfo> {
    let proc = |pid: i32, name: &str, cpu, mem_mb: u64, power_w, gpu_pct| ProcInfo {
      pid,
      name: name.to_string(),
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
    // smooth, so every value has a color of its own
    let mut app = app_with_procs(varied_procs());
    app.theme = smooth();
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
    let x_of = |row: &str, text: &str| row[..row.find(text).unwrap()].chars().count() as u16;
    let cell = |y: u16, row: &str, text: &str| buf[(x_of(row, text), PROC_Y + y)].fg;
    assert_eq!(cell(2, &rows[0], "25.0"), app.theme.gradient(0.25));
    assert_eq!(cell(2, &rows[0], "40.0"), app.theme.gradient(0.4));
    // MEM by its share of the RAM (36 GB), POWER against 10 W
    assert_eq!(cell(2, &rows[0], "300M"), app.theme.gradient(300.0 / (36.0 * 1024.0)));
    assert_eq!(cell(3, &rows[1], "1.5G"), app.theme.gradient(1.5 / 36.0));
    assert_eq!(cell(2, &rows[0], "1.50W"), app.theme.gradient(0.15));
    assert_eq!(cell(3, &rows[1], "0.80W"), app.theme.gradient(0.08));
    assert_ne!(app.theme.gradient(0.15), app.theme.gradient(0.08));
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

    app.update_procs(varied_procs());
    let line = proc_row(&render_buffer(&mut app, 200, 50), 4);
    assert!(line.ends_with("   0.0    20M       -    0.0 │"), "{line}");
  }

  #[test]
  fn narrow_proc_panel_drops_columns() {
    let mut app = test_app();
    render_buffer(&mut app, 40, 20);
    app.update_procs(varied_procs());

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
    // NAME gets the 9 cells left: truncated
    assert!(row(&buf, 10).starts_with("│   631 WindowSer   25.0"), "{}", row(&buf, 10));

    // very narrow: PID and NAME only, nothing drawn over the border
    let buf = render_buffer(&mut app, 18, 20);
    assert_eq!(row(&buf, 9), "│   PID NAME     │");
    assert_eq!(row(&buf, 10), "│   631 WindowSe │");
  }

  #[test]
  fn proc_table_keeps_a_blank_cell_at_both_borders() {
    // the widest pids (5 digits) and a name longer than its column
    let mut procs = varied_procs();
    procs[0].pid = 99_998;
    procs[1].name = "x".repeat(300);
    let mut app = app_with_procs(procs);
    assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).is_continue());

    for (width, height) in [(200, 50), (80, 24), (40, 20), (18, 20)] {
      let buf = render_buffer(&mut app, width, height);
      let proc = app.layout(buf.area).proc.expect("process box");
      for y in proc.top() + 1..proc.bottom() - 1 {
        let ctx = format!("{width}x{height}: {}", row(&buf, y));
        assert_eq!(buf[(proc.left() + 1, y)].symbol(), " ", "{ctx}");
        assert_eq!(buf[(proc.right() - 2, y)].symbol(), " ", "{ctx}");
      }

      let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
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
    let app = App::with(test_soc(), Config::load_from(Some(file.path())));
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
    assert_eq!(app.cfg.interval, 1000);
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
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_continue());
    assert!(!app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));
  }

  #[test]
  fn filter_narrows_the_table() {
    let mut app = app_with_procs(varied_procs());
    for c in "/SAF".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_continue());

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

    // (header, sort key, pids descending, pids ascending); a new column keeps the direction
    let cases = [
      ("PID", Pid, [2301, 631, 1], [1, 631, 2301]),
      ("NAME", Name, [631, 2301, 1], [1, 2301, 631]),
      // ties by pid
      ("USER", User, [631, 2301, 1], [1, 631, 2301]),
      ("MEM", Mem, [2301, 631, 1], [1, 631, 2301]),
      // launchd has no power reading: last both ways
      ("POWER", Power, [631, 2301, 1], [2301, 631, 1]),
      ("GPU%", Gpu, [631, 2301, 1], [1, 2301, 631]),
      ("CPU%", Cpu, [631, 2301, 1], [1, 2301, 631]),
    ];
    for (header, sort, desc, asc) in cases {
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      click(&mut app, x_of(&line, header), PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, true), "{header}");
      assert_eq!(shown_pids(&app), desc, "{header}");
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      assert_eq!(line.matches('↓').count() + line.matches('↑').count(), 1, "{line}");

      // again, on the arrow this time: the whole header cell counts
      let arrow = x_of(&line, &format!("{header} ↓")) + header.len() as u16 + 1;
      click(&mut app, arrow, PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, false), "{header}");
      // saved like `s` / `S`
      assert_eq!(file.saved()["proc_sort"], format!("{sort:?}"), "{header}");
      assert_eq!(file.saved()["proc_sort_desc"], false, "{header}");
      assert_eq!(shown_pids(&app), asc, "{header}");
      let line = proc_row(&render_buffer(&mut app, 200, 50), 1);
      assert!(line.contains(&format!("{header} ↑")), "{line}");

      // and back
      click(&mut app, x_of(&line, header), PROC_Y + 1);
      assert_eq!((app.cfg.proc_sort, app.cfg.proc_sort_desc), (sort, true), "{header}");
    }

    assert_eq!(file.saved()["proc_sort"], "Cpu");
    assert_eq!(file.saved()["proc_sort_desc"], true);
  }

  #[test]
  fn click_on_filter_label_starts_typing() {
    let mut app = app_with_procs(varied_procs());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    let x = x_of(&title, "/ filter");

    // the blank cells around the label and the count title don't count
    for x in [x - 1, x + 8, x_of(&title, "proc 3")] {
      click(&mut app, x, PROC_Y);
      assert!(!app.proc_view.typing(), "x {x}: {title}");
    }
    click(&mut app, x + 7, PROC_Y);
    assert!(app.proc_view.typing());

    // keys go to the filter, which replaces the label
    for c in "saf".chars() {
      assert!(app.handle_key(key(c)).is_continue());
    }
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert!(title.starts_with("╭─ proc 1/3 ─ /saf█ ─"), "{title}");

    // a kept filter: a click on its text edits it again
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_continue());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert!(title.starts_with("╭─ proc 1/3 ─ /saf ─"), "{title}");
    click(&mut app, x_of(&title, "/saf") + 2, PROC_Y);
    assert!(app.proc_view.typing());
    assert_eq!(app.proc_view.filter(), "saf");
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

    // kept: one more character instead of the cursor, and a click on it edits it again
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_continue());
    let title = proc_row(&render_buffer(&mut app, 200, 50), 0);
    assert_eq!(title, format!("╭─ proc 0/3 ─ /…{} ─╮", &filter[69..]));
    click(&mut app, 190, PROC_Y);
    assert!(app.proc_view.typing());

    // no room next to the count: the filter takes its place
    let buf = render_buffer(&mut app, 20, 50);
    assert_eq!(app.layout(buf.area).proc.map(|r| r.y), Some(PROC_Y));
    assert_eq!(proc_row(&buf, 0), format!("╭─ /…{}█ ─╮", &filter[239..]));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_continue());
    let buf = render_buffer(&mut app, 20, 50);
    assert_eq!(proc_row(&buf, 0), format!("╭─ /…{} ─╮", &filter[238..]));
    click(&mut app, 5, PROC_Y);
    assert!(app.proc_view.typing());
  }

  #[test]
  fn narrow_process_box_drops_the_filter_label_and_its_click() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 23, 50);
    assert_eq!(proc_row(&buf, 0), "╭─ proc 3 ─ / filter ─╮");

    // one cell less: no label, and nothing on the border starts typing
    let buf = render_buffer(&mut app, 22, 50);
    assert_eq!(proc_row(&buf, 0), format!("╭─ proc 3 {}╮", "─".repeat(11)));
    for x in 0..22 {
      click(&mut app, x, PROC_Y);
      assert!(!app.proc_view.typing(), "x {x}");
    }

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

    // a scrolled table: the row on screen, not the row from the top
    let mut app = hundred_procs_app();
    assert!(app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).is_continue());
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
    let wheel = |app: &mut App, kind| app.handle_mouse(mouse(kind, 100, PROC_Y + 10));
    // (screen row of the selection, its process)
    let selected = |app: &mut App| {
      let buf = render_buffer(app, 200, 50);
      let rows = (2..29).filter(|&y| buf[(1, PROC_Y + y)].modifier.contains(Modifier::REVERSED));
      let rows: Vec<u16> = rows.map(|y| y - 2).collect();
      assert_eq!(rows.len(), 1, "{rows:?}");
      (rows[0], app.proc_view.selected_pid().unwrap() - 1000)
    };
    use MouseEventKind::{ScrollDown, ScrollUp};

    // without a selection: from the top row, which scrolls away with the selection on it
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (0, 3));
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (0, 6));
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (0, 3));
    // the top of the table: the selection moves up on screen instead
    wheel(&mut app, ScrollUp);
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (0, 0));

    // the selected row keeps its place on screen
    for _ in 0..5 {
      assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).is_continue());
    }
    assert_eq!(selected(&mut app), (5, 5));
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (5, 8));
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (5, 5));

    // the end of the table: the selection moves down on screen, then stops
    assert!(app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).is_continue());
    assert_eq!(selected(&mut app), (26, 99));
    wheel(&mut app, ScrollUp);
    assert_eq!(selected(&mut app), (26, 96));
    wheel(&mut app, ScrollDown);
    wheel(&mut app, ScrollDown);
    assert_eq!(selected(&mut app), (26, 99));

    // the wheel over the metrics box doesn't move the list
    app.handle_mouse(mouse(ScrollUp, 100, 3));
    assert_eq!(selected(&mut app), (26, 99));
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

    let cells = [
      // borders and the padding cells of the header row, the gaps between header cells
      (0, PROC_Y + 1),
      (1, PROC_Y + 1),
      (198, PROC_Y + 1),
      (199, PROC_Y + 3),
      (x_of(&header, "CPU% ↓") - 1, PROC_Y + 1),
      (x_of(&header, "USER") + 10, PROC_Y + 1),
      // top border, the count title, the bottom border with the key hints
      (100, PROC_Y),
      (4, PROC_Y),
      (100, 49),
      (x_of(&row(&buf, 49), "q quit"), 49),
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
    let targets = [(x_of(&header, "MEM"), PROC_Y + 1), (x_of(&proc_row(&buf, 0), "/"), PROC_Y)];
    for kind in kinds {
      for (x, y) in targets.into_iter().chain([(100, PROC_Y + 2)]) {
        app.handle_mouse(mouse(kind, x, y));
        assert_eq!(state(&app), before, "{kind:?} at {x}, {y}");
      }
    }
    assert_eq!(
      render_to_string(&mut app, 200, 50),
      buf.content.iter().map(|c| c.symbol()).collect::<String>()
    );
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
    app.update_procs(varied_procs());
    render_buffer(&mut app, 200, 50);
    render_buffer(&mut app, 60, 12);
    try_all(&mut app);

    // back on screen, the same clicks work again
    render_buffer(&mut app, 200, 50);
    app.update_procs(varied_procs());
    render_buffer(&mut app, 200, 50);
    click(&mut app, mem.0, mem.1);
    assert_eq!(app.cfg.proc_sort, ProcSort::Mem);
  }

  #[test]
  fn selected_row_is_highlighted_and_scrolled_into_view() {
    let procs: Vec<ProcInfo> = (0..100)
      .map(|i| ProcInfo { pid: 1000 + i, name: format!("proc{i}"), ..test_procs()[0].clone() })
      .collect();
    let mut app = app_with_procs(procs);
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);

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

    assert!(app.handle_key(down).is_continue());
    assert!(app.handle_key(down).is_continue());
    assert_eq!(app.proc_view.selected_pid(), Some(1001));
    let buf = render_buffer(&mut app, 200, 50);
    let line = proc_row(&buf, 3);
    assert!(line.contains("proc1 "));
    // the whole row, gradient-colored CPU% too
    let cpu = line[..line.find("12.5").unwrap()].chars().count() as u16;
    for x in [1, 100, cpu, 198] {
      assert!(selected(&buf, x, 3), "x {x}");
    }
    assert_eq!(buf[(cpu, PROC_Y + 2)].fg, app.theme.gradient(0.125));
    assert!(!selected(&buf, 1, 2));
    assert!(!selected(&buf, 0, 3), "border not highlighted");
    assert_eq!(buf.content.iter().filter(|cell| reversed(cell)).count(), 198);

    // End: the last process is on the last row (29 is the border); 27 rows on screen
    assert!(app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 28).contains("proc99 "));
    assert!(selected(&buf, 1, 28));
    assert!(proc_row(&buf, 2).contains("proc73 "), "{}", proc_row(&buf, 2));

    // esc clears the selection, the table goes back to the top
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc0 "));
  }

  #[test]
  fn procs_thread_samples_only_while_active() {
    let (tx, rx) = mpsc::channel();
    let active = Arc::new(AtomicBool::new(false));
    let msec = Arc::new(RwLock::new(TUI_MIN_MS));
    let sampler = run_procs_thread(tx, msec.clone(), active.clone());

    // Shows the panel; returns how long the first sample took and the sample.
    let show = |active: &AtomicBool| {
      let shown = Instant::now();
      active.store(true, Ordering::Relaxed);
      let Ok(Event::Procs(procs)) = rx.recv_timeout(Duration::from_secs(5)) else {
        panic!("no process sample");
      };
      (shown.elapsed(), procs)
    };

    // paused: nothing
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "sampled while paused");

    // shown: the baseline sample stays silent, the first message comes after the warm-up with
    // the own process in it
    let (waited, procs) = show(&active);
    assert!(waited >= PROCS_WARMUP, "{waited:?}: the baseline sample was sent");
    let pid = std::process::id() as i32;
    assert!(procs.iter().any(|p| p.pid == pid && !p.name.is_empty()));

    // paused again: a sample already in progress may still arrive, but no more
    active.store(false, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_millis(1200);
    let mut late = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
      if rx.recv_timeout(left).is_ok() {
        late += 1;
      }
    }
    assert!(late <= 1, "{late} samples after pausing");

    // the pause dropped the sampler: a new baseline and warm-up, then the long interval
    *msec.write().unwrap() = TUI_MAX_MS;
    let (waited, _) = show(&active);
    assert!(waited >= PROCS_WARMUP, "{waited:?}: no new baseline after the pause");

    // hidden and shown again well within the 10 s interval: noticed during the wait, so the list
    // fills in after a new warm-up, not after the rest of the interval
    active.store(false, Ordering::Relaxed);
    thread::sleep(PROCS_PAUSE_POLL * 5);
    let (waited, _) = show(&active);
    assert!(waited >= PROCS_WARMUP, "{waited:?}");

    // exits once the receiver is gone (at its next send, after a pause cuts the wait short)
    drop(rx);
    active.store(false, Ordering::Relaxed);
    thread::sleep(PROCS_PAUSE_POLL * 5);
    active.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sampler.is_finished() {
      assert!(Instant::now() < deadline, "the process thread doesn't exit");
      thread::sleep(Duration::from_millis(10));
    }
    sampler.join().expect("process thread exits cleanly");
  }
}
