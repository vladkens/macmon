use std::sync::{Arc, RwLock};
use std::{io::stdout, time::Instant};
use std::{sync::mpsc, time::Duration};

use ratatui::crossterm::{
  ExecutableCommand,
  event::{self, KeyCode, KeyModifiers},
  terminal,
};
use ratatui::{prelude::*, widgets::*};

use crate::config::{Config, ViewType};
use crate::metrics::{Metrics, Sampler, zero_div};
use crate::{metrics::MemMetrics, sources::SocInfo};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const GB: u64 = 1024 * 1024 * 1024;
const MAX_SPARKLINE: usize = 128;
const MAX_TEMPS: usize = 8;

// MARK: Term utils

fn enter_term() -> Terminal<impl Backend> {
  std::panic::set_hook(Box::new(|info| {
    leave_term();
    eprintln!("{}", info);
  }));

  terminal::enable_raw_mode().unwrap();
  stdout().execute(terminal::EnterAlternateScreen).unwrap();

  let term = CrosstermBackend::new(std::io::stdout());
  let term = Terminal::new(term).unwrap();
  term
}

fn leave_term() {
  terminal::disable_raw_mode().unwrap();
  stdout().execute(terminal::LeaveAlternateScreen).unwrap();
}

// MARK: Storage

#[derive(Debug, Default)]
struct FreqStore {
  items: Vec<u64>, // from 0 to 100
  top_value: u64,
  usage: f64, // from 0.0 to 1.0
}

impl FreqStore {
  fn push(&mut self, value: u64, usage: f64) {
    self.items.insert(0, (usage * 100.0) as u64);
    self.items.truncate(MAX_SPARKLINE);

    self.top_value = value;
    self.usage = usage;
  }
}

#[derive(Debug, Default)]
struct PowerStore {
  items: Vec<u64>,
  top_value: f64,
  max_value: f64,
  avg_value: f64,
}

impl PowerStore {
  fn push(&mut self, value: f64) {
    let was_top = if !self.items.is_empty() { self.items[0] as f64 / 1000.0 } else { 0.0 };

    self.items.insert(0, (value * 1000.0) as u64);
    self.items.truncate(MAX_SPARKLINE);

    self.top_value = avg2(was_top, value);
    self.avg_value = self.items.iter().sum::<u64>() as f64 / self.items.len() as f64 / 1000.0;
    self.max_value = self.items.iter().max().map_or(0, |v| *v) as f64 / 1000.0;
  }
}

#[derive(Debug, Default)]
struct MemoryStore {
  items: Vec<u64>,
  ram_usage: u64,
  ram_total: u64,
  swap_usage: u64,
  swap_total: u64,
  max_ram: u64,
}

impl MemoryStore {
  fn push(&mut self, value: MemMetrics) {
    self.items.insert(0, value.ram_usage);
    self.items.truncate(MAX_SPARKLINE);

    self.ram_usage = value.ram_usage;
    self.ram_total = value.ram_total;
    self.swap_usage = value.swap_usage;
    self.swap_total = value.swap_total;
    self.max_ram = self.items.iter().max().map_or(0, |v| *v);
  }
}

#[derive(Debug, Default)]
struct TempStore {
  items: Vec<f32>,
}

impl TempStore {
  fn last(&self) -> f32 {
    *self.items.first().unwrap_or(&0.0)
  }

  fn push(&mut self, value: f32) {
    // https://www.tunabellysoftware.com/blog/files/tg-pro-apple-silicon-m3-series-support.html
    // https://github.com/vladkens/macmon/issues/12
    let value = if value == 0.0 { self.trend_ema(0.8) } else { value };
    if value == 0.0 {
      return; // skip if not sensor available
    }

    self.items.insert(0, value);
    self.items.truncate(MAX_TEMPS);
  }

  // https://en.wikipedia.org/wiki/Exponential_smoothing
  fn trend_ema(&self, alpha: f32) -> f32 {
    if self.items.len() < 2 {
      return 0.0;
    }

    // starts from most recent value, so need to be reversed
    let mut iter = self.items.iter().rev();
    let mut ema = *iter.next().unwrap_or(&0.0);

    for &item in iter {
      ema = alpha * item + (1.0 - alpha) * ema;
    }

    ema
  }
}

// MARK: Components

fn h_stack(area: Rect) -> (Rect, Rect) {
  let ha = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Fill(1), Constraint::Fill(1)].as_ref())
    .split(area);

  (ha[0], ha[1])
}

// MARK: Threads

enum Event {
  Update(Metrics),
  ChangeColor,
  ChangeView,
  IncInterval,
  DecInterval,
  Tick,
  Quit,
}

