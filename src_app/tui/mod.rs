//! Terminal user interface.

mod layout;
mod palette;
mod panels;
mod proc_view;
mod store;
mod theme;
mod widgets;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::{io::stdout, time::Instant};
use std::{sync::mpsc, time::Duration};

use ratatui::crossterm::{
  ExecutableCommand,
  event::{self, KeyCode, KeyEvent, KeyModifiers},
  terminal,
};
use ratatui::prelude::*;

use crate::config::{Config, TUI_MAX_MS, TUI_MIN_MS};
use crate::procs::{ProcInfo, ProcSampler};
use macmon::{Metrics, Sampler, SocInfo};
use proc_view::ProcView;
use store::{CpuClusters, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};
use theme::Theme;

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

// MARK: Term utils

fn enter_term() -> Terminal<impl Backend> {
  std::panic::set_hook(Box::new(|info| {
    leave_term();
    eprintln!("{}", info);
  }));

  terminal::enable_raw_mode().unwrap();
  stdout().execute(terminal::EnterAlternateScreen).unwrap();

  let term = CrosstermBackend::new(std::io::stdout());
  Terminal::new(term).unwrap()
}

fn leave_term() {
  terminal::disable_raw_mode().unwrap();
  stdout().execute(terminal::LeaveAlternateScreen).unwrap();
}

// MARK: Threads

enum Event {
  Update(Box<Metrics>),
  Procs(Vec<ProcInfo>),
  Key(KeyEvent),
  Tick,
}

/// How often the paused process thread checks whether the panel is back.
const PROCS_PAUSE_POLL: Duration = Duration::from_millis(100);
/// Window of the first process sample after the panel shows up, so the list fills in quickly.
const PROCS_WARMUP: Duration = Duration::from_millis(TUI_MIN_MS as u64);

fn run_inputs_thread(tx: mpsc::Sender<Event>, tick: u64) {
  let tick_rate = Duration::from_millis(tick);

  std::thread::spawn(move || {
    let mut last_tick = Instant::now();

    loop {
      if event::poll(Duration::from_millis(tick)).unwrap() {
        match event::read().unwrap() {
          event::Event::Key(key) => tx.send(Event::Key(key)).unwrap(),
          _ => {}
        };
      }

      if last_tick.elapsed() >= tick_rate {
        tx.send(Event::Tick).unwrap();
        last_tick = Instant::now();
      }
    }
  });
}

fn run_sampler_thread(tx: mpsc::Sender<Event>, msec: Arc<RwLock<u32>>) {
  std::thread::spawn(move || {
    let mut sampler = Sampler::new().unwrap();

    // Send initial metrics
    tx.send(Event::Update(Box::new(sampler.get_metrics(100).unwrap()))).unwrap();

    loop {
      let msec = (*msec.read().unwrap()).max(TUI_MIN_MS);
      tx.send(Event::Update(Box::new(sampler.get_metrics(msec).unwrap()))).unwrap();
    }
  });
}

/// Sends `Event::Procs` every `msec` while `active` is set (the process panel is on screen) and
/// sleeps otherwise. A pause drops the sampler, so rates after it don't average over the hidden
/// time. Exits when the receiver is gone.
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
      thread::sleep(delay.saturating_sub(started.elapsed()));
    }
  })
}

// MARK: App

