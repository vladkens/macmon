use crate::metrics::{self, MemoryMetrics, Metrics, SocInfo};
use crossterm::{
  event::{self, KeyCode, KeyModifiers},
  terminal, ExecutableCommand,
};
use ratatui::{prelude::*, widgets::*};
use std::{io::stdout, time::Instant};
use std::{sync::mpsc, time::Duration};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

const GB: u64 = 1024 * 1024 * 1024;
const MAX_SPARKLINE: usize = 128;
const BASE_COLOR: Color = Color::LightGreen;

// MARK: Terminal

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

fn items_add(vec: &mut Vec<u64>, val: u64) -> &Vec<u64> {
  vec.insert(0, val);
  if vec.len() > MAX_SPARKLINE {
    vec.pop();
  }
  vec
}

#[derive(Debug, Default)]
struct FreqStore {
  items: Vec<u64>,
  top_value: u64,
  usage: u8,
}

impl FreqStore {
  fn push(&mut self, value: u64, usage: u8) {
    // items_add(&mut self.items, value);
    items_add(&mut self.items, usage as u64);
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
    items_add(&mut self.items, (value * 1000.0) as u64);
    self.top_value = value;
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
  fn push(&mut self, value: MemoryMetrics) {
    items_add(&mut self.items, value.ram_usage);
    self.ram_usage = value.ram_usage;
    self.ram_total = value.ram_total;
    self.swap_usage = value.swap_usage;
    self.swap_total = value.swap_total;
    self.max_ram = self.items.iter().max().map_or(0, |v| *v);
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

fn title_block<'a>(label_l: &str, label_r: &str) -> Block<'a> {
  let mut block = Block::new()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(BASE_COLOR)
    // .title_style(Style::default().gray())
    .padding(Padding::zero());

  if label_l.len() > 0 {
    block = block.title(block::Title::from(format!(" {label_l} ")).alignment(Alignment::Left));
  }

  if label_r.len() > 0 {
    block = block.title(block::Title::from(format!(" {label_r} ")).alignment(Alignment::Right));
  }

  block
}

fn get_freq_block<'a>(label: &str, val: &'a FreqStore) -> Sparkline<'a> {
  let label = format!("{} {:3}% @ {:4.0} MHz", label, val.usage, val.top_value);
  Sparkline::default()
    .block(title_block(label.as_str(), ""))
    .direction(RenderDirection::RightToLeft)
    .data(&val.items)
    .max(100)
    .style(BASE_COLOR)
}

fn get_power_block<'a>(label: &str, val: &'a PowerStore) -> Sparkline<'a> {
  let label = format!(
    // "{} {:.2}W (avg: {:.2}W, max: {:.2}W)",
    "{} {:.2}W (~{:.2}W ^{:.2}W)",
    // "{} {:.2}W (~{:.2}W ↑{:.2}W)",
    label,
    val.top_value,
    val.avg_value,
    val.max_value
  );

  Sparkline::default()
    .block(title_block(label.as_str(), ""))
    .direction(RenderDirection::RightToLeft)
    .data(&val.items)
    .style(BASE_COLOR)
}

fn get_ram_block<'a>(val: &'a MemoryStore) -> Sparkline<'a> {
  let ram_usage_gb = val.ram_usage as f64 / GB as f64;
  let ram_total_gb = val.ram_total as f64 / GB as f64;

  let swap_usage_gb = val.swap_usage as f64 / GB as f64;
  let swap_total_gb = val.swap_total as f64 / GB as f64;

  let label_l = format!("RAM {:4.2} / {:4.1} GB", ram_usage_gb, ram_total_gb);
  let label_r = format!("SWAP {:.2} / {:.1} GB", swap_usage_gb, swap_total_gb);

  Sparkline::default()
    .block(title_block(label_l.as_str(), label_r.as_str()))
    .direction(RenderDirection::RightToLeft)
    .data(&val.items)
    .max(val.ram_total)
    .style(BASE_COLOR)
}

// MARK: Threads

enum Event {
  Update(Metrics),
  Tick,
  Quit,
}

