//! Terminal user interface.

mod layout;
mod panels;
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
use layout::compute_layout;
use macmon::{Metrics, Sampler, SocInfo};
use panels::Titles;
use store::{CpuFreqStore, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};
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

  ecpu_freq: CpuFreqStore,
  pcpu_freq: CpuFreqStore,
  igpu_freq: FreqStore,

  /// Latest process list, `None` until the first sample with rates arrives.
  procs: Option<Vec<ProcInfo>>,
  /// Set while the process panel is on screen; the process thread samples only then.
  procs_active: Arc<AtomicBool>,
}

impl App {
  pub fn new() -> WithError<Self> {
    let soc = SocInfo::new()?;
    let cfg = Config::load();
    let theme = Theme::new(&cfg.theme, theme::detect_truecolor());
    Ok(Self { cfg, theme, soc, ..Default::default() })
  }

  fn update_metrics(&mut self, data: Metrics) {
    self.cpu_power.push(data.cpu_power as f64);
    self.gpu_power.push(data.gpu_power as f64);
    self.ane_power.push(data.ane_power as f64);
    self.all_power.push(data.all_power as f64);
    self.sys_power.push(data.sys_power as f64);

    let ecpu = FreqSample::new(data.ecpu_freq_mhz, data.ecpu_scaled_ratio, data.ecpu_active_ratio);
    let pcpu = FreqSample::new(data.pcpu_freq_mhz, data.pcpu_scaled_ratio, data.pcpu_active_ratio);
    let igpu = FreqSample::new(data.gpu_freq_mhz, data.gpu_scaled_ratio, data.gpu_active_ratio);

    self.ecpu_freq.push(ecpu, &data.ecpu_cores);
    self.pcpu_freq.push(pcpu, &data.pcpu_cores);
    self.igpu_freq.push(igpu);

    self.cpu_temp.push(data.temp.cpu_temp_avg);
    self.gpu_temp.push(data.temp.gpu_temp_avg);
    self.fans.push(data.fans);

    self.mem.push(data.memory);
  }

  /// Follows the process panel visibility (toggled or auto-hidden). A hidden panel drops its
  /// list, so it reads "collecting…" when shown again instead of showing stale rows.
  fn set_procs_visible(&mut self, visible: bool) {
    self.procs_active.store(visible, Ordering::Relaxed);
    if !visible {
      self.procs = None;
    }
  }

  /// Stores a process sample; one still in flight when the panel got hidden is dropped.
  fn update_procs(&mut self, procs: Vec<ProcInfo>) {
    if self.procs_active.load(Ordering::Relaxed) {
      self.procs = Some(procs);
    }
  }

