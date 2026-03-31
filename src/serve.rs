use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use macmon::{Metrics, SocInfo};

pub type SharedMetrics = Arc<Mutex<Option<Metrics>>>;

#[rustfmt::skip]
fn to_prometheus(m: &Metrics, soc: &SocInfo) -> String {
  let chip = &soc.chip_name;
  let l = format!(r#"chip="{chip}""#);

  macro_rules! gauge {
    ($out:expr, $name:literal, $help:literal, $value:expr) => {
      $out.push_str(&format!(
        "# HELP {} {}\n# TYPE {} gauge\n{}{{{l}}} {}\n\n",
        $name, $help, $name, $name, $value
      ));
    };
  }

  let mut out = String::new();
  gauge!(out, "macmon_cpu_temp_celsius", "Average CPU temperature in Celsius", m.temp.cpu_temp_avg);
  gauge!(out, "macmon_gpu_temp_celsius", "Average GPU temperature in Celsius", m.temp.gpu_temp_avg);
  gauge!(out, "macmon_memory_ram_total_bytes", "Total RAM in bytes", m.memory.ram_total);
  gauge!(out, "macmon_memory_ram_used_bytes", "Used RAM in bytes", m.memory.ram_usage);
  gauge!(out, "macmon_memory_swap_total_bytes", "Total swap in bytes", m.memory.swap_total);
  gauge!(out, "macmon_memory_swap_used_bytes", "Used swap in bytes", m.memory.swap_usage);
  gauge!(out, "macmon_cpu_usage_ratio", "Combined CPU utilization (0–1), weighted by core count", m.cpu_usage_pct);
  gauge!(out, "macmon_ecpu_freq_mhz", "Efficiency CPU cluster frequency in MHz", m.ecpu_usage.0);
  gauge!(out, "macmon_ecpu_usage_ratio", "Efficiency CPU cluster utilization (0–1)", m.ecpu_usage.1);
  gauge!(out, "macmon_pcpu_freq_mhz", "Performance CPU cluster frequency in MHz", m.pcpu_usage.0);
  gauge!(out, "macmon_pcpu_usage_ratio", "Performance CPU cluster utilization (0–1)", m.pcpu_usage.1);
  gauge!(out, "macmon_gpu_freq_mhz", "GPU frequency in MHz", m.gpu_usage.0);
  gauge!(out, "macmon_gpu_usage_ratio", "GPU utilization (0–1)", m.gpu_usage.1);
  gauge!(out, "macmon_cpu_power_watts", "CPU power consumption in Watts", m.cpu_power);
  gauge!(out, "macmon_gpu_power_watts", "GPU power consumption in Watts", m.gpu_power);
  gauge!(out, "macmon_ane_power_watts", "Apple Neural Engine power consumption in Watts", m.ane_power);
  gauge!(out, "macmon_all_power_watts", "Combined CPU+GPU+ANE power consumption in Watts", m.all_power);
  gauge!(out, "macmon_sys_power_watts", "Total system power consumption in Watts", m.sys_power);
  gauge!(out, "macmon_ram_power_watts", "RAM power consumption in Watts", m.ram_power);
  gauge!(out, "macmon_gpu_ram_power_watts", "GPU RAM power consumption in Watts", m.gpu_ram_power);
  out
}

fn to_json(m: &Metrics, soc: &SocInfo) -> String {
  let mut doc = serde_json::to_value(m).unwrap_or_default();
  doc["soc"] = serde_json::to_value(soc).unwrap_or_default();
  doc["timestamp"] = serde_json::to_value(chrono::Utc::now().to_rfc3339()).unwrap_or_default();
  serde_json::to_string(&doc).unwrap_or_default()
}

fn read_path(stream: &mut TcpStream) -> Option<String> {
  let mut buf = [0u8; 2048];
  let n = stream.read(&mut buf).ok()?;
  let text = std::str::from_utf8(&buf[..n]).ok()?;
  let path = text.lines().next()?.split_whitespace().nth(1)?;
  Some(path.split('?').next().unwrap_or(path).to_string())
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: String) {
  let status_text = match status {
    200 => "OK",
    404 => "Not Found",
    503 => "Service Unavailable",
    _ => "OK",
  };
  let _ = stream.write_all(
    format!(
      "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
      body.len()
    )
    .as_bytes(),
  );
}

fn handle_conn(mut stream: TcpStream, shared: SharedMetrics, soc: Arc<SocInfo>) {
  let path = match read_path(&mut stream) {
    Some(p) => p,
    None => return,
  };

  let lock = shared.lock().unwrap();

  let Some(m) = lock.as_ref() else {
    drop(lock);
    write_response(&mut stream, 503, "application/json", r#"{"error":"no data yet"}"#.to_string());
    return;
  };

  match path.as_str() {
    "/json" => {
      let body = to_json(m, &soc);
      drop(lock);
      write_response(&mut stream, 200, "application/json", body);
    }
    "/metrics" => {
      let body = to_prometheus(m, &soc);
      drop(lock);
      write_response(&mut stream, 200, "text/plain; version=0.0.4; charset=utf-8", body);
    }
    _ => {
      drop(lock);
      write_response(&mut stream, 404, "application/json", r#"{"error":"not found"}"#.to_string());
    }
  }
}

pub fn launchd(port: u16, install: bool) -> Result<(), Box<dyn std::error::Error>> {
  let home = std::env::var("HOME")?;
  let plist_path = format!("{home}/Library/LaunchAgents/com.macmon.plist");

  if !install {
    let _ = std::process::Command::new("launchctl")
      .args(["unload", &plist_path])
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status();
    std::fs::remove_file(&plist_path)?;
    eprintln!("macmon service uninstalled");
    return Ok(());
  }

  let bin = std::env::current_exe()?;
  let bin = bin.to_string_lossy();
  let plist = format!(
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.macmon</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>serve</string>
    <string>--port</string>
    <string>{port}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#
  );

  let agents_dir = format!("{home}/Library/LaunchAgents");
  std::fs::create_dir_all(&agents_dir)?;

  // unload silently in case it's already running
  let _ = std::process::Command::new("launchctl")
    .args(["unload", &plist_path])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();

  std::fs::write(&plist_path, plist)?;
  std::process::Command::new("launchctl").args(["load", &plist_path]).status()?;
  eprintln!("macmon service installed: {plist_path}");
  eprintln!("serving on http://localhost:{port}");

  Ok(())
}

pub fn run(
  port: u16,
  shared: SharedMetrics,
  soc: Arc<SocInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind(format!("0.0.0.0:{port}"))?;
  eprintln!("macmon serving on http://localhost:{port}");
  eprintln!("  GET /json    → JSON metrics");
  eprintln!("  GET /metrics → Prometheus format");

  for stream in listener.incoming() {
    let Ok(stream) = stream else { continue };
    let shared = Arc::clone(&shared);
    let soc = Arc::clone(&soc);
    thread::spawn(move || handle_conn(stream, shared, soc));
  }

  Ok(())
}
