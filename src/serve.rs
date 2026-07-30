use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;

use macmon::{Metrics, SocInfo};

use crate::metrics_to_json_value;

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
  gauge!(out, "cpu_scaled_ratio", "Combined frequency-scaled CPU ratio (0–1), weighted by core count", m.cpu_scaled_ratio);
  gauge!(out, "cpu_usage_ratio", "DEPRECATED: use macmon_cpu_scaled_ratio", m.cpu_scaled_ratio);
  gauge!(out, "cpu_active_ratio", "Combined CPU active residency ratio (not frequency-scaled, 0–1), weighted by core count", m.cpu_active_ratio);
  gauge!(out, "ecpu_freq_mhz", "Efficiency CPU cluster frequency in MHz", m.ecpu_freq_mhz);
  gauge!(out, "ecpu_scaled_ratio", "Efficiency CPU cluster frequency-scaled ratio (0–1)", m.ecpu_scaled_ratio);
  gauge!(out, "ecpu_usage_ratio", "DEPRECATED: use macmon_ecpu_scaled_ratio", m.ecpu_scaled_ratio);
  gauge!(out, "ecpu_active_ratio", "Efficiency CPU cluster active residency ratio (not frequency-scaled, 0–1)", m.ecpu_active_ratio);
  gauge!(out, "pcpu_freq_mhz", "Performance CPU cluster frequency in MHz", m.pcpu_freq_mhz);
  gauge!(out, "pcpu_scaled_ratio", "Performance CPU cluster frequency-scaled ratio (0–1)", m.pcpu_scaled_ratio);
  gauge!(out, "pcpu_usage_ratio", "DEPRECATED: use macmon_pcpu_scaled_ratio", m.pcpu_scaled_ratio);
  gauge!(out, "pcpu_active_ratio", "Performance CPU cluster active residency ratio (not frequency-scaled, 0–1)", m.pcpu_active_ratio);
  gauge!(out, "gpu_freq_mhz", "GPU frequency in MHz", m.gpu_freq_mhz);
  gauge!(out, "gpu_scaled_ratio", "GPU frequency-scaled ratio (0–1)", m.gpu_scaled_ratio);
  gauge!(out, "gpu_usage_ratio", "DEPRECATED: use macmon_gpu_scaled_ratio", m.gpu_scaled_ratio);
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
    for fan in &m.fans {
      let fan_name = escape_label_value(&fan.name);
      out.push_str(&format!("{fan_speed_rpm}{{{l},fan=\"{fan_name}\"}} {}\n", fan.rpm));
    }
    out.push('\n');
  }
  out
}

fn to_json(m: &Metrics, soc: &SocInfo) -> String {
  let mut doc = metrics_to_json_value(m).unwrap_or_default();
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
  eprintln!("configured to serve on {}", serve_url(host, port));

  Ok(())
}

pub fn run(
  host: &str,
  port: u16,
  shared: SharedMetrics,
  soc: Arc<SocInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind((host, port))?;
  eprintln!("macmon serving on http://{}", listener.local_addr()?);
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
  use macmon::{Metrics, SocInfo};

  use super::{escape_label_value, escape_xml, serve_url, to_json, to_prometheus};

  #[test]
  fn formats_serving_urls() {
    assert_eq!(serve_url("127.0.0.1", 9090), "http://127.0.0.1:9090");
    assert_eq!(serve_url("0.0.0.0", 9090), "http://0.0.0.0:9090");
    assert_eq!(serve_url("::", 9090), "http://[::]:9090");
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

  #[test]
  fn exports_deprecated_v07_usage_series() {
    let metrics = Metrics {
      cpu_scaled_ratio: 0.1,
      ecpu_scaled_ratio: 0.2,
      pcpu_scaled_ratio: 0.3,
      gpu_scaled_ratio: 0.4,
      ..Default::default()
    };
    let soc = SocInfo { chip_name: "Test".into(), ..Default::default() };
    let output = to_prometheus(&metrics, &soc);

    for (old, new, value) in [
      ("cpu_usage_ratio", "cpu_scaled_ratio", "0.1"),
      ("ecpu_usage_ratio", "ecpu_scaled_ratio", "0.2"),
      ("pcpu_usage_ratio", "pcpu_scaled_ratio", "0.3"),
      ("gpu_usage_ratio", "gpu_scaled_ratio", "0.4"),
    ] {
      assert!(output.contains(&format!("# HELP macmon_{old} DEPRECATED: use macmon_{new}")));
      assert!(output.contains(&format!("macmon_{old}{{chip=\"Test\"}} {value}")));
      assert!(output.contains(&format!("macmon_{new}{{chip=\"Test\"}} {value}")));
    }
  }

  #[test]
  fn exports_v07_json_fields_as_aliases() {
    let metrics = Metrics {
      cpu_scaled_ratio: 0.1,
      ecpu_freq_mhz: 1000,
      ecpu_scaled_ratio: 0.2,
      pcpu_freq_mhz: 2000,
      pcpu_scaled_ratio: 0.3,
      gpu_freq_mhz: 500,
      gpu_scaled_ratio: 0.4,
      ..Default::default()
    };
    let json: serde_json::Value =
      serde_json::from_str(&to_json(&metrics, &SocInfo::default())).unwrap();

    assert_eq!(json["cpu_usage_pct"], json["cpu_scaled_ratio"]);
    assert_eq!(json["ecpu_usage"][0], json["ecpu_freq_mhz"]);
    assert_eq!(json["ecpu_usage"][1], json["ecpu_scaled_ratio"]);
    assert_eq!(json["pcpu_usage"][0], json["pcpu_freq_mhz"]);
    assert_eq!(json["pcpu_usage"][1], json["pcpu_scaled_ratio"]);
    assert_eq!(json["gpu_usage"][0], json["gpu_freq_mhz"]);
    assert_eq!(json["gpu_usage"][1], json["gpu_scaled_ratio"]);
  }
}