  /// Applies a key press to the app state. Returns `Break` when the app should quit.
  fn handle_key(&mut self, key: KeyEvent) -> ControlFlow<()> {
    match key.code {
      KeyCode::Char('q') => return ControlFlow::Break(()),
      KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => return ControlFlow::Break(()),
      KeyCode::Char('c') => {
        self.theme = self.theme.next();
        self.cfg.set_theme(self.theme.name);
      }
      KeyCode::Char('v') => self.cfg.next_view_type(),
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

  /// Process panel: "collecting…" until the first sample with rates arrives.
  fn render_proc_box(&self, f: &mut Frame, area: Rect) {
    let inner = self.draw_box(f, area, Titles::new(self.heading("proc")));
    let text = match &self.procs {
      Some(procs) => format!("{} processes", procs.len()),
      None => "collecting…".to_string(),
    };

    let row = inner.centered_vertically(Constraint::Length(1));
    f.render_widget(Line::from(Span::styled(text, self.theme.dim)).centered(), row);
  }

  fn render(&mut self, f: &mut Frame) {
    let plan = compute_layout(f.area(), self.cfg.panels, self.cfg.per_core_view);
    self.set_procs_visible(plan.proc.is_some());

    self.render_cpu_box(f, &plan);

    if let Some(r) = plan.gpu {
      self.render_gpu_box(f, r);
    }

    if let Some(r) = plan.mem {
      self.render_mem_box(f, r);
    }

    if let Some(r) = plan.power {
      self.render_power_box(f, r);
    }

    if let Some(r) = plan.proc {
      self.render_proc_box(f, r);
    }

    match plan.bottom_left() {
      Some(r) => self.render_key_hints(f, r),
      None => self.render_all_hidden(f, f.area()),
    }
  }

  pub fn run_loop(&mut self, interval: Option<u32>) -> WithError<()> {
    // use from arg if provided, otherwise use config restored value
    self.cfg.interval = interval.unwrap_or(self.cfg.interval).clamp(TUI_MIN_MS, TUI_MAX_MS);
    let msec = Arc::new(RwLock::new(self.cfg.interval));

    let (tx, rx) = mpsc::channel::<Event>();
    run_inputs_thread(tx.clone(), 250);
    run_sampler_thread(tx.clone(), msec.clone());
    run_procs_thread(tx.clone(), msec.clone(), self.procs_active.clone());

    let mut term = enter_term();

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
  use ratatui::style::Color;

  use super::theme::{THEMES, Theme};
  use super::{App, Event, run_procs_thread};
  use crate::config::{Panels, RatioMode, TUI_MIN_MS, ViewType};
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

  /// Text of the box row starting at the screen row containing `marker`.
  fn row_with(buf: &Buffer, marker: &str) -> Option<String> {
    (0..buf.area.height).map(|y| row(buf, y)).find(|row| row.contains(marker))
  }

  #[test]
  fn quit_keys_break() {
    let mut app = App::default();
    assert_eq!(app.handle_key(key('q')), ControlFlow::Break(()));

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), ControlFlow::Break(()));
    // ctrl-c doesn't change the theme
    assert_eq!(app.theme.name, "default");
    assert_eq!(app.cfg.theme, "default");
  }

  #[test]
  fn c_cycles_themes() {
    let mut app = App::default();
    assert_eq!(app.theme.name, "default");

    assert_eq!(app.handle_key(key('c')), ControlFlow::Continue(()));
    assert_eq!(app.theme.name, "nord");
    assert_eq!(app.cfg.theme, "nord");

    for _ in 1..THEMES.len() {
      assert_eq!(app.handle_key(key('c')), ControlFlow::Continue(()));
    }
    assert_eq!(app.theme.name, "default");
    assert_eq!(app.cfg.theme, "default");
  }

  #[test]
  fn v_toggles_view_type() {
    let mut app = App::default();
    assert_eq!(app.cfg.view_type, ViewType::Braille);
    assert_eq!(app.handle_key(key('v')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.view_type, ViewType::Block);
    assert_eq!(app.handle_key(key('v')), ControlFlow::Continue(()));
    assert_eq!(app.cfg.view_type, ViewType::Braille);
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

    assert_eq!(app.theme.name, "default");
    assert_eq!(app.cfg.view_type, ViewType::Braille);
    assert!(!app.cfg.per_core_view);
    assert_eq!(app.cfg.ratio_mode, RatioMode::Scaled);
    assert_eq!(app.cfg.interval, 1000);
  }

  #[test]
  fn renders_with_every_theme() {
    for truecolor in [true, false] {
      for theme in THEMES {
        for view_type in [ViewType::Braille, ViewType::Block] {
          let mut app = test_app();
          app.theme = Theme::new(theme.name, truecolor);
          app.cfg.view_type = view_type;

          let buf = render_buffer(&mut app, 120, 40);
          // top-left corner is the outer box border
          assert_eq!(buf[(0, 0)].symbol(), "╭");
          assert_eq!(buf[(0, 0)].fg, app.theme.border, "theme {}", theme.name);

          let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
          assert!(screen.contains(&format!("c {}", theme.name)));
          if !truecolor {
            let is_rgb = |c: Color| matches!(c, Color::Rgb(..));
            assert!(buf.content.iter().all(|cell| !is_rgb(cell.fg) && !is_rgb(cell.bg)));
          }
        }
      }
    }
  }

  /// Text that only the given panel renders (with `test_metrics`).
  const PANEL_MARKERS: [(char, &[&str]); 4] = [
    ('1', &["E-CPU 42% @ 1800 MHz", "P-CPU 77% @ 3200 MHz", "cpu 45°C", "Apple M3 Pro"]),
    ('2', &["gpu 23% @ 1400 MHz · 40°C"]),
    ('3', &[" mem ", "RAM  20.00/36.0 GB"]),
    ('4', &["power 6.60W", "ANE"]),
  ];

  #[test]
  fn renders_metric_panels_at_common_sizes() {
    // (width, height, process panel shown)
    for (width, height, proc) in
      [(200, 50, true), (120, 40, true), (80, 24, false), (60, 15, false)]
    {
      for per_core_view in [false, true] {
        for view_type in [ViewType::Braille, ViewType::Block] {
          let mut app = test_app();
          app.cfg.per_core_view = per_core_view;
          app.cfg.view_type = view_type;

          let screen = render_to_string(&mut app, width, height);
          let ctx = format!("{width}x{height} per_core_view={per_core_view} {view_type:?}");
          for label in ["E-CPU", "P-CPU", "GPU", "RAM", "ANE", "CPU", "45°C", "40°C", "q quit"] {
            assert!(screen.contains(label), "missing {label:?} ({ctx})");
          }
          for (_, markers) in PANEL_MARKERS {
            for marker in markers {
              assert!(screen.contains(marker), "missing {marker:?} ({ctx})");
            }
          }
          assert_eq!(screen.contains(" proc "), proc, "{ctx}");
        }
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
  fn cpu_box_title_has_chip_clock_and_version() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    let top = row(&buf, 0);
    assert!(top.starts_with("╭─ cpu 45°C ─ Apple M3 Pro · 6E+6P · 18GPU · 36GB ─"), "{top}");
    assert!(top.ends_with(&format!(" macmon v{} · 1000ms ─╮", env!("CARGO_PKG_VERSION"))));
    let is_clock = |word: &str| word.len() == 8 && word.chars().filter(|c| *c == ':').count() == 2;
    assert!(top.split_whitespace().any(is_clock), "no clock in {top}");

    // narrow: chip info and clock are dropped instead of overlapping
    let buf = render_buffer(&mut app, 40, 12);
    let top = row(&buf, 0);
    assert!(top.starts_with("╭─ cpu 45°C ─"), "{top}");
    assert!(!top.contains("Apple") && !top.contains(':'), "{top}");
  }

  #[test]
  fn power_title_is_not_overwritten() {
    // 100 columns: the POWER box is 40 cells wide next to the process panel
    let mut app = test_app_with(|m| {
      m.fans =
        (0..2).map(|i| FanMetric { name: format!("fan{i}"), rpm: 2000, max_rpm: None }).collect()
    });
    let buf = render_buffer(&mut app, 100, 30);
    let title = row_with(&buf, " power ").expect("power box");
    assert!(title.starts_with("╭─ power 6.60W · avg 6.60W · max 6.60W"), "{title}");
    assert!(!title.contains("Fan") && !title.contains("SYS"), "{title}");

    // SYS and fans go to the footer row instead
    let footer = row_with(&buf, "SYS ").expect("footer");
    assert!(footer.starts_with("│SYS  12.00W  Fans 2000/2000 RPM"), "{footer}");
  }

  #[test]
  fn key_hints_degrade_at_narrow_widths() {
    let full = "q quit  c default  v braille  d cores  r scaled  -/+ 1000ms  1-5 panels";
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

  #[test]
  fn multi_die_cores_show_die_prefix() {
    let mut app = test_app_with(|m| {
      let core = |die_id, core_id| CpuCoreMetrics { die_id, ..core(core_id, 0.5) };
      m.ecpu_cores = vec![core(0, 0), core(1, 0)];
      m.pcpu_cores = vec![core(0, 0), core(0, 1), core(1, 0), core(1, 1)];
    });
    app.cfg.per_core_view = true;

    let screen = render_to_string(&mut app, 200, 50);
    for label in ["D0 E0", "D1 E0", "D0 P0", "D0 P1", "D1 P0", "D1 P1"] {
      assert!(screen.contains(label), "missing {label}");
    }

    // single die: no prefix
    let mut app = test_app();
    app.cfg.per_core_view = true;
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E0 ") && screen.contains("P5 "));
    assert!(!screen.contains("D0 "));
  }

  #[test]
  fn swap_row_hidden_without_swap() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 120, 40).contains("SWAP  1.00/2.0 GB"));

    let mut app = test_app_with(|m| m.memory.swap_total = 0);
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("RAM  20.00/36.0 GB ▰"));
    assert!(!screen.contains("SWAP"));
  }

  #[test]
  fn fans_and_sys_hidden_when_unavailable() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 80, 24);
    assert!(screen.contains("SYS  12.00W") && screen.contains("Fan 1200 RPM"));