fn run_inputs_thread(tx: mpsc::Sender<Event>, tick: u64) {
  let tick_rate = Duration::from_millis(tick);

  std::thread::spawn(move || {
    let mut last_tick = Instant::now();

    loop {
      if event::poll(Duration::from_millis(100)).unwrap() {
        match event::read().unwrap() {
          event::Event::Key(key) => {
            if key.code == KeyCode::Char('q')
              || (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)
            {
              tx.send(Event::Quit).unwrap();
            }
          }
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

fn run_sampler_thread(tx: mpsc::Sender<Event>, info: SocInfo, cycle_time: u64) {
  let interval = cycle_time.max(100);
  let check_ts = 100;

  std::thread::spawn(move || {
    let mut sampler = metrics::get_metrics_sampler(info).unwrap();

    loop {
      let metrics = sampler.get_metrics(interval).unwrap();
      tx.send(Event::Update(metrics)).unwrap();
      std::thread::sleep(Duration::from_millis(interval - check_ts));
    }
  });
}

// MARK: App

#[derive(Debug, Default)]
pub struct App {
  info: metrics::SocInfo,
  ecpu_freq: FreqStore,
  pcpu_freq: FreqStore,
  gpu_freq: FreqStore,
  cpu_power: PowerStore,
  gpu_power: PowerStore,
  ane_power: PowerStore,
  all_power: PowerStore,
  memory: MemoryStore,
}

impl App {
  pub fn new(info: metrics::SocInfo) -> Self {
    let mut app = App::default();
    app.info = info;
    app
  }

  fn update_metrics(&mut self, data: Metrics) {
    self.cpu_power.push(data.cpu_power as f64);
    self.gpu_power.push(data.gpu_power as f64);
    self.ane_power.push(data.ane_power as f64);
    self.all_power.push(data.all_power as f64);
    self.ecpu_freq.push(data.ecpu_usage.0 as u64, (data.ecpu_usage.1 * 100.0) as u8);
    self.pcpu_freq.push(data.pcpu_usage.0 as u64, (data.pcpu_usage.1 * 100.0) as u8);
    self.gpu_freq.push(data.gpu_usage.0 as u64, (data.gpu_usage.1 * 100.0) as u8);
    self.memory.push(data.memory);
  }

  fn render(&self, f: &mut Frame) {
    let label_l = format!(
      "{} ({}E+{}P+{}GPU {}GB)",
      self.info.chip_name,
      self.info.ecpu_cores,
      self.info.pcpu_cores,
      self.info.gpu_cores,
      self.info.memory_gb,
    );

    let label_r = format!(
      "Power: {:.2}W (avg: {:.2}W, max: {:.2}W)",
      self.all_power.top_value, self.all_power.avg_value, self.all_power.max_value
    );

    let rows = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Fill(2), Constraint::Fill(1)].as_ref())
      .split(f.size());

    let brand = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let block = title_block(&label_l, &brand);
    let iarea = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    let iarea = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Fill(1), Constraint::Fill(1)].as_ref())
      .split(iarea);

    // 1st row
    let (c1, c2) = h_stack(iarea[0]);
    f.render_widget(get_freq_block("E-CPU", &self.ecpu_freq), c1);
    f.render_widget(get_freq_block("P-CPU", &self.pcpu_freq), c2);

    // 2nd row
    let (c1, c2) = h_stack(iarea[1]);
    f.render_widget(get_ram_block(&self.memory), c1);
    f.render_widget(get_freq_block("GPU", &self.gpu_freq), c2);

    // 3rd row
    let block = title_block(&label_r, "");
    let iarea = block.inner(rows[1]);
    f.render_widget(block, rows[1]);

    let ha = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Fill(1), Constraint::Fill(1), Constraint::Fill(1)].as_ref())
      .split(iarea);

    f.render_widget(get_power_block("CPU", &self.cpu_power), ha[0]);
    f.render_widget(get_power_block("GPU", &self.gpu_power), ha[1]);
    f.render_widget(get_power_block("ANE", &self.ane_power), ha[2]);
  }

  pub fn run_loop(&mut self, interval: u64) -> WithError<()> {
    let (tx, rx) = mpsc::channel::<Event>();
    run_inputs_thread(tx.clone(), 200);
    run_sampler_thread(tx.clone(), self.info.clone(), interval);

    let mut term = enter_term();

    loop {
      term.draw(|f| self.render(f)).unwrap();

      match rx.recv()? {
        Event::Update(data) => self.update_metrics(data),
        Event::Quit => break,
        _ => {}
      }
    }

    leave_term();
    Ok(())
  }
}
