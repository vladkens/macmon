//! Terminal user interface.

mod layout;
mod store;
mod theme;
mod widgets;

use std::ops::ControlFlow;
use std::sync::{Arc, RwLock};
use std::{io::stdout, time::Instant};
use std::{sync::mpsc, time::Duration};

use ratatui::crossterm::{
  ExecutableCommand,
  event::{self, KeyCode, KeyEvent, KeyModifiers},
  terminal,
};
use ratatui::{prelude::*, widgets::*};

use crate::config::{Config, TUI_MAX_MS, TUI_MIN_MS, ViewType};
use layout::{LayoutPlan, compute_layout};
use macmon::{Metrics, Sampler, SocInfo};
use store::{CpuFreqStore, FanStore, FreqSample, FreqStore, MemoryStore, PowerStore, TempStore};
use theme::Theme;
use widgets::{Meter, graph};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const GB: u64 = 1024 * 1024 * 1024;

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

// MARK: Components

fn h_stack(area: Rect) -> [Rect; 2] {
  Layout::horizontal([Constraint::Fill(1); 2]).areas(area)
}

fn v_stack(area: Rect) -> [Rect; 2] {
  Layout::vertical([Constraint::Fill(1); 2]).areas(area)
}

// MARK: Threads

enum Event {
  Update(Box<Metrics>),
  Key(KeyEvent),
  Tick,
}

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

