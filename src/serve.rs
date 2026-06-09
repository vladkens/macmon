use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;

use macmon::{Metrics, SocInfo};

pub type SharedMetrics = Arc<RwLock<Option<Metrics>>>;

fn escape_label_value(value: &str) -> String {
  value.replace('\\', r"\\").replace('\n', r"\n").replace('"', r#"\""#)
}

#[rustfmt::skip]
fn to_prometheus(m: &Metrics, soc: &SocInfo) -> String {
  let chip = escape_label_value(&soc.chip_name);
  let l = format!(r#"chip="{chip}""#);

  macro_rules! metric_name {
    ($name:literal) => {
      concat!("macmon_", $name)
    };
  }

  macro_rules! gauge_head {
    ($out:expr, $name:literal, $help:literal) => {
      let name = metric_name!($name);
      $out.push_str(&format!("# HELP {} {}\n# TYPE {} gauge\n", name, $help, name));
    };
  }

  macro_rules! gauge {
    ($out:expr, $name:literal, $help:literal, $value:expr) => {
      gauge_head!($out, $name, $help);
      let name = metric_name!($name);
      $out.push_str(&format!("{}{{{l}}} {}\n\n", name, $value));
    };
  }

  let mut out = String::new();
  gauge!(out, "cpu_temp_celsius", "Average CPU temperature in Celsius", m.temp.cpu_temp_avg);
  gauge!(out, "gpu_temp_celsius", "Average GPU temperature in Celsius", m.temp.gpu_temp_avg);
  gauge!(out, "memory_ram_total_bytes", "Total RAM in bytes", m.memory.ram_total);
  gauge!(out, "memory_ram_used_bytes", "Used RAM in bytes", m.memory.ram_usage);
  gauge!(out, "memory_swap_total_bytes", "Total swap in bytes", m.memory.swap_total);
  gauge!(out, "memory_swap_used_bytes", "Used swap in bytes", m.memory.swap_usage);
  gauge!(out, "cpu_usage_ratio", "Combined CPU effective usage (frequency-scaled, 0–1), weighted by core count", m.cpu_usage_pct);
  gauge!(out, "cpu_active_ratio", "Combined CPU active residency ratio (not frequency-scaled, 0–1), weighted by core count", m.cpu_active_ratio);
  gauge!(out, "ecpu_freq_mhz", "Efficiency CPU cluster average frequency in MHz", m.ecpu_usage.0);
  gauge!(out, "ecpu_usage_ratio", "Efficiency CPU cluster effective usage (frequency-scaled, 0–1)", m.ecpu_usage.1);
  gauge!(out, "ecpu_active_ratio", "Efficiency CPU cluster active residency ratio (not frequency-scaled, 0–1)", m.ecpu_active_ratio);
  gauge!(out, "pcpu_freq_mhz", "Performance CPU cluster average frequency in MHz", m.pcpu_usage.0);
  gauge!(out, "pcpu_usage_ratio", "Performance CPU cluster effective usage (frequency-scaled, 0–1)", m.pcpu_usage.1);
  gauge!(out, "pcpu_active_ratio", "Performance CPU cluster active residency ratio (not frequency-scaled, 0–1)", m.pcpu_active_ratio);
  gauge!(out, "gpu_freq_mhz", "GPU frequency in MHz", m.gpu_usage.0);
  gauge!(out, "gpu_usage_ratio", "GPU effective usage (frequency-scaled, 0–1)", m.gpu_usage.1);
  gauge!(out, "gpu_active_ratio", "GPU active residency ratio (not frequency-scaled, 0–1)", m.gpu_active_ratio);
  gauge!(out, "cpu_power_watts", "CPU power consumption in Watts", m.cpu_power);
  gauge!(out, "gpu_power_watts", "GPU power consumption in Watts", m.gpu_power);
  gauge!(out, "ane_power_watts", "Apple Neural Engine power consumption in Watts", m.ane_power);
  gauge!(out, "all_power_watts", "Combined CPU+GPU+ANE power consumption in Watts", m.all_power);
  gauge!(out, "sys_power_watts", "Total system power consumption in Watts", m.sys_power);
  gauge!(out, "ram_power_watts", "RAM power consumption in Watts", m.ram_power);
  gauge!(out, "gpu_ram_power_watts", "GPU RAM power consumption in Watts", m.gpu_ram_power);
  if !m.fans.is_empty() {
    gauge_head!(out, "fan_speed_rpm", "Fan speed in revolutions per minute");
    let fan_speed_rpm = metric_name!("fan_speed_rpm");
    for (i, fan) in m.fans.iter().enumerate() {
      out.push_str(&format!("{fan_speed_rpm}{{{l},fan=\"{i}\"}} {}\n", fan.rpm));
    }
    out.push('\n');
  }
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

fn serve_url(host: &str, port: u16) -> String {
  let host = if matches!(host, "0.0.0.0" | "::") { "localhost" } else { host };
  let host = if host.contains(':') && !host.starts_with('[') {
    format!("[{host}]")
  } else {
    host.to_string()
  };

  format!("http://{host}:{port}")
}

fn escape_xml(value: &str) -> String {
  value
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
    .replace('\'', "&apos;")
}

fn handle_conn(mut stream: TcpStream, shared: SharedMetrics, soc: Arc<SocInfo>) {
  let path = match read_path(&mut stream) {
    Some(p) => p,
    None => return,
  };

  if path == "/" {
    write_response(&mut stream, 200, "application/json", r#"{}"#.to_string());
    return;
  }

  let lock = shared.read().unwrap();

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

pub fn launchd(host: &str, port: u16, install: bool) -> Result<(), Box<dyn std::error::Error>> {
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
  let bin = escape_xml(&bin);
  let host_xml = escape_xml(host);
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
    <string>--host</string>
    <string>{host_xml}</string>
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
  eprintln!("serving on {}", serve_url(host, port));

  Ok(())
}

pub fn run(
  host: &str,
  port: u16,
  shared: SharedMetrics,
  soc: Arc<SocInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind((host, port))?;
  eprintln!("macmon serving on {}", serve_url(host, port));
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

#[cfg(test)]
mod tests {
  use super::{escape_label_value, escape_xml, serve_url};

  #[test]
  fn formats_serving_urls() {
    assert_eq!(serve_url("127.0.0.1", 9090), "http://127.0.0.1:9090");
    assert_eq!(serve_url("0.0.0.0", 9090), "http://localhost:9090");
    assert_eq!(serve_url("::", 9090), "http://localhost:9090");
    assert_eq!(serve_url("::1", 9090), "http://[::1]:9090");
  }

  #[test]
  fn escapes_xml_values() {
    assert_eq!(escape_xml(r#"<host>&"'host"#), "&lt;host&gt;&amp;&quot;&apos;host");
  }

  #[test]
  fn escapes_prometheus_label_values() {
    assert_eq!(escape_label_value("Mac\\Book\n\"Pro\""), r#"Mac\\Book\n\"Pro\""#);
  }
}