fn handle_key_event(key: &event::KeyEvent, tx: &mpsc::Sender<Event>) -> WithError<()> {
  match key.code {
    KeyCode::Char('q') => Ok(tx.send(Event::Quit)?),
    KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => Ok(tx.send(Event::Quit)?),
    KeyCode::Char('c') => Ok(tx.send(Event::ChangeColor)?),
    KeyCode::Char('v') => Ok(tx.send(Event::ChangeView)?),
    KeyCode::Char('+') => Ok(tx.send(Event::IncInterval)?),
    KeyCode::Char('=') => Ok(tx.send(Event::IncInterval)?), // fallback to press without shift
    KeyCode::Char('-') => Ok(tx.send(Event::DecInterval)?),
    _ => Ok(()),
  }
}

fn run_inputs_thread(tx: mpsc::Sender<Event>, tick: u64) {
  let tick_rate = Duration::from_millis(tick);

  std::thread::spawn(move || {
    let mut last_tick = Instant::now();

    loop {
      if event::poll(Duration::from_millis(tick)).unwrap() {
        match event::read().unwrap() {
          event::Event::Key(key) => handle_key_event(&key, &tx).unwrap(),
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
    tx.send(Event::Update(sampler.get_metrics(100).unwrap())).unwrap();

    loop {
      let msec = *msec.read().unwrap();
      tx.send(Event::Update(sampler.get_metrics(msec).unwrap())).unwrap();
    }
  });
}

// get average of two values, used to smooth out metrics
// see: https://github.com/vladkens/macmon/issues/10
fn avg2<T: num_traits::Float>(a: T, b: T) -> T {
  return if a == T::zero() { b } else { (a + b) / T::from(2.0).unwrap() };
}

// MARK: App

#[derive(Debug, Default)]
pub struct App {
  cfg: Config,

  soc: SocInfo,
  mem: MemoryStore,

  cpu_power: PowerStore,
  gpu_power: PowerStore,
  ane_power: PowerStore,
  all_power: PowerStore,
  sys_power: PowerStore,

  cpu_temp: TempStore,
  gpu_temp: TempStore,

  ecpu_freq: FreqStore,
  pcpu_freq: FreqStore,
  igpu_freq: FreqStore,
}

impl App {
  pub fn new() -> WithError<Self> {
    let soc = SocInfo::new()?;
    let cfg = Config::load();
    Ok(Self { cfg, soc, ..Default::default() })
  }

  fn update_metrics(&mut self, data: Metrics) {
    self.cpu_power.push(data.cpu_power as f64);
    self.gpu_power.push(data.gpu_power as f64);
    self.ane_power.push(data.ane_power as f64);
    self.all_power.push(data.all_power as f64);
    self.sys_power.push(data.sys_power as f64);
    self.ecpu_freq.push(data.ecpu_usage.0 as u64, data.ecpu_usage.1 as f64);
    self.pcpu_freq.push(data.pcpu_usage.0 as u64, data.pcpu_usage.1 as f64);
    self.igpu_freq.push(data.gpu_usage.0 as u64, data.gpu_usage.1 as f64);

    self.cpu_temp.push(data.temp.cpu_temp_avg);
    self.gpu_temp.push(data.temp.gpu_temp_avg);

    self.mem.push(data.memory);
  }

  fn title_block<'a>(&self, label_l: &str, label_r: &str) -> Block<'a> {
    let mut block = Block::new()
      .borders(Borders::ALL)
      .border_type(BorderType::Rounded)
      .border_style(self.cfg.color)
      // .title_style(Style::default().gray())
      .padding(Padding::ZERO);

    if label_l.len() > 0 {
      block = block.title_top(Line::from(format!(" {label_l} ")));
    }

    if label_r.len() > 0 {
      block = block.title_top(Line::from(format!(" {label_r} ")).alignment(Alignment::Right));
    }

    block
  }

  fn get_power_block<'a>(&self, label: &str, val: &'a PowerStore, temp: f32) -> Sparkline<'a> {
    let label_l = format!(
      "{} {:.2}W ({:.2}, {:.2})",
      // "{} {:.2}W (avg: {:.2}W, max: {:.2}W)",
      // "{} {:.2}W (~{:.2}W ^{:.2}W)",
      label,
      val.top_value,
      val.avg_value,
      val.max_value
    );

    let label_r = if temp > 0.0 { format!("{:.1}°C", temp) } else { "".to_string() };

    Sparkline::default()
      .block(self.title_block(label_l.as_str(), label_r.as_str()))
      .direction(RenderDirection::RightToLeft)
      .data(&val.items)
      .style(self.cfg.color)
  }

  fn render_freq_block(&self, f: &mut Frame, r: Rect, label: &str, val: &FreqStore) {
    let label = format!("{} {:3.0}% @ {:4.0} MHz", label, val.usage * 100.0, val.top_value);
    let block = self.title_block(label.as_str(), "");

    match self.cfg.view_type {
      ViewType::Sparkline => {
        let w = Sparkline::default()
          .block(block)
          .direction(RenderDirection::RightToLeft)
          .data(&val.items)
          .max(100)
          .style(self.cfg.color);
        f.render_widget(w, r);
      }
      ViewType::Gauge => {
        let w = Gauge::default()
          .block(block)
          .gauge_style(self.cfg.color)
          .style(self.cfg.color)
          .label("")
          .ratio(val.usage);
        f.render_widget(w, r);
      }
    }
  }

  fn render_mem_block(&self, f: &mut Frame, r: Rect, val: &MemoryStore) {
    let ram_usage_gb = val.ram_usage as f64 / GB as f64;
    let ram_total_gb = val.ram_total as f64 / GB as f64;

    let swap_usage_gb = val.swap_usage as f64 / GB as f64;
    let swap_total_gb = val.swap_total as f64 / GB as f64;

    let label_l = format!("RAM {:4.2} / {:4.1} GB", ram_usage_gb, ram_total_gb);
    let label_r = format!("SWAP {:.2} / {:.1} GB", swap_usage_gb, swap_total_gb);

    let block = self.title_block(label_l.as_str(), label_r.as_str());
    match self.cfg.view_type {
      ViewType::Sparkline => {
        let w = Sparkline::default()
          .block(block)
          .direction(RenderDirection::RightToLeft)
          .data(&val.items)
          .max(val.ram_total)
          .style(self.cfg.color);
        f.render_widget(w, r);
      }
      ViewType::Gauge => {
        let w = Gauge::default()
          .block(block)
          .gauge_style(self.cfg.color)
          .style(self.cfg.color)
          .label("")
          .ratio(zero_div(ram_usage_gb, ram_total_gb));
        f.render_widget(w, r);
      }
    }
  }

  fn render(&mut self, f: &mut Frame) {
    let label_l = format!(
      "{} ({}E+{}P+{}GPU {}GB)",
      self.soc.chip_name,
      self.soc.ecpu_cores,
      self.soc.pcpu_cores,
      self.soc.gpu_cores,
      self.soc.memory_gb,
    );

    let rows = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Fill(2), Constraint::Fill(1)].as_ref())
      .split(f.area());

    let brand = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let block = self.title_block(&label_l, &brand);
    let iarea = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    let iarea = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Fill(1), Constraint::Fill(1)].as_ref())
      .split(iarea);

    // 1st row
    let (c1, c2) = h_stack(iarea[0]);
    self.render_freq_block(f, c1, "E-CPU", &self.ecpu_freq);
    self.render_freq_block(f, c2, "P-CPU", &self.pcpu_freq);

    // 2nd row
    let (c1, c2) = h_stack(iarea[1]);
    self.render_mem_block(f, c1, &self.mem);
    self.render_freq_block(f, c2, "GPU", &self.igpu_freq);

    // 3rd row
    let label_l = format!(
      "Power: {:.2}W (avg {:.2}W, max {:.2}W)",
      self.all_power.top_value, self.all_power.avg_value, self.all_power.max_value,
    );

    // Show label only if sensor is available
    let label_r = if self.sys_power.top_value > 0.0 {
      format!(
        "Total {:.2}W ({:.2}, {:.2})",
        self.sys_power.top_value, self.sys_power.avg_value, self.sys_power.max_value
      )
    } else {
      "".to_string()
    };

    let block = self.title_block(&label_l, &label_r);
    let usage = format!(" 'q' – quit, 'c' – color, 'v' – view | -/+ {}ms ", self.cfg.interval);
    let block = block.title_bottom(Line::from(usage).right_aligned());
    let iarea = block.inner(rows[1]);
    f.render_widget(block, rows[1]);

    let ha = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Fill(1), Constraint::Fill(1), Constraint::Fill(1)].as_ref())
      .split(iarea);

    f.render_widget(self.get_power_block("CPU", &self.cpu_power, self.cpu_temp.last()), ha[0]);
    f.render_widget(self.get_power_block("GPU", &self.gpu_power, self.gpu_temp.last()), ha[1]);
    f.render_widget(self.get_power_block("ANE", &self.ane_power, 0.0), ha[2]);
  }

  pub fn run_loop(&mut self, interval: Option<u32>) -> WithError<()> {
    // use from arg if provided, otherwise use config restored value
    self.cfg.interval = interval.unwrap_or(self.cfg.interval).max(100).min(10_000);
    let msec = Arc::new(RwLock::new(self.cfg.interval));

    let (tx, rx) = mpsc::channel::<Event>();
    run_inputs_thread(tx.clone(), 250);
    run_sampler_thread(tx.clone(), msec.clone());

    let mut term = enter_term();

    loop {
      term.draw(|f| self.render(f)).unwrap();

      match rx.recv()? {
        Event::Quit => break,
        Event::Update(data) => self.update_metrics(data),
        Event::ChangeColor => self.cfg.next_color(),
        Event::ChangeView => self.cfg.next_view_type(),
        Event::IncInterval => {
          self.cfg.inc_interval();
          *msec.write().unwrap() = self.cfg.interval;
        }
        Event::DecInterval => {
          self.cfg.dec_interval();
          *msec.write().unwrap() = self.cfg.interval;
        }
        _ => {}
      }
    }

    leave_term();
    Ok(())
  }
}