    let mut app = test_app_with(|m| {
      m.fans.clear();
      m.sys_power = 0.0;
    });
    let buf = render_buffer(&mut app, 80, 24);
    let screen: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!screen.contains("SYS") && !screen.contains("Fan"));
    // the rows keep the space, the footer row stays blank
    let ane = row_with(&buf, "ANE").expect("ane row");
    assert!(ane.starts_with("│ANE   0.10W"), "{ane}");

    // only one of them: no separator left behind
    let mut app = test_app_with(|m| m.sys_power = 0.0);
    let footer = row_with(&render_buffer(&mut app, 80, 24), "Fan").expect("fans footer");
    assert!(footer.starts_with("│Fan 1200 RPM "), "{footer}");
  }

  #[test]
  fn low_power_box_puts_units_on_one_row() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 60, 15);
    assert!(screen.contains("CPU 4.50W 45°C · GPU 2.00W 40°C · ANE 0.10W"));
  }

  #[test]
  fn keys_update_rendered_panels() {
    let mut app = test_app();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("E-CPU 42%") && screen.contains("gpu 23%"));
    assert!(!screen.contains("E5 "), "per-core grid is off by default");

    // r: active ratios
    assert!(app.handle_key(key('r')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("E-CPU 50%") && screen.contains("P-CPU 80%"));
    assert!(screen.contains("gpu 30%") && screen.contains("r active"));

    // d: per-core grid
    assert!(app.handle_key(key('d')).is_continue());
    assert!(render_to_string(&mut app, 120, 40).contains("E5 "));

    // +/-: interval in the CPU title and the hints
    assert!(app.handle_key(key('+')).is_continue());
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("· 1250ms ─╮") && screen.contains("-/+ 1250ms"));
    assert!(app.handle_key(key('-')).is_continue());
    assert!(app.handle_key(key('-')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("-/+ 750ms"));

    // v: graph style in the hints
    assert!(app.handle_key(key('v')).is_continue());
    assert!(render_to_string(&mut app, 200, 50).contains("v block"));
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
    assert!(screen.contains("Apple M3 Pro") && screen.contains("1400 MHz"));

    for c in ['1', '2'] {
      assert_eq!(app.handle_key(key(c)), ControlFlow::Continue(()));
    }
    let screen = render_to_string(&mut app, 200, 50);
    assert!(!screen.contains("Apple M3 Pro"), "cpu box still shown");
    assert!(!screen.contains("1400 MHz"), "gpu box still shown");
    assert!(screen.contains("RAM") && screen.contains("power") && screen.contains(" proc "));
  }

  #[test]
  fn proc_panel_auto_hides_in_small_window() {
    let mut app = test_app();
    assert!(render_to_string(&mut app, 200, 50).contains(" proc "));
    assert!(!render_to_string(&mut app, 80, 24).contains(" proc "));
    assert!(app.cfg.panels.proc, "auto-hide must not change the config");

    // the only visible panel is never auto-hidden
    app.cfg.panels = Panels { proc: true, cpu: false, gpu: false, mem: false, power: false };
    let screen = render_to_string(&mut app, 80, 24);
    assert!(screen.contains(" proc ") && screen.contains("q quit"));
  }

  #[test]
  fn key_hints_follow_bottom_left_box() {
    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰─ q quit"), "hints on the power box");

    // without POWER the hints move to MEM, which now ends at the bottom too
    app.cfg.panels.power = false;
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰─ q quit"), "hints on the mem box");
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
    let sizes = [(200, 50), (120, 40), (100, 20), (80, 24), (60, 15), (30, 8), (5, 3), (1, 1)];
    for (width, height) in sizes {
      for bits in 0..32u8 {
        let mut app = test_app();
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
  fn per_core_view_renders_meters() {
    for (view_type, filled, empty) in [(ViewType::Braille, "▰", "▱"), (ViewType::Block, "█", "░")]
    {
      let mut app = test_app();
      app.cfg.per_core_view = true;
      app.cfg.view_type = view_type;

      let screen = render_to_string(&mut app, 120, 40);
      assert!(screen.contains("E5 ") && screen.contains("P5 "));
      assert!(screen.contains(" 40%") && screen.contains(" 70%"));
      assert!(screen.contains(filled) && screen.contains(empty), "{view_type:?}");
    }
  }

  #[test]
  fn view_type_switches_graph_style() {
    let is_braille = |c: char| ('\u{2801}'..='\u{28ff}').contains(&c);
    let mut app = test_app();
    assert!(render_to_string(&mut app, 120, 40).chars().any(is_braille));

    app.cfg.view_type = ViewType::Block;
    let screen = render_to_string(&mut app, 120, 40);
    assert!(!screen.chars().any(is_braille));
    assert!(screen.contains('█'));
  }

  #[test]
  fn renders_without_metrics() {
    let mut app = App::default();
    let screen = render_to_string(&mut app, 120, 40);
    assert!(screen.contains("power 0.00W") && screen.contains("RAM"));
    assert!(!screen.contains("°C") && !screen.contains("SYS") && !screen.contains("Fan"));
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
    render_buffer(&mut app, 80, 24);
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
    render_buffer(&mut app, 80, 24);
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
    assert_eq!(app.procs, Some(test_procs()));
    let screen = render_to_string(&mut app, 200, 50);
    assert!(screen.contains("3 processes") && !screen.contains("collecting"));

    app.update_procs(vec![]);
    assert!(render_to_string(&mut app, 200, 50).contains("0 processes"));
  }

  #[test]
  fn hidden_proc_panel_drops_samples() {
    let mut app = test_app();
    // samples arriving before the first frame are dropped
    app.update_procs(test_procs());
    assert_eq!(app.procs, None);

    render_buffer(&mut app, 200, 50);
    app.update_procs(test_procs());
    assert!(app.procs.is_some());

    // hiding drops the list and a sample still in flight
    render_buffer(&mut app, 80, 24);
    assert_eq!(app.procs, None);
    app.update_procs(test_procs());
    assert_eq!(app.procs, None);

    // shown again: collecting until the next sample instead of stale rows
    assert!(render_to_string(&mut app, 200, 50).contains("collecting…"));
    app.update_procs(test_procs());
    assert!(render_to_string(&mut app, 200, 50).contains("3 processes"));
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