#[derive(Debug, Default)]
pub struct App {
  cfg: Config,
  /// Terminal colors; the gradient steps through ANSI colors until `run_loop` queries the palette.
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
    let soc = SocInfo::new()?;
    let cfg = Config::load();
    let proc_view = ProcView::new(cfg.proc_sort, cfg.proc_sort_desc);
    Ok(Self { cfg, soc, proc_view, ..Default::default() })
  }

  fn update_metrics(&mut self, data: Metrics) {
    self.cpu_power.push(data.cpu_power as f64);
    self.gpu_power.push(data.gpu_power as f64);
    self.ane_power.push(data.ane_power as f64);
    self.all_power.push(data.all_power as f64);
    self.sys_power.push(data.sys_power as f64);

    self.clusters.push(&store::cluster_samples(&self.soc, &data));
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

  /// Follows the process panel visibility (toggled or auto-hidden). A hidden panel drops its
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

  /// Applies a key press to the app state. Returns `Break` when the app should quit.
  /// Keys of the process panel (only while it is on screen) take precedence; while a filter is
  /// typed every key except Ctrl-C goes to it.
  fn handle_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
      return ControlFlow::Break(());
    }

    if self.procs_visible() {
      let sort = (self.proc_view.sort, self.proc_view.sort_desc);
      let used = self.proc_view.handle_key(key);
      if (self.proc_view.sort, self.proc_view.sort_desc) != sort {
        self.cfg.set_proc_sort(self.proc_view.sort, self.proc_view.sort_desc);
      }
      if used {
        return ControlFlow::Continue(());
      }
    }

    match key.code {
      KeyCode::Char('q') => return ControlFlow::Break(()),
      KeyCode::Char('d') => self.cfg.toggle_per_core_view(),
      KeyCode::Char('r') => self.cfg.toggle_ratio_mode(),
      KeyCode::Char('+') => self.cfg.inc_interval(),
      KeyCode::Char('=') => self.cfg.inc_interval(), // fallback to press without shift
      KeyCode::Char('-') => self.cfg.dec_interval(),
      KeyCode::Char(c @ '1'..='5') => self.cfg.toggle_panel(c),
      _ => {}
    }

    ControlFlow::Continue(())
  }

  fn render_all_hidden(&self, f: &mut Frame, area: Rect) {
    let text = "all panels hidden · press 1-5 to show · q quit";
    let row = area.centered_vertically(Constraint::Length(1));
    f.render_widget(Line::from(Span::styled(text, self.theme.dim)).centered(), row);
  }

  fn render(&mut self, f: &mut Frame) {
    let plan = self.layout(f.area());
    self.set_procs_visible(plan.proc.is_some());

    self.render_metrics_box(f, &plan);
    if let Some(r) = plan.proc {
      self.render_proc_box(f, r);
    }

    let hints_end = match plan.bottom() {
      Some(r) => self.render_key_hints(f, r),
      None => {
        self.render_all_hidden(f, f.area());
        None
      }
    };

    // the process box is the bottom one: its hints follow the global ones and give way first
    if let (Some(r), Some(end)) = (plan.proc, hints_end) {
      self.render_proc_hints(f, r, end + 1);
    }
  }

  pub fn run_loop(&mut self, interval: Option<u32>) -> WithError<()> {
    // use from arg if provided, otherwise use config restored value
    self.cfg.interval = interval.unwrap_or(self.cfg.interval).clamp(TUI_MIN_MS, TUI_MAX_MS);
    let msec = Arc::new(RwLock::new(self.cfg.interval));

    let (tx, rx) = mpsc::channel::<Event>();
    run_sampler_thread(tx.clone(), msec.clone());
    run_procs_thread(tx.clone(), msec.clone(), self.procs_active.clone());

    let mut term = enter_term();

    // raw mode is on and the input thread doesn't read the terminal yet, so the palette replies
    // can't turn into key presses; the palette only matters for a smooth (truecolor) gradient
    let truecolor = theme::detect_truecolor();
    let palette = if truecolor { palette::query_terminal() } else { None };
    self.theme = Theme::new(palette, truecolor);
    run_inputs_thread(tx.clone(), 250);

    loop {
      term.draw(|f| self.render(f)).unwrap();

      match rx.recv()? {
        Event::Update(data) => self.update_metrics(*data),
        Event::Procs(procs) => self.update_procs(procs),
        Event::Key(key) => {
          if self.handle_key(key).is_break() {
            break;
          }
          *msec.write().unwrap() = self.cfg.interval;
        }
        Event::Tick => {}
      }
    }

    leave_term();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::ops::ControlFlow;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::{Arc, RwLock, mpsc};
  use std::time::{Duration, Instant};

  use macmon::{CpuCoreMetrics, FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
  use ratatui::layout::Rect;
  use ratatui::style::{Color, Modifier};

  use super::layout::Strip;
  use super::palette::{Palette, Rgb};
  use super::store::{ClusterSample, CpuClusters, FreqSample};
  use super::theme::Theme;
  use super::widgets::core_bar;
  use super::{App, Event, run_procs_thread};
  use crate::config::{Panels, ProcSort, RatioMode, TUI_MIN_MS};
  use crate::procs::ProcInfo;

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
  }

  fn core(core_id: usize, ratio: f32) -> CpuCoreMetrics {
    CpuCoreMetrics { die_id: 0, core_id, freq_mhz: 2000, scaled_ratio: ratio, active_ratio: ratio }
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
      ecpu_cores: (0..6).map(|i| core(i, 0.4)).collect(),
      pcpu_cores: (0..6).map(|i| core(i, 0.7)).collect(),
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

  /// App with a few samples of `test_metrics` changed by `edit`.
  fn test_app_with(edit: impl Fn(&mut Metrics)) -> App {
    let mut app = App { soc: test_soc(), ..Default::default() };
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

  #[test]
  fn quit_keys_break() {
    let mut app = App::default();
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
  }

  #[test]
  fn c_and_v_do_nothing() {
    // no theme switching and no graph style switching: the keys are free
    let mut app = app_with_procs(varied_procs());
    let cfg = serde_json::to_string(&app.cfg).unwrap();
    let theme = app.theme;
    for c in ['c', 'v', 'C', 'V'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?}");
    }

    assert_eq!(serde_json::to_string(&app.cfg).unwrap(), cfg);
    assert_eq!(app.theme, theme);
    assert!(!app.proc_view.typing() && app.proc_view.filter().is_empty());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(bottom.starts_with(GLOBAL_HINTS), "{bottom}");
  }

  #[test]
  fn d_toggles_per_core_view() {
    let mut app = App::default();
    assert!(!app.cfg.per_core_view);
    assert_eq!(app.handle_key(key('d')), ControlFlow::Continue(()));
    assert!(app.cfg.per_core_view);
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

    assert!(!app.cfg.per_core_view);
    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval, 1000);
    assert_eq!(app.cfg.panels, Panels::default());
  }

  /// Solarized-like terminal palette, as a terminal would answer the palette query.
  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  /// App with every kind of colored cell on screen: strips, cores row, power column, processes
  /// with a selected row.
  fn colorful_app(theme: Theme) -> App {
    let mut app = app_with_procs(varied_procs());
    app.theme = theme;
    app.cfg.per_core_view = true;
    assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).is_continue());
    app
  }

  fn frame_colors(buf: &Buffer) -> impl Iterator<Item = Color> + '_ {
    buf.content.iter().flat_map(|cell| [cell.fg, cell.bg])
  }

  #[test]
  fn renders_terminal_colors_without_smooth_palette() {
    // no palette, no truecolor, or neither: ANSI colors only
    let themes = [Theme::new(None, true), Theme::new(Some(PALETTE), false), Theme::default()];
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
    let theme = Theme::new(Some(PALETTE), true);
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
    assert!(rgb.len() > 100, "{} RGB colors", rgb.len());
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

  /// Text that only the given panel renders (with `test_metrics`).
  const PANEL_MARKERS: [(char, &[&str]); 4] = [
    ('1', &["E-CPU  42% 1.8GHz ", "P-CPU  77% 3.2GHz "]),
    ('2', &["GPU    23% 1.4GHz "]),
    ('3', &["RAM    56% 20/36G ", "SWAP   50% 1/2G   "]),
    ('4', &["CPU   4.50W  45°C ", "GPU   2.00W  40°C ", "ANE   0.10W", "all   6.60W"]),
  ];

  #[test]
  fn renders_metric_panels_at_common_sizes() {
    // (width, height, process panel shown)
    let sizes = [(200, 50, true), (120, 40, true), (100, 30, true), (80, 24, true), (72, 24, true)];
    for (width, height, proc) in sizes.into_iter().chain([(60, 15, false)]) {
      for per_core_view in [false, true] {
        let mut app = test_app();
        app.cfg.per_core_view = per_core_view;

        let screen = render_to_string(&mut app, width, height);
        let ctx = format!("{width}x{height} per_core_view={per_core_view}");
        for label in ["M3 Pro · 6E+6P", "SYS  12.00W  fan 1200rpm", "q quit"] {
          assert!(screen.contains(label), "missing {label:?} ({ctx})");
        }
        for (_, markers) in PANEL_MARKERS {
          for marker in markers {
            assert!(screen.contains(marker), "missing {marker:?} ({ctx})");
          }
        }
        assert_eq!(screen.contains("cores  E ▄▄▄▄▄▄  P ▆▆▆▆▆▆"), per_core_view, "{ctx}");
        assert_eq!(screen.contains(" proc "), proc, "{ctx}");
      }
    }
  }

  #[test]
  fn panel_labels_follow_visibility() {
    for (key, _) in PANEL_MARKERS {
      let mut app = test_app();
      assert_eq!(app.handle_key(self::key(key)), ControlFlow::Continue(()));

      for (width, height) in [(200, 50), (80, 24)] {
        let screen = render_to_string(&mut app, width, height);
        for (other, other_markers) in PANEL_MARKERS {
          for marker in other_markers {
            let ctx = format!("panel {key} hidden, {marker:?} at {width}x{height}");
            assert_eq!(screen.contains(marker), other != key, "{ctx}");
          }
        }
      }
    }
  }

  #[test]
  fn metrics_title_has_chip_clock_and_version() {
    let is_clock = |word: &str| word.len() == 8 && word.chars().filter(|c| *c == ':').count() == 2;
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    let top = row(&buf, 0);
    assert!(top.starts_with("╭─ M3 Pro · 6E+6P · 18GPU · 36GB ─"), "{top}");
    let version = format!(" · macmon v{} · 1000ms ─╮", env!("CARGO_PKG_VERSION"));
    assert!(top.ends_with(&version), "{top}");
    let clock = top.trim_end_matches(&version).rsplit(' ').next().unwrap();
    assert!(is_clock(clock), "no clock before the version in {top}");

    // narrower: the version is dropped, the clock stays
    let top = row(&render_buffer(&mut app, 60, 15), 0);
    assert!(top.starts_with("╭─ M3 Pro · 6E+6P · 18GPU · 36GB ─"), "{top}");
    assert!(top.split_whitespace().any(is_clock) && !top.contains("macmon"), "{top}");

    // narrowest: the chip summary is cut, nothing else fits
    let top = row(&render_buffer(&mut app, 24, 15), 0);
    assert_eq!(top, "╭─ M3 Pro · 6E+6P · 18─╮");
  }

  /// Text of the power rows in a rendered frame of `app`.
  fn power_rows(app: &App, buf: &Buffer) -> Vec<String> {
    let power = app.layout(buf.area).power.expect("power rows");
    let text = |y| (power.left()..power.right()).map(|x| buf[(x, y)].symbol()).collect::<String>();
    (power.top()..power.bottom()).map(|y| text(y).trim_end().to_string()).collect()
  }

  fn is_braille(c: char) -> bool {
    ('\u{2801}'..='\u{28ff}').contains(&c)
  }

  #[test]
  fn power_column_rows() {
    let mut app = test_app_with(|m| {
      m.fans =
        (0..2).map(|i| FanMetric { name: format!("fan{i}"), rpm: 2000, max_rpm: None }).collect()
    });
    let buf = render_buffer(&mut app, 100, 30);
    let rows = power_rows(&app, &buf);

    // text with a history graph for CPU / GPU / ANE
    for (row, text) in rows.iter().zip(["CPU   4.50W  45°C ", "GPU   2.00W  40°C ", "ANE   0.10W "])
    {
      // the newest sample is in the last cell
      let graph: String = row.chars().skip(18).filter(|c| *c != ' ').collect();
      assert!(row.starts_with(text), "{row}");
      assert!(!graph.is_empty() && graph.chars().all(is_braille), "{row}");
      assert_eq!(row.chars().count(), 30, "{row}");
    }
    assert_eq!(rows[3], "SYS  12.00W  fans 2000/2000rpm");
    assert_eq!(rows[4], "all   6.60W avg 6.6 max 6.6");
    assert!(rows[5..].iter().all(String::is_empty));

    // 30 cells right of the strips, a separator line between them
    let power = app.layout(buf.area).power.unwrap();
    assert_eq!((power.x, power.width), (68, 30));
    for y in 1..11 {
      assert_eq!(buf[(66, y)].symbol(), "│", "row {y}");
    }
  }

  #[test]
  fn key_hints_degrade_at_narrow_widths() {
    let full = "q quit  d cores  r scaled  -/+ 1000ms  1-5 panels";
    let only_power = Panels { cpu: false, gpu: false, mem: false, power: true, proc: false };
    for width in [200, 120, 80, 60, 45, 30, 20, 14, 12] {
      let mut app = test_app();
      app.cfg.panels = only_power;
      let buf = render_buffer(&mut app, width, 24);
      let bottom = row(&buf, 23);
      let ctx = format!("width {width}: {bottom}");

      assert!(bottom.starts_with("╰─ q quit") && bottom.ends_with("─╯"), "{ctx}");
      // only whole hints are shown
      let shown = bottom.trim_start_matches("╰─ ").trim_end_matches(['─', '╯']).trim_end();
      assert!(full.starts_with(shown), "{ctx}");
      assert!(shown.len() == full.len() || full[shown.len()..].starts_with("  "), "{ctx}");
    }

    // too narrow for any hint: plain border
    let mut app = test_app();
    app.cfg.panels = only_power;
    let buf = render_buffer(&mut app, 11, 24);
    assert_eq!(row(&buf, 23), "╰─────────╯");
  }

  /// App with synthetic CPU clusters of `(label, cores per die, load)` on `dies` dies and the
  /// per-core row on.
  fn chip_app(clusters: &[(&str, usize, f32)], dies: usize) -> App {
    let cores: Vec<Vec<CpuCoreMetrics>> = clusters
      .iter()
      .map(|&(_, per_die, ratio)| {
        let die =
          move |die_id| (0..per_die).map(move |i| CpuCoreMetrics { die_id, ..core(i, ratio) });
        (0..dies).flat_map(die).collect()
      })
      .collect();
    let samples: Vec<ClusterSample> = clusters
      .iter()
      .zip(&cores)
      .map(|(&(label, _, ratio), cores)| {
        let aggregate = FreqSample::new(2000, ratio, ratio);
        ClusterSample { label, count: cores.len(), aggregate, cores }
      })
      .collect();

    let mut app = test_app();
    app.cfg.per_core_view = true;
    app.clusters = CpuClusters::default();
    for _ in 0..3 {
      app.clusters.push(&samples);
    }
    app
  }

  #[test]
  fn core_rows_for_real_chips() {
    // (name, clusters with idle / busy / half loaded cores, dies, cores lines at 72 and 100)
    type Lines = &'static [&'static str];
    type Chip = (&'static str, &'static [(&'static str, usize, f32)], usize, Lines, Lines);
    let chips: [Chip; 5] = [
      (
        "M1",
        &[("E", 4, 0.0), ("P", 4, 1.0)],
        1,
        &["cores  E ▁▁▁▁  P ████"],
        &["cores  E ▁▁▁▁  P ████"],
      ),
      (
        "M4 Max",
        &[("E", 4, 0.0), ("P", 12, 1.0)],
        1,
        &["cores  E ▁▁▁▁  P ████████████"],
        &["cores  E ▁▁▁▁  P ████████████"],
      ),
      (
        "M6",
        &[("E", 6, 0.0), ("P", 4, 1.0), ("S", 2, 0.5)],
        1,
        &["cores  E ▁▁▁▁▁▁  P ████  S ▅▅"],
        &["cores  E ▁▁▁▁▁▁  P ████  S ▅▅"],
      ),
      (
        "M3 Ultra",
        &[("E", 4, 0.0), ("P", 12, 1.0)],
        2,
        &["cores  D0 E ▁▁▁▁  P ████████████", "       D1 E ▁▁▁▁  P ████████████"],
        &["cores  E ▁▁▁▁▁▁▁▁  P ████████████████████████"],
      ),
      (
        "M5 Ultra",
        &[("P", 12, 0.0), ("S", 6, 1.0)],
        2,
        &["cores  D0 P ▁▁▁▁▁▁▁▁▁▁▁▁  S ██████", "       D1 P ▁▁▁▁▁▁▁▁▁▁▁▁  S ██████"],
        &["cores  P ▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁  S ████████████"],
      ),
    ];

    for (name, clusters, dies, narrow, wide) in chips {
      for (width, expected) in [(72, narrow), (100, wide)] {
        let mut app = chip_app(clusters, dies);
        let buf = render_buffer(&mut app, width, 30);
        let plan = app.layout(buf.area);
        let ctx = format!("{name} at {width}");

        // one line per die when everything doesn't fit on one line
        let text =
          |r: Rect| (r.left()..r.right()).map(|x| buf[(x, r.y)].symbol()).collect::<String>();
        let lines: Vec<String> =
          plan.cores.iter().map(|(_, r)| text(*r).trim_end().to_string()).collect();
        assert_eq!(lines, expected, "{ctx}");

        // every core bar is drawn, all of them left of the power column separator
        let top = plan.top.unwrap();
        let sep = plan.separator.expect("power column on the side");
        for (label, per_die, ratio) in clusters.iter() {
          let bar = core_bar(f64::from(*ratio));
          let cells: Vec<(u16, u16)> = (top.top()..top.bottom())
            .flat_map(|y| (top.left()..top.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() == bar)
            .collect();
          assert_eq!(cells.len(), per_die * dies, "{ctx}: {label} bars");
          assert!(cells.iter().all(|&(x, _)| x < sep.x), "{ctx}: {label} bars overflow");
        }

        // nothing drawn over the box borders or the separator
        for y in top.top() + 1..top.bottom() - 1 {
          let line = row(&buf, y);
          assert!(line.starts_with('│') && line.ends_with('│'), "{ctx}: {line}");
          assert_eq!(buf[(sep.x, y)].symbol(), "│", "{ctx}: {line}");
        }
      }
    }
  }

  #[test]
  fn three_clusters_get_strips_and_title() {
    let mut app = chip_app(&[("E", 6, 0.2), ("P", 4, 0.4), ("S", 2, 0.6)], 1);
    let screen = render_to_string(&mut app, 100, 30);
    for label in ["E-CPU  20% 2.0GHz", "P-CPU  40% 2.0GHz", "S-CPU  60% 2.0GHz"] {
      assert!(screen.contains(label), "missing {label}");
    }
    assert!(screen.contains("M3 Pro · 6E+4P+2S · 18GPU · 36GB"));

    // the strips stack in cluster order above GPU
    let plan = app.layout(Rect::new(0, 0, 100, 30));
    let strips: Vec<Strip> = plan.strips.iter().map(|(strip, _)| *strip).collect();
    use Strip::*;
    assert_eq!(strips, [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram, Swap]);
  }

  #[test]
  fn swap_row_hidden_without_swap() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 120, 40).contains("SWAP   50% 1/2G   ▰"));

    let mut app = test_app_with(|m| m.memory.swap_total = 0);
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("RAM    56% 20/36G ▰"));
    assert!(!screen.contains("SWAP"));
  }

  #[test]
  fn fans_and_sys_hidden_when_unavailable() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 80, 24);
    assert_eq!(power_rows(&app, &buf)[3], "SYS  12.00W  fan 1200rpm");

    // neither: the total follows ANE
    let mut app = test_app_with(|m| {
      m.fans.clear();
      m.sys_power = 0.0;
    });
    let buf = render_buffer(&mut app, 80, 24);
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!screen.contains("SYS") && !screen.contains("fan"));
    let rows = power_rows(&app, &buf);
    assert!(rows[2].starts_with("ANE   0.10W") && rows[3].starts_with("all   6.60W"), "{rows:?}");

    // only one of them: no gap left behind
    let mut app = test_app_with(|m| m.sys_power = 0.0);
    let buf = render_buffer(&mut app, 80, 24);
    assert_eq!(power_rows(&app, &buf)[3], "fan 1200rpm");
  }

  #[test]
  fn narrow_screen_puts_power_rows_under_strips() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 60, 15);
    let rows: Vec<String> = (1..14).map(|y| row(&buf, y)).collect();

    // no separator: power rows span the full width right under the SWAP strip
    assert!(rows.iter().all(|row| row.matches('│').count() == 2), "{rows:#?}");
    let swap = rows.iter().position(|row| row.starts_with("│ SWAP   50% 1/2G   ▰")).unwrap();
    assert!(rows[swap + 1].starts_with("│ CPU   4.50W  45°C "), "{}", rows[swap + 1]);
    assert!(rows[swap + 5].starts_with("│ all   6.60W avg 6.6 max 6.6 "), "{}", rows[swap + 5]);
    assert_eq!(swap + 5, rows.len() - 1, "power rows end at the bottom border");
  }

  #[test]
  fn keys_update_rendered_panels() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("E-CPU  42%") && screen.contains("GPU    23%"));
    assert!(!screen.contains("cores  E"), "per-core row is off by default");

    // r: active ratios
    assert!(app.handle_key(key('r')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E-CPU  50%") && screen.contains("P-CPU  80%"));
    assert!(screen.contains("GPU    30%") && screen.contains("r active"));

    // d: per-core row
    assert!(app.handle_key(key('d')).is_continue());
    assert!(render_to_string(&mut app, 120, 40).contains("cores  E ▄▄▄▄▄▄  P ▆▆▆▆▆▆"));

    // +/-: interval in the metrics title and the hints
    assert!(app.handle_key(key('+')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("· 1250ms ─╮") && screen.contains("-/+ 1250ms"));
    assert!(app.handle_key(key('-')).is_continue());
    assert!(app.handle_key(key('-')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("-/+ 750ms"));
  }

  #[test]
  fn digit_keys_toggle_panels() {
    let mut app = App::default();
    for c in ['1', '2', '3', '4', '5'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()));
    }
    assert_eq!(
      app.cfg.panels,
      Panels { cpu: false, gpu: false, mem: false, power: false, proc: false }
    );

    assert_eq!(app.handle_key(key('3')), ControlFlow::Continue(()));
    assert!(app.cfg.panels.mem);
    assert!(!app.cfg.panels.cpu && !app.cfg.panels.proc);

    // other digits don't touch the panels
    for c in ['0', '6', '9'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()));
    }
    assert!(app.cfg.panels.mem && !app.cfg.panels.gpu);
  }

  #[test]
  fn hidden_panels_are_not_rendered() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E-CPU") && screen.contains("1.4GHz"));

    for c in ['1', '2'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()));
    }
    let screen = render_to_string(&mut app, 200, 50);
    assert!(!screen.contains("E-CPU"), "cpu strips still shown");
    assert!(!screen.contains("1.4GHz"), "gpu strip still shown");
    assert!(screen.contains("RAM") && screen.contains("all   6.60W") && screen.contains(" proc "));
  }

  #[test]
  fn hidden_rows_shrink_metrics_box() {
    let area = Rect::new(0, 0, 60, 25);
    let top_height = |app: &App| app.layout(area).top.map(|r| r.height);
    let mut app = test_app();
    // 60 columns: 5 strips + 5 power rows under them need more than 40% of the height
    assert_eq!(top_height(&app), Some(12));

    // CPU strips, GPU strip (the last graph: the box keeps only the rows it needs), RAM / SWAP
    for (key, height) in [('1', 10), ('2', 9), ('3', 7)] {
      assert!(app.handle_key(self::key(key)).is_continue());
      assert_eq!(top_height(&app), Some(height), "after {key}");
      let proc = app.layout(area).proc.map(|r| r.height);
      assert_eq!(proc, Some(25 - height), "after {key}");
    }

    // every metric hidden: the process box takes the full height
    assert!(app.handle_key(key('4')).is_continue());
    let plan = app.layout(area);
    assert_eq!((plan.top, plan.proc), (None, Some(area)));
  }

  #[test]
  fn proc_panel_auto_hides_in_small_window() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 200, 50).contains(" proc "));
    assert!(render_to_string(&mut app, 80, 24).contains(" proc "), "width doesn't matter");
    assert!(!render_to_string(&mut app, 60, 15).contains(" proc "));
    assert!(!render_to_string(&mut app, 200, 12).contains(" proc "));
    assert!(app.cfg.panels.proc, "auto-hide must not change the config");

    // the only visible panel is never auto-hidden
    app.cfg.panels = Panels { proc: true, cpu: false, gpu: false, mem: false, power: false };
    let screen = render_to_string(&mut app, 60, 15);
    assert!(screen.contains(" proc ") && screen.contains("q quit"));
  }

  #[test]
  fn key_hints_on_bottom_box() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰─ q quit"), "hints on the process box");
    assert!(row(&buf, 19).starts_with("╰───"), "plain metrics box border");

    // without the process box the hints move to the metrics box
    app.cfg.panels.proc = false;
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰─ q quit"), "hints on the metrics box");
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(screen.matches("q quit").count(), 1);
  }

  #[test]
  fn all_panels_hidden_shows_hint() {
    let mut app = test_app();
    for c in ['1', '2', '3', '4', '5'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()));
    }

    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("all panels hidden · press 1-5 to show"));
    assert!(!screen.contains('╭'));
  }

  #[test]
  fn renders_any_size_and_panel_set() {
    let sizes = [
      (200, 50),
      (120, 40),
      (100, 30),
      (100, 20),
      (80, 24),
      (72, 24),
      (69, 24),
      (60, 15),
      (30, 8),
      (5, 3),
      (1, 1),
    ];
    let ultra = [("P", 12, 0.5), ("S", 6, 0.9)];
    for (width, height) in sizes {
      for bits in 0..64u8 {
        let mut app = if bits & 32 != 0 { chip_app(&ultra, 2) } else { test_app() };
        app.proc_view.set_procs(test_procs());
        app.proc_view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        app.cfg.per_core_view = bits % 3 == 0;
        app.cfg.panels = Panels {
          cpu: bits & 1 != 0,
          gpu: bits & 2 != 0,
          mem: bits & 4 != 0,
          power: bits & 8 != 0,
          proc: bits & 16 != 0,
        };
        render_buffer(&mut app, width, height);
      }
    }
  }

  #[test]
  fn graphs_are_braille() {
    let mut app = test_app();
    for _ in 0..40 {
      app.update_metrics(test_metrics());
    }

    for (width, height) in [(200, 50), (120, 40), (60, 15)] {
      let buf = render_buffer(&mut app, width, height);
      let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
      let ctx = format!("{width}x{height}");

      // strip and power graphs in braille, meters in ▰▱, no block characters (cores row off)
      assert!(screen.chars().filter(|c| is_braille(*c)).count() > 40, "{ctx}");
      let ram = screen.split("RAM    56% 20/36G ").nth(1).expect("ram strip");
      assert!(ram.starts_with('▰') && screen.contains('▱'), "{ctx}");
      let block = |c: char| ('▁'..='█').contains(&c) || c == '░';
      assert!(!screen.chars().any(block), "{ctx}");
      for row in &power_rows(&app, &buf)[..3] {
        let graph: String = row.chars().skip(18).filter(|c| *c != ' ').collect();
        assert!(!graph.is_empty() && graph.chars().all(is_braille), "{ctx}: {row}");
      }
    }
  }

  #[test]
  fn renders_without_metrics() {
    let mut app = App::default();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("all   0.00W") && screen.contains("RAM     0% 0/0G"));
    assert!(screen.contains("╭─ macmon ─"), "title without chip info");
    assert!(!screen.contains("°C") && !screen.contains("SYS") && !screen.contains("fan"));
  }

  fn test_procs() -> Vec<ProcInfo> {
    let proc = |pid: i32, name: &str| ProcInfo {
      pid,
      ppid: 1,
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
    render_buffer(&mut app, 60, 15);
    assert!(!procs_active(&app));
    assert!(app.cfg.panels.proc);
    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));

    // `5` toggles the panel; the flag follows on the next frame
    assert!(app.handle_key(key('5')).is_continue());
    assert!(procs_active(&app));
    render_buffer(&mut app, 200, 50);
    assert!(!procs_active(&app));
    assert!(app.handle_key(key('5')).is_continue());
    render_buffer(&mut app, 200, 50);
    assert!(procs_active(&app));

    // the only visible panel is never auto-hidden
    app.cfg.panels = Panels { proc: true, cpu: false, gpu: false, mem: false, power: false };
    render_buffer(&mut app, 60, 15);
    assert!(procs_active(&app));

    for c in ['5', '1'] {
      assert!(app.handle_key(key(c)).is_continue());
      render_buffer(&mut app, 200, 50);
      assert!(!procs_active(&app), "after {c}");
    }
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
    render_buffer(&mut app, 60, 15);
    assert_eq!(app.proc_view.procs(), None);
    app.update_procs(test_procs());
    assert_eq!(app.proc_view.procs(), None);

    // shown again: collecting until the next sample instead of stale rows
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    app.update_procs(test_procs());
    assert!(render_to_string(&mut app, 200, 50).contains(" proc 3 "));
  }

  /// App with the process panel on screen at 200x50 and `procs` in it.
  fn app_with_procs(procs: Vec<ProcInfo>) -> App {
    let mut app = test_app();
    render_buffer(&mut app, 200, 50);
    app.update_procs(procs);
    app
  }

  /// Screen row where the process box starts in a 200x50 window (60% of the height, full width).
  const PROC_Y: u16 = 20;

  /// Text of row `y` of the process box in a 200x50 window: 0 is the title, 1 the header.
  fn proc_row(buf: &Buffer, y: u16) -> String {
    row(buf, PROC_Y + y)
  }

  fn varied_procs() -> Vec<ProcInfo> {
    let proc = |pid: i32, name: &str, cpu, mem_mb: u64, power_w, gpu_pct| ProcInfo {
      pid,
      ppid: 1,
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
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);

    // title: count left, sort key right
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 3 ─"), "{title}");
    assert!(title.ends_with(" cpu ↓ ─╮"), "{title}");

    let header = proc_row(&buf, 1);
    let words: Vec<&str> = header.split_whitespace().collect();
    assert_eq!(words, ["│", "PID", "NAME", "USER", "CPU%", "MEM", "POWER", "GPU%", "│"]);

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
    assert!(rows[0].starts_with("│  631 WindowServer"), "{}", rows[0]);
    // one blank cell before the right border
    assert!(rows[0].ends_with("  25.0   300M  1.50W  40.0 │"), "{}", rows[0]);

    // gradient colors for load values, dim zeros and missing power
    let x_of = |row: &str, text: &str| row[..row.find(text).unwrap()].chars().count() as u16;
    let cell = |y: u16, row: &str, text: &str| buf[(x_of(row, text), PROC_Y + y)].fg;
    assert_eq!(cell(2, &rows[0], "25.0"), app.theme.gradient(0.25));
    assert_eq!(cell(2, &rows[0], "40.0"), app.theme.gradient(0.4));
    assert_eq!(cell(4, &rows[2], "-"), app.theme.dim);
    assert_eq!(cell(4, &rows[2], "0.0"), app.theme.dim);
    assert_eq!(cell(4, &rows[2], "launchd"), app.theme.text);
    // the sorted column header stands out
    assert_eq!(cell(1, &header, "CPU%"), app.theme.title);
    assert_eq!(cell(1, &header, "MEM"), app.theme.dim);
  }

  #[test]
  fn narrow_proc_panel_drops_columns() {
    let mut app = test_app();
    app.cfg.panels = Panels { proc: true, cpu: false, gpu: false, mem: false, power: false };
    render_buffer(&mut app, 40, 20);
    app.update_procs(varied_procs());

    let buf = render_buffer(&mut app, 40, 20);
    let header = row(&buf, 1);
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
    // NAME gets the 11 cells left: truncated
    assert!(row(&buf, 2).starts_with("│  631 WindowServe   25.0"), "{}", row(&buf, 2));

    // very narrow: PID and NAME only, nothing drawn over the border
    let buf = render_buffer(&mut app, 18, 20);
    assert_eq!(row(&buf, 1), "│  PID NAME      │");
    assert_eq!(row(&buf, 2), "│  631 WindowSer │");
  }

  #[test]
  fn proc_sort_keys_persist_in_config() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('s')).is_continue());
    assert_eq!(app.cfg.proc_sort, ProcSort::Mem);
    assert!(app.cfg.proc_sort_desc);

    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 0).ends_with(" mem ↓ ─╮"));
    assert!(proc_row(&buf, 2).contains("Safari"), "largest memory first");

    assert!(app.handle_key(key('S')).is_continue());
    assert!(!app.cfg.proc_sort_desc);
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 0).ends_with(" mem ↑ ─╮"));
    assert!(proc_row(&buf, 2).contains("launchd"));
  }

  #[test]
  fn typing_filter_ignores_global_keys() {
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());

    for c in ['q', 'c', 'v', 'd', 'r', '5', '+', '-', 's'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?} while typing");
    }
    assert_eq!(app.proc_view.filter(), "qcvdr5+-s");
    assert!(!app.cfg.per_core_view);
    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval, 1000);
    assert_eq!(app.cfg.panels, Panels::default());
    assert_eq!(app.cfg.proc_sort, ProcSort::Cpu);

    // the filter shows in the title with a cursor, no process matches it
    let buf = render_buffer(&mut app, 200, 50);
    let title = proc_row(&buf, 0);
    assert!(title.starts_with("╭─ proc 0/3 ─ /qcvdr5+-s█ ─"), "{title}");

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
    render_buffer(&mut app, 60, 15); // auto-hidden

    assert!(app.handle_key(key('/')).is_continue());
    assert!(!app.proc_view.typing());
    assert!(app.handle_key(key('s')).is_continue());
    assert_eq!(app.cfg.proc_sort, ProcSort::Cpu);
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    // hiding the panel while typing ends the input mode
    let mut app = app_with_procs(varied_procs());
    assert!(app.handle_key(key('/')).is_continue());
    assert!(app.proc_view.typing());
    render_buffer(&mut app, 60, 15);
    assert!(!app.proc_view.typing());
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));
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

  /// Global key hints on the bottom border, before the process hints.
  const GLOBAL_HINTS: &str = "╰─ q quit  d cores  r scaled  -/+ 1000ms  1-5 panels ─";

  #[test]
  fn proc_hints_follow_global_hints() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    let bottom = row(&buf, 49);
    assert!(bottom.starts_with(GLOBAL_HINTS), "{bottom}");
    let proc_hints = &bottom[GLOBAL_HINTS.len()..];
    assert!(proc_hints.starts_with(" / filter  s sort  S reverse  ↑↓ select ─"), "{bottom}");
    assert!(bottom.ends_with("─╯") && !bottom.contains("esc"), "{bottom}");

    // esc hint once there is something to clear
    assert!(app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).is_continue());
    assert!(row(&render_buffer(&mut app, 200, 50), 49).contains("↑↓ select  esc clear"));

    // input mode hints
    assert!(app.handle_key(key('/')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    let proc_hints = &bottom[GLOBAL_HINTS.len()..];
    assert!(proc_hints.starts_with(" enter keep  esc clear  ↑↓ select ─"), "{bottom}");
  }

  #[test]
  fn proc_hints_give_way_first() {
    let mut app = app_with_procs(varied_procs());
    // room for the first process hints only
    let bottom = row(&render_buffer(&mut app, 80, 40), 39);
    assert!(bottom.starts_with(&format!("{GLOBAL_HINTS} / filter  s sort ─")), "{bottom}");
    assert!(!bottom.contains("reverse"), "{bottom}");

    let bottom = row(&render_buffer(&mut app, 60, 40), 39);
    assert!(bottom.starts_with(GLOBAL_HINTS) && !bottom.contains("filter"), "{bottom}");

    // too narrow for the global hints: `q quit` stays, no process hints
    let bottom = row(&render_buffer(&mut app, 30, 40), 39);
    assert!(bottom.starts_with("╰─ q quit  d cores ─") && !bottom.contains("filter"));
  }

  #[test]
  fn procs_thread_samples_only_while_active() {
    let (tx, rx) = mpsc::channel();
    let active = Arc::new(AtomicBool::new(false));
    let thread = run_procs_thread(tx, Arc::new(RwLock::new(TUI_MIN_MS)), active.clone());

    // paused: a sample (baseline + warm-up + delta) would take longer than ~250 ms
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "sampled while paused");

    // active: the first message is already a delta sample with the own process in it
    active.store(true, Ordering::Relaxed);
    let Ok(Event::Procs(procs)) = rx.recv_timeout(Duration::from_secs(10)) else {
      panic!("no process sample");
    };
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

    // exits once the receiver is gone
    drop(rx);
    active.store(true, Ordering::Relaxed);
    thread.join().expect("process thread exits cleanly");
  }
}
