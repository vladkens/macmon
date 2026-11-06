//! HTTP serving and launchd support.

use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use macmon::{Metrics, SocInfo};

use crate::metrics_to_json_value;

pub type SharedMetrics = Arc<RwLock<Option<Metrics>>>;

const DAEMON_PLIST: &str = "/Library/LaunchDaemons/com.macmon.plist";
const DAEMON_TARGET: &str = "system/com.macmon";
const NOT_INSTALLED: &str = "the macmon service is not installed";

fn escape_label_value(value: &str) -> String {
  value.replace('\\', r"\\").replace('\n', r"\n").replace('"', r#"\""#)
}

#[rustfmt::skip]
#[allow(deprecated)] // keeps exporting macmon_ecpu_* / macmon_pcpu_*
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

  // `gauge_any!`: a gauge every Mac and VM has.
  macro_rules! gauge_any {
    ($out:expr, $name:literal, $help:literal, $value:expr) => {
      gauge_head!($out, $name, $help);
      let name = metric_name!($name);
      $out.push_str(&format!("{}{{{l}}} {}\n\n", name, $value));
    };
  }

  // `tiers_any!`: a per-tier gauge (`tier="E"` etc.) every Mac and VM has; none without tiers.
  macro_rules! tiers_any {
    ($out:expr, $name:literal, $help:literal, $field:ident) => {
      if !m.cpu_tiers.is_empty() {
        gauge_head!($out, $name, $help);
        let name = metric_name!($name);
        for tier in &m.cpu_tiers {
          let tier_label = escape_label_value(&tier.label);
          $out.push_str(&format!("{name}{{{l},tier=\"{tier_label}\"}} {}\n", tier.$field));
        }
        $out.push('\n');
      }
    };
  }

  // `gauge_mac!`: a gauge only a real Mac has (power counters, frequency tables); left out in a VM.
  macro_rules! gauge_mac {
    ($($args:tt)*) => {
      if !soc.vm {
        gauge_any!($($args)*);
      }
    };
  }
  // `tiers_mac!`: a per-tier gauge only a real Mac has (frequency tables); left out in a VM.
  macro_rules! tiers_mac {
    ($($args:tt)*) => {
      if !soc.vm {
        tiers_any!($($args)*);
      }
    };
  }

  let mut out = String::new();
  if let Some(value) = m.temp.cpu_temp_avg {
    gauge_any!(out, "cpu_temp_celsius", "Average CPU temperature in Celsius", value);
  }
  if let Some(value) = m.temp.gpu_temp_avg {
    gauge_any!(out, "gpu_temp_celsius", "Average GPU temperature in Celsius", value);
  }
  gauge_any!(out, "memory_ram_total_bytes", "Total RAM in bytes", m.memory.ram_total);
  gauge_any!(out, "memory_ram_used_bytes", "Used RAM in bytes", m.memory.ram_usage);
  gauge_any!(out, "memory_swap_total_bytes", "Total swap in bytes", m.memory.swap_total);
  gauge_any!(out, "memory_swap_used_bytes", "Used swap in bytes", m.memory.swap_usage);
  gauge_any!(out, "cpu_scaled_ratio", "Combined frequency-scaled CPU ratio (0–1), weighted by core count", m.cpu_scaled_ratio);
  gauge_any!(out, "cpu_usage_ratio", "DEPRECATED: use macmon_cpu_scaled_ratio", m.cpu_scaled_ratio);
  gauge_any!(out, "cpu_active_ratio", "Combined CPU active residency ratio (not frequency-scaled, 0–1), weighted by core count", m.cpu_active_ratio);
  gauge_mac!(out, "ecpu_freq_mhz", "DEPRECATED: use macmon_cpu_tier_freq_mhz. Lowest CPU tier frequency in MHz", m.ecpu_freq_mhz);
  gauge_any!(out, "ecpu_scaled_ratio", "DEPRECATED: use macmon_cpu_tier_scaled_ratio. Lowest CPU tier frequency-scaled ratio (0–1)", m.ecpu_scaled_ratio);
  gauge_any!(out, "ecpu_usage_ratio", "DEPRECATED: use macmon_cpu_tier_scaled_ratio", m.ecpu_scaled_ratio);
  gauge_any!(out, "ecpu_active_ratio", "DEPRECATED: use macmon_cpu_tier_active_ratio. Lowest CPU tier active residency ratio (not frequency-scaled, 0–1)", m.ecpu_active_ratio);
  gauge_mac!(out, "pcpu_freq_mhz", "DEPRECATED: use macmon_cpu_tier_freq_mhz. Highest CPU tier frequency in MHz", m.pcpu_freq_mhz);
  gauge_any!(out, "pcpu_scaled_ratio", "DEPRECATED: use macmon_cpu_tier_scaled_ratio. Highest CPU tier frequency-scaled ratio (0–1)", m.pcpu_scaled_ratio);
  gauge_any!(out, "pcpu_usage_ratio", "DEPRECATED: use macmon_cpu_tier_scaled_ratio", m.pcpu_scaled_ratio);
  gauge_any!(out, "pcpu_active_ratio", "DEPRECATED: use macmon_cpu_tier_active_ratio. Highest CPU tier active residency ratio (not frequency-scaled, 0–1)", m.pcpu_active_ratio);
  tiers_mac!(out, "cpu_tier_freq_mhz", "CPU tier frequency in MHz", freq_mhz);
  tiers_any!(out, "cpu_tier_scaled_ratio", "CPU tier frequency-scaled ratio (0–1)", scaled_ratio);
  tiers_any!(out, "cpu_tier_active_ratio", "CPU tier active residency ratio (not frequency-scaled, 0–1)", active_ratio);
  gauge_mac!(out, "gpu_freq_mhz", "GPU frequency in MHz", m.gpu_freq_mhz);
  gauge_any!(out, "gpu_scaled_ratio", "GPU frequency-scaled ratio (0–1)", m.gpu_scaled_ratio);
  gauge_any!(out, "gpu_usage_ratio", "DEPRECATED: use macmon_gpu_scaled_ratio", m.gpu_scaled_ratio);
  gauge_any!(out, "gpu_active_ratio", "GPU active residency ratio (not frequency-scaled, 0–1)", m.gpu_active_ratio);
  gauge_mac!(out, "cpu_power_watts", "CPU power consumption in Watts", m.cpu_power);
  gauge_mac!(out, "gpu_power_watts", "GPU power consumption in Watts", m.gpu_power);
  gauge_mac!(out, "ane_power_watts", "Apple Neural Engine power consumption in Watts", m.ane_power);
  gauge_mac!(out, "all_power_watts", "Combined CPU+GPU+ANE power consumption in Watts", m.all_power);
  gauge_mac!(out, "sys_power_watts", "Total system power consumption in Watts", m.sys_power);
  gauge_mac!(out, "ram_power_watts", "RAM power consumption in Watts", m.ram_power);
  gauge_mac!(out, "gpu_ram_power_watts", "GPU RAM power consumption in Watts", m.gpu_ram_power);
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

fn plist(bin: &str, host: &str, port: u16, user: Option<&str>) -> String {
  let bin = escape_xml(bin);
  let host_xml = escape_xml(host);
  let user = user.map(escape_xml);
  let user = user.map(|x| format!("  <key>UserName</key>\n  <string>{x}</string>\n"));
  let user = user.unwrap_or_default();

  format!(
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
{user}</dict>
</plist>
"#
  )
}

fn agent_plist(home: &str) -> String {
  format!("{home}/Library/LaunchAgents/com.macmon.plist")
}

fn launchctl_quiet(args: &[&str]) {
  let _ = std::process::Command::new("launchctl")
    .args(args)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
}

fn own_args() -> String {
  std::env::args().collect::<Vec<_>>().join(" ")
}

fn refuse(msg: &str) -> ! {
  eprintln!("{msg}");
  std::process::exit(1);
}

fn agent_refusal(exe: &str) -> String {
  format!(
    "the macmon daemon is installed and wants the same port, run: sudo {exe} serve --uninstall"
  )
}

fn install_refusal(args: &str) -> String {
  format!("installing needs root, to start at boot run:\n  sudo {args}\n")
    + &format!("or to start at login, without sudo:\n  {args} --agent")
}

pub fn install_agent(host: &str, port: u16) -> Result<(), Box<dyn std::error::Error>> {
  if Path::new(DAEMON_PLIST).exists() {
    refuse(&agent_refusal(&std::env::args().next().unwrap_or("macmon".into())));
  }

  let home = std::env::var("HOME")?;
  let plist_path = agent_plist(&home);

  let bin = std::env::current_exe()?;
  let plist = plist(&bin.to_string_lossy(), host, port, None);

  let agents_dir = format!("{home}/Library/LaunchAgents");
  std::fs::create_dir_all(&agents_dir)?;

  // unload silently in case it's already running
  launchctl_quiet(&["unload", &plist_path]);

  std::fs::write(&plist_path, plist)?;
  std::process::Command::new("launchctl").args(["load", &plist_path]).status()?;
  eprintln!("macmon service installed: {plist_path}");
  eprintln!("configured to serve on {}", serve_url(host, port));

  Ok(())
}

fn user_info(user: &str) -> Option<(u32, String)> {
  let name = CString::new(user).ok()?;

  // HOME can't tell: sudo may keep the caller's or set root's
  let pw = unsafe { libc::getpwnam(name.as_ptr()) };
  if pw.is_null() {
    return None;
  }

  let dir = unsafe { CStr::from_ptr((*pw).pw_dir) };
  Some((unsafe { (*pw).pw_uid }, dir.to_string_lossy().into_owned()))
}

// As root, for the user behind sudo. Says if there was an agent to remove.
fn remove_user_agent(user: &str) -> std::io::Result<bool> {
  let Some((uid, home)) = user_info(user) else { return Ok(false) };

  let plist_path = agent_plist(&home);
  if !Path::new(&plist_path).exists() {
    return Ok(false);
  }

  // fails when it isn't loaded, which is fine
  launchctl_quiet(&["bootout", &format!("gui/{uid}/com.macmon")]);

  std::fs::remove_file(plist_path)?;
  eprintln!("removed the launchd agent of {user}");
  Ok(true)
}

fn bootstrap_daemon() -> Result<(), Box<dyn std::error::Error>> {
  let mut tries = 6;

  loop {
    let args = ["bootstrap", "system", DAEMON_PLIST];
    let out = std::process::Command::new("launchctl").args(args).output()?;
    if out.status.success() {
      return Ok(());
    }

    tries -= 1;
    if tries == 0 {
      let err = String::from_utf8_lossy(&out.stderr);
      let err = err.trim();
      let msg = format!("launchctl bootstrap failed, {DAEMON_PLIST} stays for next boot: {err}");
      return Err(msg.into());
    }

    // right after bootout launchd may still be removing the old service
    thread::sleep(Duration::from_millis(500));
  }
}

// A plain root shell has no SUDO_USER, or has root in it: a daemon must never run as root.
fn daemon_user(sudo_user: Option<String>) -> Option<String> {
  sudo_user.filter(|x| !x.is_empty() && x != "root")
}

fn is_root() -> bool {
  unsafe { libc::geteuid() == 0 }
}

pub fn install_daemon(host: &str, port: u16) -> Result<(), Box<dyn std::error::Error>> {
  let args = own_args();

  if !is_root() {
    refuse(&install_refusal(&args));
  }

  let Some(user) = daemon_user(std::env::var("SUDO_USER").ok()) else {
    refuse(&format!("the macmon daemon must not run as root, from your account run: sudo {args}"));
  };

  let bin = std::env::current_exe()?;
  let plist = plist(&bin.to_string_lossy(), host, port, Some(&user));

  // launchd refuses a plist that isn't root-owned or is writable by others; umask can't be trusted
  let tmp = format!("{DAEMON_PLIST}.tmp");
  std::fs::write(&tmp, plist)?;
  std::os::unix::fs::chown(&tmp, Some(0), Some(0))?;
  std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
  std::fs::rename(&tmp, DAEMON_PLIST)?;

  // the file goes first: if writing it fails, the running service is still up
  launchctl_quiet(&["bootout", DAEMON_TARGET]);
  bootstrap_daemon()?;

  eprintln!("macmon daemon installed: {DAEMON_PLIST}");
  eprintln!("configured to serve on {}", serve_url(host, port));
  eprintln!("the daemon runs as {user}");

  // after the daemon is up, so a failed install keeps the agent; launchd retries the port meanwhile
  if let Err(e) = remove_user_agent(&user) {
    eprintln!("could not remove the launchd agent of {user}: {e}");
  }

  Ok(())
}

fn uninstall_daemon() -> Result<(), Box<dyn std::error::Error>> {
  launchctl_quiet(&["bootout", DAEMON_TARGET]);

  // bootout also fails when nothing is loaded: only a service still there is an error
  let args = ["print", DAEMON_TARGET];
  if std::process::Command::new("launchctl").args(args).output()?.status.success() {
    return Err(format!("launchctl bootout failed, {DAEMON_PLIST} stays").into());
  }

  std::fs::remove_file(DAEMON_PLIST)?;
  eprintln!("macmon daemon uninstalled");

  Ok(())
}

pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
  let daemon = Path::new(DAEMON_PLIST).exists();

  if is_root() {
    if daemon {
      uninstall_daemon()?;
    }

    let user = daemon_user(std::env::var("SUDO_USER").ok());
    let agent = user.map(|x| remove_user_agent(&x)).transpose()?.unwrap_or(false);
    if !daemon && !agent {
      refuse(NOT_INSTALLED);
    }

    return Ok(());
  }

  let plist_path = agent_plist(&std::env::var("HOME")?);
  let agent = Path::new(&plist_path).exists();
  if agent {
    launchctl_quiet(&["unload", &plist_path]);
    std::fs::remove_file(&plist_path)?;
    eprintln!("macmon agent uninstalled");
  }

  if daemon {
    refuse(&format!("the macmon daemon needs root, run: sudo {}", own_args()));
  }

  if !agent {
    refuse(NOT_INSTALLED);
  }

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
  use macmon::{CpuTierMetrics, Metrics, SocInfo};

  use super::{
    agent_refusal, daemon_user, escape_label_value, escape_xml, install_refusal, plist, serve_url,
    to_json, to_prometheus,
  };

  const AGENT_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.macmon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/macmon</string>
    <string>serve</string>
    <string>--host</string>
    <string>0.0.0.0</string>
    <string>--port</string>
    <string>9090</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#;

  #[test]
  fn unavailable_temperature_has_no_prometheus_gauge() {
    let mut metrics = Metrics::default();
    metrics.temp.cpu_temp_avg = Some(45.0);
    let output = to_prometheus(&metrics, &SocInfo::default());
    assert!(output.contains("macmon_cpu_temp_celsius{chip=\"\"} 45"));
    assert!(!output.contains("macmon_gpu_temp_celsius"));
    assert!(output.contains("macmon_gpu_freq_mhz"));
  }

  #[test]
  fn a_vm_has_no_power_and_frequency_gauges() {
    let tier = CpuTierMetrics { label: "_".into(), ..Default::default() };
    let metrics = Metrics { cpu_tiers: vec![tier], ..Default::default() };
    let types = |vm| {
      let soc = SocInfo { vm, ..Default::default() };
      let output = to_prometheus(&metrics, &soc);
      let types = output.lines().filter_map(|x| x.strip_prefix("# TYPE macmon_"));
      types.map(|x| x.trim_end_matches(" gauge").to_string()).collect::<Vec<_>>()
    };

    // a Mac: every gauge, in the same order
    let power =
      ["cpu", "gpu", "ane", "all", "sys", "ram", "gpu_ram"].map(|x| format!("{x}_power_watts"));
    let freqs = ["ecpu_freq_mhz", "pcpu_freq_mhz", "cpu_tier_freq_mhz", "gpu_freq_mhz"];
    let mac = types(false);
    assert_eq!(mac.len(), 29, "{mac:?}"); // no temperature and fan gauges without readings
    assert!(power.iter().chain(&freqs.map(String::from)).all(|x| mac.contains(x)), "{mac:?}");

    // a VM: the same without the power and frequency gauges
    let expected = mac.iter().filter(|x| !power.contains(x) && !freqs.contains(&x.as_str()));
    assert_eq!(types(true), expected.cloned().collect::<Vec<_>>());
  }

  #[test]
  fn cpu_tier_gauges_have_a_series_per_tier() {
    let tier = |label: &str, freq_mhz| CpuTierMetrics {
      label: label.into(),
      freq_mhz,
      ..Default::default()
    };
    let metrics =
      Metrics { cpu_tiers: vec![tier("E", 1800), tier("P", 3200)], ..Default::default() };
    let output = to_prometheus(&metrics, &SocInfo::default());
    assert!(output.contains("macmon_cpu_tier_freq_mhz{chip=\"\",tier=\"E\"} 1800\n"));
    assert!(output.contains("macmon_cpu_tier_freq_mhz{chip=\"\",tier=\"P\"} 3200\n"));
    assert_eq!(output.matches("# TYPE macmon_cpu_tier_active_ratio gauge").count(), 1);

    let output = to_prometheus(&Metrics::default(), &SocInfo::default());
    assert!(!output.contains("# TYPE macmon_cpu_tier_"));
  }

  #[test]
  fn agent_plist_stays_the_same() {
    assert_eq!(plist("/opt/homebrew/bin/macmon", "0.0.0.0", 9090, None), AGENT_PLIST);
  }

  #[test]
  fn daemon_plist_names_its_user() {
    let user = "  <key>UserName</key>\n  <string>a&amp;b</string>\n";
    let expected = AGENT_PLIST.replace("</dict>", &format!("{user}</dict>"));
    assert_eq!(plist("/opt/homebrew/bin/macmon", "0.0.0.0", 9090, Some("a&b")), expected);
  }

  #[test]
  fn daemon_needs_a_non_root_sudo_user() {
    assert_eq!(daemon_user(Some("user".into())), Some("user".into()));

    for value in [None, Some(String::new()), Some("root".into())] {
      assert_eq!(daemon_user(value), None);
    }
  }

  #[test]
  fn install_refusal_gives_both_commands_with_own_args() {
    let output = install_refusal("macmon serve --port 8080 --install");
    assert!(output.contains("\n  sudo macmon serve --port 8080 --install\n"));
    assert!(output.ends_with("\n  macmon serve --port 8080 --install --agent"));
  }

  #[test]
  fn agent_refusal_gives_the_removal_command() {
    assert!(agent_refusal("./macmon").ends_with("run: sudo ./macmon serve --uninstall"));
  }

  #[test]
  fn plist_escapes_its_values() {
    let output = plist("/a&b/macmon", "<host>", 8080, None);
    assert!(output.contains("<string>/a&amp;b/macmon</string>"));
    assert!(output.contains("<string>&lt;host&gt;</string>"));
    assert!(output.contains("<string>8080</string>"));
  }

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
  fn includes_usage_fields_in_json() {
    let json: serde_json::Value =
      serde_json::from_str(&to_json(&Metrics::default(), &SocInfo::default())).unwrap();

    for field in ["cpu_tiers", "cpu_usage_pct", "ecpu_usage", "pcpu_usage", "gpu_usage"] {
      assert!(json.get(field).is_some(), "missing {field}");
    }
  }
}
