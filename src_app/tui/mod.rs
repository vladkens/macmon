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
      KeyCode::Char('r') => self.cfg.toggle_ratio_mode(),
      KeyCode::Char('+') => self.cfg.inc_interval(),
      KeyCode::Char('=') => self.cfg.inc_interval(), // fallback to press without shift
      KeyCode::Char('-') => self.cfg.dec_interval(),
      KeyCode::Char('p') => self.cfg.toggle_procs(),
      _ => {}
    }

    ControlFlow::Continue(())
  }

  fn render(&mut self, f: &mut Frame) {
    let plan = self.layout(f.area());
    self.set_procs_visible(plan.proc.is_some());

    self.render_metrics_box(f, &plan);
    if let Some(r) = plan.proc {
      self.render_proc_box(f, r);
    }
    if let Some(r) = plan.bottom() {
      self.render_key_hints(f, r, plan.proc.is_some());
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
    // can't turn into key presses; the palette only matters for a smooth (truecolor) gradient,
    // and SSH sessions skip the query (late replies)
    let truecolor = theme::detect_truecolor();
    let palette = if palette::should_query(truecolor) { palette::query_terminal() } else { None };
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

  use macmon::{FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
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
  use super::{App, Event, run_procs_thread};
  use crate::config::{ProcSort, RatioMode, TUI_MIN_MS};
  use crate::procs::ProcInfo;

  fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
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

  /// App with `samples` samples of `test_metrics` changed by `edit`.
  fn app_with_samples(samples: usize, edit: impl Fn(&mut Metrics)) -> App {
    let mut app = App { soc: test_soc(), ..Default::default() };
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

  fn is_braille(c: char) -> bool {
    ('\u{2801}'..='\u{28ff}').contains(&c)
  }

  #[test]
  fn quit_keys_break() {
    let mut app = App::default();
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
  }

  #[test]
  fn removed_keys_do_nothing() {
    // no theme or graph style switches, no cores row, no panel keys
    let mut app = app_with_procs(varied_procs());
    let cfg = serde_json::to_string(&app.cfg).unwrap();
    let theme = app.theme;
    let screen = render_to_string(&mut app, 200, 50);
    for c in ['c', 'v', 'd', 'C', 'V', 'D', '0', '1', '2', '3', '4', '5', '6', '9'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()), "{c:?}");
    }

    assert_eq!(serde_json::to_string(&app.cfg).unwrap(), cfg);
    assert_eq!(app.theme, theme);
    assert!(!app.proc_view.typing() && app.proc_view.filter().is_empty());
    assert_eq!(render_to_string(&mut app, 200, 50), screen);
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
  }

  #[test]
  fn p_toggles_process_list() {
    let mut app = app_with_procs(varied_procs());
    assert!(procs_active(&app));

    assert_eq!(app.handle_key(key('p')), ControlFlow::Continue(()));
    assert!(!app.cfg.show_procs);
    // saved with the other settings
    let json = serde_json::to_string(&app.cfg).unwrap();
    assert!(json.contains(r#""show_procs":false"#), "{json}");

    // the metrics box keeps its height, the hints move to its border without the process keys
    let buf = render_buffer(&mut app, 200, 50);
    let plan = app.layout(buf.area);
    assert_eq!((plan.top.map(|r| r.height), plan.proc), (Some(7), None));
    assert!(row(&buf, 6).ends_with("─ q quit | r scaled | -/+ 1000ms ─╯"), "{}", row(&buf, 6));
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!screen.contains(" proc ") && !screen.contains("WindowServer"));
    assert!((7..50).all(|y| row(&buf, y).trim().is_empty()), "nothing below the metrics");
    assert!(!procs_active(&app), "no process sampling while hidden");

    // shown again: collecting until the next sample
    assert_eq!(app.handle_key(key('p')), ControlFlow::Continue(()));
    assert!(app.cfg.show_procs);
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains(" proc ") && screen.contains("collecting…"));
    assert!(screen.contains("/ filter | s sort ─╯"));
    assert!(procs_active(&app));
  }

  /// Solarized-like terminal palette, as a terminal would answer the palette query.
  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  /// App with every kind of colored cell on screen: strips, power column, processes with a
  /// selected row.
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

  /// Strip and power row text of `test_metrics`.
  const METRIC_MARKERS: [&str; 10] = [
    "E-CPU  42% 1.8GHz ",
    "P-CPU  77% 3.2GHz ",
    "GPU    23% 1.4GHz ",
    "RAM    56% 20/36G ",
    "SWAP   50% 1/2G   ",
    "CPU    4.50W avg  4.50 max  4.50",
    "GPU    2.00W avg  2.00 max  2.00",
    "ANE    0.10W avg  0.10 max  0.10",
    "Power  6.60W avg  6.60 max  6.60",
    "Total 12.00W avg 12.00 max 12.00",
  ];

  #[test]
  fn renders_metrics_at_common_sizes() {
    // (width, height, process list shown)
    let sizes = [(200, 50, true), (120, 40, true), (100, 30, true), (80, 24, true), (72, 24, true)];
    for (width, height, proc) in sizes.into_iter().chain([(60, 15, false)]) {
      let mut app = test_app();
      let screen = render_to_string(&mut app, width, height);
      let ctx = format!("{width}x{height}");
      for label in METRIC_MARKERS.iter().chain(&["M3 Pro · 6E+6P", "fan 1200rpm", "q quit"]) {
        assert!(screen.contains(label), "missing {label:?} ({ctx})");
      }
      assert_eq!(screen.contains(" proc "), proc, "{ctx}");
    }
  }

  #[test]
  fn metrics_title_has_chip_and_version() {
    let mut app = test_app();
    let chip = "╭─ M3 Pro · 6E+6P · 18GPU · 36GB ─";
    let version = format!("─ macmon v{} ─╮", env!("CARGO_PKG_VERSION"));
    for width in [200, 100, 60] {
      let top = row(&render_buffer(&mut app, width, 30), 0);
      assert!(top.starts_with(chip) && top.ends_with(&version), "{top}");
      // no clock and no interval
      assert!(!top.contains(':') && !top.contains("ms"), "{top}");
    }

    // narrower: the version is dropped
    let top = row(&render_buffer(&mut app, 40, 30), 0);
    assert_eq!(top, format!("╭─ M3 Pro · 6E+6P · 18GPU · 36GB {}╮", "─".repeat(6)));

    // narrowest: the chip summary is cut, nothing else fits
    let top = row(&render_buffer(&mut app, 24, 15), 0);
    assert_eq!(top, "╭─ M3 Pro · 6E+6P · 18─╮");
  }

  #[test]
  fn matches_target_layout_at_100_columns() {
    // M3 Pro without swap, enough history to fill the strips
    let mut app = app_with_samples(60, |m| m.memory.swap_total = 0);
    render_buffer(&mut app, 100, 30);
    app.update_procs(varied_procs());
    let buf = render_buffer(&mut app, 100, 30);
    let rows: Vec<String> = (0..30).map(|y| row(&buf, y)).collect();
    let cells = |y: usize, skip: usize, take: usize| -> String {
      rows[y].chars().skip(skip).take(take).collect()
    };

    let version = format!("─ macmon v{} ─╮", env!("CARGO_PKG_VERSION"));
    assert!(rows[0].starts_with("╭─ M3 Pro · 6E+6P · 18GPU · 36GB ──"), "{}", rows[0]);
    assert!(rows[0].ends_with(&version), "{}", rows[0]);

    // strips (46 cells) and the power column (47 cells) with ` │ ` between them
    for y in 1..6 {
      let frame: Vec<String> = [0, 1, 48, 49, 50, 98, 99].map(|x| cells(y, x, 1)).into();
      assert_eq!(frame, ["│", " ", " ", "│", " ", " ", "│"], "{}", rows[y]);
    }
    let strips = [(1, "E-CPU  42% 1.8GHz "), (2, "P-CPU  77% 3.2GHz "), (3, "GPU    23% 1.4GHz ")];
    for (y, label) in strips {
      let strip = cells(y, 2, 46);
      assert!(strip.starts_with(label), "{strip}");
      assert!(strip.chars().skip(18).all(is_braille), "graph fills the strip: {strip}");
    }
    let ram = cells(4, 2, 46);
    assert!(ram.starts_with("RAM    56% 20/36G ▰") && ram.ends_with('▱'), "{ram}");
    assert_eq!(cells(5, 2, 46).trim(), "");

    let power = [
      "CPU    4.50W avg  4.50 max  4.50  45°C ",
      "GPU    2.00W avg  2.00 max  2.00  40°C ",
      "ANE    0.10W avg  0.10 max  0.10       ",
    ];
    for (y, text) in (1..).zip(power) {
      assert_eq!(cells(y, 51, 39), text);
      assert!(cells(y, 90, 8).chars().all(is_braille), "{}", rows[y]);
    }
    assert_eq!(cells(4, 51, 47).trim_end(), "Power  6.60W avg  6.60 max  6.60");
    assert_eq!(cells(5, 51, 47).trim_end(), "Total 12.00W avg 12.00 max 12.00  fan 1200rpm");
    assert_eq!(rows[6], format!("╰{}╯", "─".repeat(98)));

    // the process list in the rest of the screen, the hints on its bottom border
    assert!(rows[7].starts_with("╭─ proc 3 ─") && rows[7].ends_with(" cpu ↓ ─╮"), "{}", rows[7]);
    assert!(rows[8].starts_with("│   PID NAME "), "{}", rows[8]);
    assert!(rows[9].starts_with("│   631 WindowServer "), "{}", rows[9]);
    let hints = "─ q quit | r scaled | -/+ 1000ms | / filter | s sort ─╯";
    assert!(rows[29].starts_with("╰───") && rows[29].ends_with(hints), "{}", rows[29]);
  }

  /// Text of the power rows in a rendered frame of `app`.
  fn power_rows(app: &App, buf: &Buffer) -> Vec<String> {
    let power = app.layout(buf.area).power.expect("power rows");
    let line = |y| text(buf, Rect { y, height: 1, ..power });
    (power.top()..power.bottom()).map(|y| line(y).trim_end().to_string()).collect()
  }

  #[test]
  fn power_column_rows() {
    let mut app = test_app_with(|m| {
      m.fans =
        (0..2).map(|i| FanMetric { name: format!("fan{i}"), rpm: 2000, max_rpm: None }).collect()
    });
    let buf = render_buffer(&mut app, 100, 30);
    let rows = power_rows(&app, &buf);

    // numbers, temperature and a history graph for CPU / GPU / ANE, the graphs line up
    let units = [
      "CPU    4.50W avg  4.50 max  4.50  45°C ",
      "GPU    2.00W avg  2.00 max  2.00  40°C ",
      "ANE    0.10W avg  0.10 max  0.10       ",
    ];
    for (row, text) in rows.iter().zip(units) {
      // the newest sample is in the last cell
      let graph: String = row.chars().skip(39).filter(|c| *c != ' ').collect();
      assert!(row.starts_with(text), "{row}");
      assert!(!graph.is_empty() && graph.chars().all(is_braille), "{row}");
      assert_eq!(row.chars().count(), 51, "{row}");
    }
    // Power, then Total with both fans: the column is as wide as that row
    assert_eq!(rows[3], "Power  6.60W avg  6.60 max  6.60");
    assert_eq!(rows[4], "Total 12.00W avg 12.00 max 12.00  fans 2000/2000rpm");
    assert_eq!(rows.len(), 5);

    // right of the strips, a separator line between them
    let power = app.layout(buf.area).power.unwrap();
    assert_eq!((power.x, power.width), (47, 51));
    for y in 1..6 {
      assert_eq!(buf[(45, y)].symbol(), "│", "row {y}");
    }
  }

  /// App whose power samples differ, so the current value (the mean of the last two samples),
  /// average and maximum differ too: CPU 5 / 4 / 6 W, GPU 2.5 / 2 / 3, ANE 0.25 / 0.2 / 0.3,
  /// Power 9.5 / 8 / 12, Total 16 / 14 / 20.
  fn varied_power_app() -> App {
    let mut app = App { soc: test_soc(), ..Default::default() };
    let samples =
      [(2.0, 1.0, 0.1, 10.0, 5.0), (4.0, 2.0, 0.2, 12.0, 7.0), (6.0, 3.0, 0.3, 20.0, 12.0)];
    for (cpu_power, gpu_power, ane_power, sys_power, all_power) in samples {
      let metrics =
        Metrics { cpu_power, gpu_power, ane_power, sys_power, all_power, ..test_metrics() };
      app.update_metrics(metrics);
    }
    app
  }

  /// Power rows of `varied_power_app` with every part shown, without the history graphs.
  const VARIED_POWER_ROWS: [&str; 5] = [
    "CPU    5.00W avg  4.00 max  6.00  45°C",
    "GPU    2.50W avg  2.00 max  3.00  40°C",
    "ANE    0.25W avg  0.20 max  0.30",
    "Power  9.50W avg  8.00 max 12.00",
    "Total 16.00W avg 14.00 max 20.00  fan 1200rpm",
  ];

  /// Power row text without the history graph.
  fn without_graph(row: &str) -> &str {
    row.trim_end_matches(|c: char| is_braille(c) || c == ' ')
  }

  #[test]
  fn power_rows_show_avg_and_max() {
    for (width, height) in [(200, 50), (120, 40)] {
      let mut app = varied_power_app();
      let buf = render_buffer(&mut app, width, height);
      let rows = power_rows(&app, &buf);
      let ctx = format!("{width}x{height}: {rows:#?}");

      // CPU / GPU / ANE: current, average, maximum, temperature, then at least 8 graph cells
      for (row, text) in rows.iter().zip(&VARIED_POWER_ROWS[..3]) {
        assert_eq!(without_graph(row), *text, "{ctx}");
        let graph: String = row.chars().skip(39).collect();
        assert!(graph.chars().count() >= 8, "{ctx}");
        assert!(graph.trim_start().chars().all(is_braille), "{ctx}");
      }
      // Power, Total with the fan on its row
      assert_eq!(rows[3..5], VARIED_POWER_ROWS[3..], "{ctx}");
    }
  }

  #[test]
  fn narrow_power_column_drops_graph_then_temp_then_stats() {
    // power rows with or without average / maximum, temperatures and the fans on the Total row
    let expected = |stats: bool, temp: bool, inline: bool| {
      let row = |head: &str, avg_max: &str, celsius: &str| {
        let avg_max = if stats { avg_max } else { "" };
        let celsius = if temp { celsius } else { "" };
        format!("{head}{avg_max}{celsius}")
      };
      let mut rows = vec![
        row("CPU    5.00W", " avg  4.00 max  6.00", "  45°C"),
        row("GPU    2.50W", " avg  2.00 max  3.00", "  40°C"),
        row("ANE    0.25W", " avg  0.20 max  0.30", ""),
        row("Power  9.50W", " avg  8.00 max 12.00", ""),
      ];
      let total = row("Total 16.00W", " avg 14.00 max 20.00", "");
      match inline {
        true => rows.push(format!("{total}  fan 1200rpm")),
        false => rows.extend([total, "fan 1200rpm".to_string()]),
      }
      rows
    };

    // (screen size, power column width, average / maximum, temperatures, graphs, fans inline)
    let cases = [
      // next to the strips: the narrowest column with everything, then graphs go first, the fan
      // moves to a row of its own, temperatures go, the numbers stay
      ((90, 24), 47, true, true, true, true),
      ((89, 24), 46, true, true, false, true),
      ((88, 24), 45, true, true, false, true),
      ((87, 24), 44, true, true, false, false),
      ((81, 24), 38, true, true, false, false),
      ((80, 24), 37, true, false, false, false),
      ((72, 24), 32, true, false, false, false),
      // under the strips: everything fits at 60 columns, then the same order
      ((60, 15), 56, true, true, true, true),
      ((50, 24), 46, true, true, false, true),
      ((48, 24), 44, true, true, false, false),
      ((42, 24), 38, true, true, false, false),
      ((41, 24), 37, true, false, false, false),
      ((36, 24), 32, true, false, false, false),
      ((35, 24), 31, false, false, false, false),
    ];
    for ((width, height), column, stats, temp, graph, inline) in cases {
      let mut app = varied_power_app();
      let buf = render_buffer(&mut app, width, height);
      let plan = app.layout(buf.area);
      let rows = power_rows(&app, &buf);
      let ctx = format!("{width}x{height}: {rows:#?}");
      assert_eq!(plan.power.map(|r| r.width), Some(column), "{ctx}");

      // whole parts only: no number is cut by the column edge
      let shown: Vec<&str> = rows.iter().map(|row| without_graph(row)).collect();
      let shown: Vec<&str> = shown.into_iter().filter(|row| !row.is_empty()).collect();
      assert_eq!(shown, expected(stats, temp, inline), "{ctx}");
      for row in &rows[..3] {
        assert_eq!(row.chars().any(is_braille), graph, "{ctx}");
      }

      // nothing drawn over the padding, the separator or the box borders
      let top = plan.top.unwrap();
      for y in top.top() + 1..top.bottom() - 1 {
        let line = row(&buf, y);
        assert!(line.starts_with("│ ") && line.ends_with(" │"), "{ctx}: {line}");
        if let Some(sep) = plan.separator {
          let gap = [sep.x - 1, sep.x, sep.x + 1].map(|x| buf[(x, y)].symbol());
          assert_eq!(gap, [" ", "│", " "], "{ctx}: {line}");
        }
      }
    }
  }

  /// Key hints with the process list on screen.
  const HINTS: [&str; 5] = ["q quit", "r scaled", "-/+ 1000ms", "/ filter", "s sort"];

  /// Bottom border of a box `width` cells wide with the first `count` of `HINTS` right-aligned.
  fn hints_border(width: usize, count: usize) -> String {
    if count == 0 {
      return format!("╰{}╯", "─".repeat(width - 2));
    }
    let hints = format!(" {} ", HINTS[..count].join(" | "));
    format!("╰{}{hints}─╯", "─".repeat(width - 3 - hints.chars().count()))
  }

  #[test]
  fn key_hints_right_aligned_on_bottom_border() {
    let mut app = app_with_procs(varied_procs());
    let buf = render_buffer(&mut app, 200, 50);
    assert_eq!(row(&buf, 49), hints_border(200, 5));
    assert_eq!(row(&buf, 6), hints_border(200, 0), "plain metrics box border");
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(screen.matches("q quit").count(), 1);

    // keys bold, labels plain, separators dim
    let bottom = row(&buf, 49);
    let x = |text: &str| bottom[..bottom.find(text).unwrap()].chars().count() as u16;
    let cell = |x: u16| &buf[(x, 49)];
    assert!(cell(x("q quit")).modifier.contains(Modifier::BOLD));
    assert!(!cell(x("quit")).modifier.contains(Modifier::BOLD));
    assert_eq!(cell(x("quit")).fg, app.theme.text);
    assert_eq!(cell(x("| r")).fg, app.theme.dim);

    // the current ratio mode and interval
    assert!(app.handle_key(key('r')).is_continue());
    assert!(app.handle_key(key('+')).is_continue());
    let bottom = row(&render_buffer(&mut app, 200, 50), 49);
    assert!(bottom.contains(" q quit | r active | -/+ 1250ms | / filter "), "{bottom}");

    // without the process list: on the metrics box, without the process keys
    app.cfg.show_procs = false;
    assert!(app.handle_key(key('-')).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 6).ends_with("─ q quit | r active | -/+ 1000ms ─╯"), "{}", row(&buf, 6));
  }

  #[test]
  fn key_hints_drop_from_the_end_when_narrow() {
    // (width, hints shown): `q quit` 6 cells, then 3 cells between hints and 1 at both ends,
    // plus `╰─` and `─╯`
    let cases = [(200, 5), (56, 5), (55, 4), (47, 4), (46, 3), (36, 3), (35, 2), (23, 2), (22, 1)];
    for (width, count) in cases.into_iter().chain([(12, 1), (11, 0), (5, 0)]) {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, 60);
      let proc = app.layout(buf.area).proc.expect("process box");
      assert_eq!(row(&buf, proc.bottom() - 1), hints_border(width.into(), count), "{width}");
    }

    // without the process list only the first three are there to show
    for (width, count) in [(200, 3), (46, 3), (35, 2), (22, 1), (11, 0)] {
      let mut app = test_app();
      app.cfg.show_procs = false;
      let buf = render_buffer(&mut app, width, 60);
      let top = app.layout(buf.area).top.expect("metrics box");
      assert_eq!(row(&buf, top.bottom() - 1), hints_border(width.into(), count), "{width}");
    }
  }

  /// App with synthetic CPU clusters of `(label, cores, load)`.
  fn clusters_app(clusters: &[(&str, usize, f32)]) -> App {
    let samples: Vec<ClusterSample> = clusters
      .iter()
      .map(|&(label, count, ratio)| ClusterSample {
        label,
        count,
        aggregate: FreqSample::new(2000, ratio, ratio),
      })
      .collect();

    let mut app = test_app();
    app.clusters = CpuClusters::default();
    for _ in 0..3 {
      app.clusters.push(&samples);
    }
    app
  }

  #[test]
  fn three_clusters_get_strips_and_title() {
    let mut app = clusters_app(&[("E", 6, 0.2), ("P", 4, 0.4), ("S", 2, 0.6)]);
    let screen = render_to_string(&mut app, 100, 30);
    for label in ["E-CPU  20% 2.0GHz", "P-CPU  40% 2.0GHz", "S-CPU  60% 2.0GHz"] {
      assert!(screen.contains(label), "missing {label}");
    }
    assert!(screen.contains("M3 Pro · 6E+4P+2S · 18GPU · 36GB"));

    // the strips stack in cluster order above GPU, one row more than the power rows
    let plan = app.layout(Rect::new(0, 0, 100, 30));
    let strips: Vec<Strip> = plan.strips.iter().map(|(strip, _)| *strip).collect();
    use Strip::*;
    assert_eq!(strips, [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram, Swap]);
    assert_eq!(plan.top.map(|r| r.height), Some(8));
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
  fn fans_and_total_hidden_when_unavailable() {
    // 80 columns: the fan doesn't fit after Total, it gets a row of its own
    let mut app = test_app();
    let buf = render_buffer(&mut app, 80, 24);
    let rows = power_rows(&app, &buf);
    assert!(rows[3].starts_with("Power  6.60W"), "{rows:?}");
    assert_eq!(rows[4..6], ["Total 12.00W avg 12.00 max 12.00", "fan 1200rpm"]);

    // neither: Power is the last row
    let mut app = test_app_with(|m| {
      m.fans.clear();
      m.sys_power = 0.0;
    });
    let buf = render_buffer(&mut app, 80, 24);
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!screen.contains("Total") && !screen.contains("fan"));
    let rows = power_rows(&app, &buf);
    assert!(rows[2].starts_with("ANE    0.10W") && rows[3].starts_with("Power  6.60W"), "{rows:?}");
    assert!(rows[4..].iter().all(String::is_empty), "{rows:?}");

    // only one of them: no gap left behind
    let mut app = test_app_with(|m| m.sys_power = 0.0);
    let buf = render_buffer(&mut app, 80, 24);
    assert_eq!(power_rows(&app, &buf)[4], "fan 1200rpm");
  }

  #[test]
  fn narrow_screen_puts_power_rows_under_strips() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 60, 15);
    let rows: Vec<String> = (0..15).map(|y| row(&buf, y)).collect();

    // no separator: power rows span the full width right under the SWAP strip
    assert!(rows[1..11].iter().all(|row| row.matches('│').count() == 2), "{rows:#?}");
    assert!(rows[5].starts_with("│ SWAP   50% 1/2G   ▰"), "{}", rows[5]);
    assert!(rows[6].starts_with("│ CPU    4.50W avg  4.50 max  4.50  45°C "), "{}", rows[6]);
    assert!(rows[6].chars().any(is_braille), "{}: room for the graph", rows[6]);
    assert!(rows[9].starts_with("│ Power  6.60W avg  6.60 max  6.60 "), "{}", rows[9]);
    let total = "│ Total 12.00W avg 12.00 max 12.00  fan 1200rpm ";
    assert!(rows[10].starts_with(total), "{}", rows[10]);

    // the box ends right after them, the process list is auto-hidden
    assert!(rows[11].starts_with("╰─"), "{}", rows[11]);
    assert!(rows[12..].iter().all(|row| row.trim().is_empty()), "{rows:#?}");
  }

  #[test]
  fn keys_update_rendered_metrics() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("E-CPU  42%") && screen.contains("GPU    23%"));

    // r: active ratios
    assert!(app.handle_key(key('r')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E-CPU  50%") && screen.contains("P-CPU  80%"));
    assert!(screen.contains("GPU    30%") && screen.contains("r active"));

    // +/-: interval in the hints only
    assert!(app.handle_key(key('+')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("-/+ 1250ms") && screen.matches("1250ms").count() == 1);
    assert!(app.handle_key(key('-')).is_continue());
    assert!(app.handle_key(key('-')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("-/+ 750ms"));
  }

  #[test]
  fn metrics_box_fits_content_and_procs_take_the_rest() {
    // (width, height, metrics box height): 5 strips next to 5 power rows; at 80 columns the fan
    // moves to a row of its own
    for (width, height, top) in [(200, 50, 7), (120, 40, 7), (80, 24, 8)] {
      let mut app = test_app();
      let buf = render_buffer(&mut app, width, height);
      let plan = app.layout(buf.area);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(Rect::new(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(Rect::new(0, top, width, height - top)), "{ctx}");

      // no blank rows in the metrics box
      let rows = power_rows(&app, &buf);
      assert!(rows.iter().all(|row| !row.is_empty()), "{ctx}: {rows:#?}");
      assert!(plan.strips.last().is_some_and(|(_, r)| r.bottom() < top), "{ctx}");
      assert!(row(&buf, top).starts_with("╭─ proc"), "{ctx}");
    }
  }

  #[test]
  fn proc_panel_auto_hides_in_small_window() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 200, 50).contains(" proc "));
    assert!(render_to_string(&mut app, 80, 24).contains(" proc "), "width doesn't matter");
    assert!(!render_to_string(&mut app, 60, 15).contains(" proc "));
    assert!(!render_to_string(&mut app, 200, 12).contains(" proc "));
    assert!(render_to_string(&mut app, 200, 13).contains(" proc "));
    assert!(app.cfg.show_procs, "auto-hide must not change the config");
  }

  #[test]
  fn renders_any_size() {
    let sizes = [
      (400, 120),
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

      // strip and power graphs in braille, meters in ▰▱, no block characters
      assert!(screen.chars().filter(|c| is_braille(*c)).count() > 40, "{ctx}");
      let ram = screen.split("RAM    56% 20/36G ").nth(1).expect("ram strip");
      assert!(ram.starts_with('▰') && screen.contains('▱'), "{ctx}");
      let block = |c: char| ('▁'..='█').contains(&c) || c == '░';
      assert!(!screen.chars().any(block), "{ctx}");
      for row in &power_rows(&app, &buf)[..3] {
        let graph: String = row.chars().skip(39).filter(|c| *c != ' ').collect();
        assert!(!graph.is_empty() && graph.chars().all(is_braille), "{ctx}: {row}");
      }
    }
  }

  #[test]
  fn graphs_fill_strips_once_history_is_long_enough() {
    // cells of the graph strips (clusters and GPU) with braille in them, and the graph width
    let graph_cells = |app: &mut App, width: u16| -> Vec<(usize, usize)> {
      let buf = render_buffer(app, width, 50);
      let plan = app.layout(buf.area);
      let graphs = plan.strips.iter().filter(|(s, _)| matches!(s, Strip::Cluster(_) | Strip::Gpu));
      let cells = |r: &Rect| {
        let text = text(&buf, *r);
        (text.chars().filter(|c| is_braille(*c)).count(), r.width as usize - 18)
      };
      graphs.map(|(_, r)| cells(r)).collect()
    };

    // 3 samples: only the newest cells on the right
    let mut app = test_app();
    for (braille, _) in graph_cells(&mut app, 200) {
      assert_eq!(braille, 2);
    }

    // a long history fills the strips of a wide terminal, newest sample on the right
    for _ in 0..700 {
      app.update_metrics(test_metrics());
    }
    assert_eq!(app.igpu_freq.ratio(RatioMode::Scaled).items.len(), 703);
    // (screen width, graph cells): 64 cells was all the old 128 sample history could fill
    for (width, graph) in [(100, 28), (200, 128), (400, 328)] {
      let cells = graph_cells(&mut app, width);
      assert_eq!(cells, [(graph, graph); 3], "width {width}");
    }
  }

  #[test]
  fn renders_without_metrics() {
    let mut app = App::default();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("Power  0.00W") && screen.contains("RAM    0% 0/0G"));
    assert!(screen.contains("╭─ macmon ─"), "title without chip info");
    assert!(!screen.contains("°C") && !screen.contains("Total") && !screen.contains("fan"));
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

  /// Screen row where the process box starts in a 200x50 window: right under the 7 rows of the
  /// metrics box.
  const PROC_Y: u16 = 7;

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
    // one blank cell after the left border and before the right one
    assert!(header.starts_with("│   PID NAME "), "{header}");
    assert!(rows[0].starts_with("│   631 WindowServer"), "{}", rows[0]);
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
    render_buffer(&mut app, 40, 20);
    app.update_procs(varied_procs());

    // the process box under the 13 rows of metrics (power rows under the strips)
    let buf = render_buffer(&mut app, 40, 20);
    assert_eq!(app.layout(buf.area).proc, Some(Rect::new(0, 13, 40, 7)));
    let header = row(&buf, 14);
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
    // NAME gets the 10 cells left: truncated
    assert!(row(&buf, 15).starts_with("│   631 WindowServ   25.0"), "{}", row(&buf, 15));

    // very narrow: PID and NAME only, nothing drawn over the border
    let buf = render_buffer(&mut app, 18, 20);
    assert_eq!(row(&buf, 14), "│   PID NAME     │");
    assert_eq!(row(&buf, 15), "│   631 WindowSe │");
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

    // End: the last process is on the last row (42 is the border); 40 rows on screen
    assert!(app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 41).contains("proc99 "));
    assert!(selected(&buf, 1, 41));
    assert!(proc_row(&buf, 2).contains("proc60 "), "{}", proc_row(&buf, 2));

    // esc clears the selection, the table goes back to the top
    assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_continue());
    let buf = render_buffer(&mut app, 200, 50);
    assert!(proc_row(&buf, 2).contains("proc0 "));
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