fn ratio(value: f64, total: f64) -> f64 {
  if total == 0.0 { 0.0 } else { value / total }
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

  fn title_block<'a>(&self, label_l: &str, label_r: &str) -> Block<'a> {
    let mut block = Block::new()
      .borders(Borders::ALL)
      .border_type(BorderType::Rounded)
      .border_style(self.theme.border)
      .title_style(self.theme.title)
      .padding(Padding::ZERO);

    if !label_l.is_empty() {
      block = block.title_top(Line::from(format!(" {label_l} ")));
    }

    if !label_r.is_empty() {
      block = block.title_top(Line::from(format!(" {label_r} ")).alignment(Alignment::Right));
    }

    block
  }

  /// Renders `block` with a history graph inside it (`max: None` scales to the visible data).
  fn render_graph_block(
    &self,
    f: &mut Frame,
    r: Rect,
    block: Block,
    data: &[u64],
    max: Option<u64>,
  ) {
    let inner = block.inner(r);
    f.render_widget(block, r);

    let mut w = graph(self.cfg.view_type, data, &self.theme);
    if let Some(max) = max {
      w = w.max(max);
    }
    f.render_widget(w, inner);
  }

  fn render_power_block(&self, f: &mut Frame, r: Rect, label: &str, val: &PowerStore, temp: f32) {
    let label_l =
      format!("{} {:.2}W ({:.2}, {:.2})", label, val.top_value, val.avg_value, val.max_value);

    let label_r = if temp > 0.0 { format!("{:.1}°C", temp) } else { "".to_string() };
    let block = self.title_block(label_l.as_str(), label_r.as_str());
    self.render_graph_block(f, r, block, &val.items, None);
  }

  fn render_freq_block(&self, f: &mut Frame, r: Rect, label: &str, val: &FreqStore) {
    let ratio = val.ratio(self.cfg.ratio_mode);
    let label = format!("{} {:3.0}% @ {:4.0} MHz", label, ratio.ratio * 100.0, val.freq_mhz);
    let block = self.title_block(label.as_str(), "");
    self.render_graph_block(f, r, block, &ratio.items, Some(100));
  }

  fn render_cores(&self, f: &mut Frame, r: Rect, label: &str, val: &CpuFreqStore) {
    if val.cores.is_empty() {
      return;
    }

    let aggregate_ratio = val.aggregate.ratio(self.cfg.ratio_mode);

    let title = format!(
      "{} {:3.0}% @ {:4.0} MHz ({} cores)",
      label,
      aggregate_ratio.ratio * 100.0,
      val.aggregate.freq_mhz,
      val.cores.len()
    );
    let block = self.title_block(title.as_str(), "");
    let inner = block.inner(r);
    f.render_widget(block, r);

    // Create vertical layout for each core
    let constraints: Vec<Constraint> = (0..val.cores.len()).map(|_| Constraint::Fill(1)).collect();

    let core_areas =
      Layout::default().direction(Direction::Vertical).constraints(constraints).split(inner);

    // Render each core
    let show_die = val.has_multiple_dies();
    for (i, (id, core)) in val.cores.iter().enumerate() {
      if i >= core_areas.len() {
        break;
      }

      let core = core.ratio(self.cfg.ratio_mode);
      let core_label = if show_die {
        format!("D{} Core {}", id.die_id, id.core_id)
      } else {
        format!("Core {}", id.core_id)
      };

      let w = Meter::new(core_label, core.ratio, &self.theme)
        .block_chars(self.cfg.view_type == ViewType::Block);
      f.render_widget(w, core_areas[i]);
    }
  }

  fn render_mem_block(&self, f: &mut Frame, r: Rect, val: &MemoryStore) {
    let ram_usage_gb = val.ram_usage as f64 / GB as f64;
    let ram_total_gb = val.ram_total as f64 / GB as f64;

    let swap_usage_gb = val.swap_usage as f64 / GB as f64;
    let swap_total_gb = val.swap_total as f64 / GB as f64;

    let ram_pct = ratio(ram_usage_gb, ram_total_gb) * 100.0;
    let label_l = format!("RAM {:4.2} / {:4.1} GB ({:.1}%)", ram_usage_gb, ram_total_gb, ram_pct);
    let label_r = if val.swap_total > 0 {
      format!("SWAP {:.2} / {:.1} GB", swap_usage_gb, swap_total_gb)
    } else {
      String::new()
    };

    let block = self.title_block(label_l.as_str(), label_r.as_str());
    self.render_graph_block(f, r, block, &val.items, Some(val.ram_total));
  }

  fn render_split_mem_block(&self, f: &mut Frame, r: Rect, val: &MemoryStore) {
    let ram_usage_gb = val.ram_usage as f64 / GB as f64;
    let ram_total_gb = val.ram_total as f64 / GB as f64;
    let swap_usage_gb = val.swap_usage as f64 / GB as f64;
    let swap_total_gb = val.swap_total as f64 / GB as f64;

    let title = "Memory";
    let block = self.title_block(title, "");
    let inner = block.inner(r);
    f.render_widget(block, r);

    let constraints = if val.swap_total > 0 {
      vec![Constraint::Fill(1), Constraint::Fill(1)]
    } else {
      vec![Constraint::Fill(1)]
    };
    let sections =
      Layout::default().direction(Direction::Vertical).constraints(constraints).split(inner);

    let ram_label = format!("RAM {:4.2}/{:4.1} GB", ram_usage_gb, ram_total_gb);
    let w = graph(self.cfg.view_type, &val.items, &self.theme).max(val.ram_total).label(ram_label);
    f.render_widget(w, sections[0]);

    if val.swap_total == 0 {
      return;
    }

    let swap_label = format!("SWAP {:4.2}/{:4.1} GB", swap_usage_gb, swap_total_gb);
    let w =
      graph(self.cfg.view_type, &val.swap_items, &self.theme).max(val.swap_total).label(swap_label);
    f.render_widget(w, sections[1]);
  }

  /// CPU box: chip info in the title, E-CPU / P-CPU graphs and the optional per-core meters.
  fn render_cpu_box(&self, f: &mut Frame, r: Rect, plan: &LayoutPlan) {
    let label_l = format!(
      "{} ({}{}+{}{}+{}GPU {}GB)",
      self.soc.chip_name,
      self.soc.ecpu_cores,
      self.soc.ecpu_label,
      self.soc.pcpu_cores,
      self.soc.pcpu_label,
      self.soc.gpu_cores,
      self.soc.memory_gb,
    );

    let brand = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    f.render_widget(self.title_block(&label_l, &brand), r);

    let ecpu_block_label = format!("{}-CPU", self.soc.ecpu_label);
    let pcpu_block_label = format!("{}-CPU", self.soc.pcpu_label);

    if let Some(graphs) = plan.cpu_graphs {
      let [ecpu, pcpu] = v_stack(graphs);
      self.render_freq_block(f, ecpu, &ecpu_block_label, &self.ecpu_freq.aggregate);
      self.render_freq_block(f, pcpu, &pcpu_block_label, &self.pcpu_freq.aggregate);
    }

    if let Some(cores) = plan.cores {
      let [ecpu, pcpu] = h_stack(cores);
      self.render_cores(f, ecpu, &ecpu_block_label, &self.ecpu_freq);
      self.render_cores(f, pcpu, &pcpu_block_label, &self.pcpu_freq);
    }
  }

  fn render_power_box(&self, f: &mut Frame, r: Rect) {
    let label_l = format!(
      "Power: {:.2}W (avg {:.2}W, max {:.2}W)",
      self.all_power.top_value, self.all_power.avg_value, self.all_power.max_value,
    );

    // Show labels only if sensors are available
    let fan_label = self.fans.label();
    let sys_label = if self.sys_power.top_value > 0.0 {
      Some(format!(
        "Total {:.2}W ({:.2}, {:.2})",
        self.sys_power.top_value, self.sys_power.avg_value, self.sys_power.max_value
      ))
    } else {
      None
    };
    let label_r = match (!fan_label.is_empty(), sys_label) {
      (true, Some(sys_label)) => format!("{fan_label} | {sys_label}"),
      (true, None) => fan_label,
      (false, Some(sys_label)) => sys_label,
      (false, None) => "".to_string(),
    };

    let block = self.title_block(&label_l, &label_r);
    let iarea = block.inner(r);
    f.render_widget(block, r);

    let [cpu, gpu, ane] = Layout::horizontal([Constraint::Fill(1); 3]).areas(iarea);
    self.render_power_block(f, cpu, "CPU", &self.cpu_power, self.cpu_temp.last());
    self.render_power_block(f, gpu, "GPU", &self.gpu_power, self.gpu_temp.last());
    self.render_power_block(f, ane, "ANE", &self.ane_power, 0.0);
  }

  /// Draws the global key hints over the bottom border of box `r`.
  fn render_key_hints(&self, f: &mut Frame, r: Rect) {
    let usage = format!(
      " q quit | c {} | v chart | d detail | r {} | -/+ {}ms | 1-5 panels ",
      self.theme.name,
      self.cfg.ratio_mode.label(),
      self.cfg.interval,
    );

    // keep the corners, the start of the hints stays visible in narrow boxes
    let row = Rect { x: r.x + 1, y: r.bottom() - 1, width: r.width.saturating_sub(2), height: 1 };
    f.render_widget(Line::from(Span::styled(usage, self.theme.title)), row);
  }

  fn render_all_hidden(&self, f: &mut Frame, area: Rect) {
    let text = "all panels hidden · press 1-5 to show · q quit";
    let row = area.centered_vertically(Constraint::Length(1));
    f.render_widget(Line::from(Span::styled(text, self.theme.dim)).centered(), row);
  }

  fn render(&mut self, f: &mut Frame) {
    let plan = compute_layout(f.area(), self.cfg.panels, self.cfg.per_core_view);

    if let Some(r) = plan.cpu {
      self.render_cpu_box(f, r, &plan);
    }

    if let Some(r) = plan.gpu {
      self.render_freq_block(f, r, "GPU", &self.igpu_freq);
    }

    if let Some(r) = plan.mem {
      if self.cfg.per_core_view {
        self.render_split_mem_block(f, r, &self.mem);
      } else {
        self.render_mem_block(f, r, &self.mem);
      }
    }

    if let Some(r) = plan.power {
      self.render_power_box(f, r);
    }

    // placeholder until the process list lands
    if let Some(r) = plan.proc {
      f.render_widget(self.title_block("proc", ""), r);
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

    let mut term = enter_term();

    loop {
      term.draw(|f| self.render(f)).unwrap();

      match rx.recv()? {
        Event::Update(data) => self.update_metrics(*data),
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

  use macmon::{CpuCoreMetrics, FanMetric, MemMetrics, Metrics, SocInfo, TempMetrics};
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::buffer::Buffer;
  use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
  use ratatui::style::Color;

  use super::App;
  use super::theme::{THEMES, Theme};
  use crate::config::{Panels, RatioMode, ViewType};

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

  fn test_app() -> App {
    let mut app = App { soc: test_soc(), ..Default::default() };
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

  fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    render_buffer(app, width, height).content.iter().map(|cell| cell.symbol()).collect()
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

  #[test]
  fn renders_metric_panels() {
    // (width, height, process panel shown)
    for (width, height, proc) in [(200, 50, true), (120, 40, false)] {
      for per_core_view in [false, true] {
        for view_type in [ViewType::Braille, ViewType::Block] {
          let mut app = test_app();
          app.cfg.panels.proc = proc;
          app.cfg.per_core_view = per_core_view;
          app.cfg.view_type = view_type;

          let screen = render_to_string(&mut app, width, height);
          let ctx = format!("{width}x{height} per_core_view={per_core_view} {view_type:?}");
          for label in ["Apple M3 Pro", "E-CPU", "P-CPU", "GPU", "RAM", "Power", "CPU", "ANE"] {
            assert!(screen.contains(label), "missing {label:?} ({ctx})");
          }
          assert!(screen.contains("Fan 1200 RPM"), "{ctx}");
          assert!(screen.contains("q quit") && screen.contains("1-5 panels"), "{ctx}");
          assert_eq!(screen.contains(" proc "), proc, "{ctx}");
        }
      }
    }
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
    assert!(screen.contains("RAM") && screen.contains("Power") && screen.contains(" proc "));
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
    let row = |buf: &Buffer, y: u16| -> String {
      (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    };

    let mut app = test_app();
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰ q quit"), "hints on the power box");

    // without POWER the hints move to MEM, which now ends at the bottom too
    app.cfg.panels.power = false;
    let buf = render_buffer(&mut app, 200, 50);
    assert!(row(&buf, 49).starts_with("╰ q quit"), "hints on the mem box");
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
      assert!(screen.contains("Core 5"));
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
    assert!(screen.contains("Power"));
  }
}
