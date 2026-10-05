use core_foundation::{
  base::{CFType, CFTypeRef, TCFType},
  data::CFData,
  dictionary::CFDictionary,
  number::CFNumber,
  propertylist::create_with_data,
  string::{CFString, CFStringRef},
};
use macmon::sources::{
  IOReport, IOServiceIterator, WithError, cfdict_get_val, cfio_integer_value, cfio_watts,
};
use std::{
  collections::BTreeMap,
  io::{BufRead, BufReader, Read},
  path::{Component, Path},
  process::{Child, Command, Stdio},
  sync::mpsc,
  thread,
  time::{Duration, Instant},
};

// MARK: Find CLPC power counters

#[derive(Default)]
struct Sample {
  seconds: f64,
  raw: BTreeMap<String, i64>,
  legacy_watts: BTreeMap<String, f64>,
  powermetrics_watts: Option<BTreeMap<String, f64>>,
}

struct Phase {
  load: String,
  samples: Vec<Sample>,
}

struct Mapping {
  component: &'static str,
  status: &'static str,
  source: &'static str,
  matching_ids: Vec<String>,
  behavioral_ids: Vec<String>,
}

pub fn run(powermetrics: bool) -> WithError<()> {
  if unsafe { libc::geteuid() } == 0 {
    return Err(
      "Run macmon find-clpc as your normal user; --powermetrics elevates only the reference".into(),
    );
  }

  eprintln!("Reading the active AppleCLPC driver and report tables...");
  let mut services = Vec::new();
  for (entry, _) in IOServiceIterator::new("AppleCLPC")? {
    let entry = Registry(entry);
    let bundle = property(entry.0, "CFBundleIdentifier")?
      .downcast::<CFString>()
      .ok_or("Invalid CLPC bundle identifier")?
      .to_string();
    let mut id = 0;
    if unsafe { IORegistryEntryGetRegistryEntryID(entry.0, &mut id) } != 0 {
      return Err("Cannot read CLPC registry ID".into());
    }
    services.push((id, bundle));
  }
  if services.len() != 1 {
    return Err("Power diagnostics currently require exactly one AppleCLPC service".into());
  }

  let (driver, bundle) = &services[0];
  let counters = parse_table(&kernel_image()?, bundle)?;
  let ids =
    counters.iter().map(|x| u64::from_str_radix(&x[2..], 16)).collect::<Result<Vec<_>, _>>()?;
  eprintln!("{bundle}: {} scalar counters", ids.len());

  if powermetrics {
    eprintln!(
      "Authorizing Apple powermetrics only; diagnostics and workloads remain unprivileged."
    );
    // Test the actual command: sudo -l can succeed even when execution needs a password.
    let mut probe = Process(
      reference_command("1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?,
    );
    let allowed = probe.wait(Duration::from_secs(3)).is_ok();
    if !allowed && !Command::new("/usr/bin/sudo").args(["-v"]).status()?.success() {
      return Err("sudo authorization for powermetrics failed".into());
    }
  }

  eprintln!("Keep other applications idle. Testing CPU, GPU and ANE separately (about a minute).");
  if !powermetrics {
    eprintln!(
      "If a reference is unavailable, extra load/recovery tests take about 2 more minutes."
    );
  }
  let mut reference = if powermetrics { Some(start_reference()?) } else { None };
  let mut phases = Vec::new();
  let mut schedule = vec!["idle", "cpu", "gpu", "ane"];
  while let Some(&mode) = schedule.get(phases.len()) {
    eprintln!("Test {}/{}: {}...", phases.len() + 1, schedule.len(), load_label(mode));
    let mut load = if mode == "idle" { None } else { Some(start_load(mode)?) };
    thread::sleep(Duration::from_secs(2));
    let mut ior = IOReport::with_clpc_reports(*driver, &ids)?;
    if let Some(reference) = &mut reference {
      // Begin at the next reference boundary, after both samplers have warmed up.
      while reference.output.try_recv().is_ok() {}
      reference.next()?;
    }
    let mut samples = Vec::new();
    for _ in 0..6 {
      let (sample, msec) = ior.get_samples(1000, 1).pop().ok_or("Missing diagnostic sample")?;
      let dt = Duration::from_millis(msec);
      let mut row = Sample { seconds: dt.as_secs_f64(), ..Default::default() };
      for x in sample {
        if x.group == "CLPC" {
          row.raw.insert(x.channel, cfio_integer_value(x.item));
          continue;
        }
        let component = if x.group == "Energy Model" {
          match x.channel.as_str() {
            x if x.ends_with("CPU Energy") => "cpu",
            "GPU Energy" => "gpu",
            x if x.starts_with("ANE") => "ane",
            _ => continue,
          }
        } else if x.group == "PMP" && x.subgroup == "Energy Counters" {
          "ane_pmp"
        } else {
          continue;
        };
        let watts = cfio_watts(x.item, &x.unit, dt)? as f64;
        *row.legacy_watts.entry(component.into()).or_default() += watts;
      }
      row.powermetrics_watts = reference.as_mut().map(Reference::next).transpose()?;
      samples.push(row);
    }
    if samples.iter().any(|x| x.raw.is_empty()) {
      return Err("The driver accepted no discovered scalar reports".into());
    }
    if let Some(load) = &mut load
      && load.0.try_wait()?.is_some()
    {
      return Err(format!("{mode} workload exited before sampling completed").into());
    }
    if let Some(mut load) = load {
      load.wait(Duration::from_secs(10))?;
    }
    phases.push(Phase { load: mode.into(), samples });
    if !powermetrics
      && phases.len() == 4
      && classify(&counters, &phases, false).iter().any(|x| x.status != "verified")
    {
      eprintln!(
        "Some channels need more evidence. Checking repeatability, recovery and mixed load."
      );
      for load in ["cpu-light", "ane", "gpu", "cpu", "all"] {
        schedule.extend(["idle", load]);
      }
      schedule.push("idle");
    }
  }
  drop(reference);
  print!("{}", summary(&classify(&counters, &phases, powermetrics)));
  Ok(())
}

fn load_label(mode: &str) -> &str {
  match mode {
    "idle" => "idle / recovery",
    "cpu" => "CPU, all threads",
    "cpu-light" => "CPU, one thread",
    "gpu" => "GPU",
    "ane" => "ANE, text recognition",
    "all" => "CPU + GPU + ANE",
    _ => mode,
  }
}

fn summary(mappings: &[Mapping]) -> String {
  use std::fmt::Write;

  let mut out = String::new();
  for mapping in mappings {
    let status = match mapping.status {
      "verified" => "verified",
      "no_active_reference" => "no active reference",
      "no_match" => "no match",
      _ => "ambiguous match",
    };
    writeln!(out, "{}: {status} ({})", mapping.component.to_uppercase(), mapping.source).unwrap();
    if !mapping.matching_ids.is_empty() {
      writeln!(out, "  Matched IDs: {}", mapping.matching_ids.join(", ")).unwrap();
    }
    if mapping.status != "verified" && !mapping.behavioral_ids.is_empty() {
      writeln!(out, "  Candidates (unverified): {}", mapping.behavioral_ids.join(", ")).unwrap();
    }
  }
  out
}

// MARK: Independent validation

fn mean(phase: &Phase, get: impl Fn(&Sample) -> Option<f64>) -> Option<f64> {
  let mut sum = 0.;
  let mut seconds = 0.;
  for row in &phase.samples {
    let value = get(row)?;
    if !value.is_finite() || value < 0. || row.seconds <= 0. {
      return None;
    }
    sum += value * row.seconds;
    seconds += row.seconds;
  }
  (seconds > 0.).then(|| sum / seconds)
}

fn classify(counters: &[String], phases: &[Phase], powermetrics: bool) -> Vec<Mapping> {
  // nJ is a hypothesis until matched to independent energy measurements.
  let values: Vec<_> = counters
    .iter()
    .filter_map(|id| {
      let watts: Option<Vec<f64>> = phases
        .iter()
        .map(|phase| mean(phase, |row| Some(*row.raw.get(id)? as f64 / row.seconds / 1e9)))
        .collect();
      watts.map(|watts| (id, watts))
    })
    .collect();
  ["cpu", "gpu", "ane"]
    .into_iter()
    .map(|component| {
      let source = if powermetrics {
        "powermetrics"
      } else if component == "ane"
        && phases
          .iter()
          .any(|p| mean(p, |r| r.legacy_watts.get("ane_pmp").copied()).unwrap_or(0.) > 0.05)
        && !phases
          .iter()
          .any(|p| mean(p, |r| r.legacy_watts.get("ane").copied()).unwrap_or(0.) > 0.05)
      {
        "PMP"
      } else {
        "Energy Model"
      };
      let reference: Option<Vec<f64>> = phases
        .iter()
        .map(|phase| {
          if powermetrics {
            mean(phase, |row| row.powermetrics_watts.as_ref()?.get(component).copied())
          } else {
            let key = if source == "PMP" { "ane_pmp" } else { component };
            mean(phase, |row| row.legacy_watts.get(key).copied())
          }
        })
        .collect();
      let loaded = phases.iter().position(|x| x.load == component);
      let idle = phases.iter().position(|x| x.load == "idle");
      let active =
        reference.as_ref().zip(loaded.zip(idle)).is_some_and(|(values, (load, idle))| {
          values.iter().all(|x| x.is_finite() && *x >= 0.)
            && values[load] > 0.1
            && values[load] > values[idle] * 1.2 + 0.05
        });
      let mut matches = Vec::new();
      let mut behavioral_ids = Vec::new();
      for (id, watts) in &values {
        let matched = active
          && reference.as_ref().is_some_and(|reference| {
            watts.iter().zip(reference).all(|(a, b)| (a - b).abs() <= (b * 0.05).max(0.03))
          });
        let reactive = loaded
          .zip(idle)
          .is_some_and(|(load, idle)| watts[load] > 0.1 && watts[load] > watts[idle] * 1.2 + 0.05);
        if (reactive || matched) && follows_load(watts, phases, component) {
          behavioral_ids.push((*id).clone());
        }
        if matched {
          matches.push((*id).clone());
        }
      }
      let status = match (active, matches.len()) {
        (false, _) => "no_active_reference",
        (true, 1) => "verified",
        (true, 0) => "no_match",
        _ => "ambiguous",
      };
      Mapping { component, status, source, matching_ids: matches, behavioral_ids }
    })
    .collect()
}

// A behavioral shortlist, never independent verification of identity or units.
fn follows_load(values: &[f64], phases: &[Phase], component: &str) -> bool {
  let rise = |index: usize| {
    let idle = phases[..index].iter().rposition(|x| x.load == "idle")?;
    // Use the quieter adjacent control: unrelated work can also spoil an idle phase.
    let baseline = if phases.get(index + 1).is_some_and(|x| x.load == "idle") {
      values[idle].min(values[index + 1])
    } else {
      values[idle]
    };
    Some((values[index] - baseline).max(0.))
  };
  let loads: Vec<usize> =
    phases.iter().enumerate().filter(|(_, x)| x.load == component).map(|(i, _)| i).collect();
  let Some(light) = phases.iter().position(|x| x.load == "cpu-light") else { return false };
  let Some(mixed) = phases.iter().position(|x| x.load == "all") else { return false };
  if loads.len() < 2 {
    return false;
  }

  let peak = loads.iter().filter_map(|&i| rise(i)).fold(0., f64::max);
  if peak < 0.1 || loads.iter().any(|&i| rise(i).unwrap_or(0.) < (peak * 0.25).max(0.05)) {
    return false;
  }

  // Require recovery after a repeated load, allowing noise and some lingering activity.
  let recovery: Vec<usize> = loads
    .iter()
    .copied()
    .filter(|&i| phases.get(i + 1).is_some_and(|p| p.load == "idle"))
    .collect();
  let recovers = !recovery.is_empty()
    && recovery.iter().all(|&i| values[i] - values[i + 1] > rise(i).unwrap_or(0.) * 0.65);
  if !recovers || rise(mixed).unwrap_or(0.) < peak * 0.25 {
    return false;
  }
  if component == "cpu" && !(0.05..peak * 0.9).contains(&rise(light).unwrap_or(0.)) {
    return false;
  }

  ["cpu", "gpu", "ane", "cpu-light"].iter().all(|&mode| {
    if mode == component || (component == "cpu" && mode == "cpu-light") {
      return true;
    }
    // OCR also uses the CPU; do not require a fictitious CPU-free ANE workload.
    // At least one quiet control is needed; background work can spoil another repeat.
    let limit = if component == "cpu" && mode == "ane" { 0.85 } else { 0.35 };
    phases
      .iter()
      .enumerate()
      .any(|(i, phase)| phase.load == mode && rise(i).unwrap_or(0.) <= (peak * limit).max(0.05))
  })
}

struct Process(Child);

impl Process {
  fn wait(&mut self, timeout: Duration) -> WithError<()> {
    let start = Instant::now();
    loop {
      if let Some(status) = self.0.try_wait()? {
        return if status.success() {
          Ok(())
        } else {
          Err(format!("Child exited: {status}").into())
        };
      }
      if start.elapsed() > timeout {
        return Err("Timed out waiting for bounded subprocess".into());
      }
      thread::sleep(Duration::from_millis(50));
    }
  }
}

impl Drop for Process {
  fn drop(&mut self) {
    if self.0.try_wait().ok().flatten().is_none() {
      // sudo forwards SIGTERM to powermetrics; killing sudo itself can orphan it.
      unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
      let _ = self.0.wait();
    }
  }
}

fn start_load(mode: &str) -> WithError<Process> {
  let mut command = Command::new(std::env::current_exe()?);
  command.args(["stress", if mode == "cpu-light" { "cpu" } else { mode }, "--duration", "12"]);
  if mode == "cpu-light" {
    command.args(["--workers", "1"]);
  }
  let mut child =
    Process(command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?);
  let stderr = child.0.stderr.take().ok_or("Missing workload stderr")?;
  let (tx, rx) = mpsc::channel();
  thread::spawn(move || {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
      if line.contains("warmup recognized") {
        let _ = tx.send(());
      }
      eprintln!("{line}");
    }
  });
  if matches!(mode, "ane" | "all") {
    rx.recv_timeout(Duration::from_secs(60)).map_err(|_| "ANE warmup failed or timed out")?;
  }
  Ok(child)
}

struct Reference {
  _process: Process,
  output: mpsc::Receiver<Result<BTreeMap<String, f64>, String>>,
}

impl Reference {
  fn next(&mut self) -> WithError<BTreeMap<String, f64>> {
    self.output.recv_timeout(Duration::from_secs(3))?.map_err(Into::into)
  }
}

fn start_reference() -> WithError<Reference> {
  let mut process = Process(
    reference_command("120")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .spawn()?,
  );
  // Drain immediately so pipe backpressure cannot delay reference samples.
  let stdout = process.0.stdout.take().ok_or("Missing powermetrics stdout")?;
  let (tx, output) = mpsc::channel();
  thread::spawn(move || {
    let mut stdout = BufReader::new(stdout);
    loop {
      let result = match read_reference(&mut stdout) {
        Ok(Some(sample)) => Ok(sample),
        Ok(None) => break,
        Err(err) => Err(err.to_string()),
      };
      let failed = result.is_err();
      if tx.send(result).is_err() || failed {
        break;
      }
    }
  });
  Ok(Reference { _process: process, output })
}

fn reference_command(samples: &str) -> Command {
  let mut cmd = Command::new("/usr/bin/sudo");
  cmd.arg("-n");
  cmd.args([
    "/usr/bin/powermetrics",
    "--samplers",
    "cpu_power,gpu_power,ane_power",
    "--show-extra-power-info",
    "--format",
    "plist",
    "-i",
    "1000",
    "-n",
    samples,
    "--buffer-size",
    "0",
  ]);
  cmd
}

fn dict_value(dict: &CFDictionary, key: &str) -> Option<CFType> {
  let ptr = cfdict_get_val(dict.as_concrete_TypeRef(), key)?;
  Some(unsafe { CFType::wrap_under_get_rule(ptr) })
}

fn read_reference(reader: &mut impl BufRead) -> WithError<Option<BTreeMap<String, f64>>> {
  let mut data = Vec::new();
  loop {
    // The NUL separator arrives with the next sample, one interval too late.
    // Deliver immediately on the closing plist tag instead.
    let count =
      reader.by_ref().take((8 * 1024 * 1024 - data.len()) as u64).read_until(b'>', &mut data)?;
    if data.ends_with(b"</plist>") {
      return parse_reference(&data).map(Some);
    }
    if count == 0 && data.iter().all(|x| *x == 0 || x.is_ascii_whitespace()) {
      return Ok(None);
    }
    if count == 0 || data.len() >= 8 * 1024 * 1024 {
      return Err("Truncated or oversized powermetrics sample".into());
    }
  }
}

fn parse_reference(data: &[u8]) -> WithError<BTreeMap<String, f64>> {
  let mut frames = data.split(|x| *x == 0).filter(|x| !x.iter().all(u8::is_ascii_whitespace));
  let plist = frames.next().ok_or("Missing powermetrics sample")?;
  if frames.next().is_some() {
    return Err("Expected one powermetrics sample".into());
  }

  let (ptr, _) = create_with_data(CFData::from_buffer(plist), 0)
    .map_err(|_| "Cannot parse powermetrics plist")?;
  let root = unsafe { CFType::wrap_under_create_rule(ptr) };
  let root = root.downcast::<CFDictionary>().ok_or("Invalid power plist")?;
  let number = |dict: &CFDictionary, key: &str| -> WithError<f64> {
    let value = dict_value(dict, key)
      .and_then(|x| x.downcast::<CFNumber>())
      .and_then(|x| x.to_f64())
      .ok_or_else(|| format!("Missing powermetrics {key}"))?;
    if !value.is_finite() || value < 0. {
      return Err("Invalid reference measurement".into());
    }
    Ok(value)
  };
  if !(0.8..=1.5).contains(&(number(&root, "elapsed_ns")? / 1e9)) {
    return Err("Incomplete or delayed powermetrics reference window".into());
  }
  let processor = dict_value(&root, "processor")
    .and_then(|x| x.downcast::<CFDictionary>())
    .ok_or("Missing powermetrics processor dictionary")?;
  ["cpu", "gpu", "ane"]
    .into_iter()
    .map(|name| Ok((name.into(), number(&processor, &format!("{name}_power"))? / 1000.)))
    .collect()
}

// MARK: Active kernel image (IORegistry and IMG4/LZFSE, no developer tools)

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
  fn IORegistryEntryFromPath(port: u32, path: *const i8) -> u32;
  fn IORegistryEntryCreateCFProperty(
    entry: u32,
    key: CFStringRef,
    allocator: *const std::ffi::c_void,
    options: u32,
  ) -> CFTypeRef;
  fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
  fn IOObjectRelease(entry: u32) -> u32;
}

#[link(name = "compression")]
unsafe extern "C" {
  fn compression_decode_buffer(
    dst: *mut u8,
    dst_size: usize,
    src: *const u8,
    src_size: usize,
    scratch: *mut u8,
    algorithm: u32,
  ) -> usize;
}

struct Registry(u32);
impl Drop for Registry {
  fn drop(&mut self) {
    unsafe { IOObjectRelease(self.0) };
  }
}

fn property(entry: u32, key: &str) -> WithError<CFType> {
  let key = CFString::new(key);
  let value = unsafe {
    IORegistryEntryCreateCFProperty(entry, key.as_concrete_TypeRef(), std::ptr::null(), 0)
  };
  if value.is_null() {
    return Err("Required IORegistry property unavailable".into());
  }
  Ok(unsafe { CFType::wrap_under_create_rule(value) })
}

fn kernel_image() -> WithError<Vec<u8>> {
  let chosen = Registry(unsafe { IORegistryEntryFromPath(0, c"IODeviceTree:/chosen".as_ptr()) });
  if chosen.0 == 0 {
    return Err("Boot device tree unavailable".into());
  }
  let path = property(chosen.0, "boot-objects-path")?;
  let path = path.downcast::<CFData>().ok_or("Invalid boot-objects-path type")?;
  let path = std::str::from_utf8(path.bytes())?.trim_end_matches('\0');
  if !path.starts_with('/')
    || Path::new(path).components().any(|x| !matches!(x, Component::RootDir | Component::Normal(_)))
  {
    return Err("Invalid boot-objects-path".into());
  }
  let path = Path::new("/System/Volumes/Preboot")
    .join(path.trim_start_matches('/'))
    .join("System/Library/Caches/com.apple.kernelcaches/kernelcache");
  let mut data = Vec::new();
  std::fs::File::open(path)?.take(256 * 1024 * 1024 + 1).read_to_end(&mut data)?;
  if data.len() > 256 * 1024 * 1024 {
    return Err("Kernel image exceeds size limit".into());
  }
  unpack(&data)
}

fn der(data: &[u8]) -> WithError<(u8, &[u8], &[u8])> {
  if data.len() < 2 {
    return Err("Truncated IMG4 header".into());
  }
  let mut size = data[1] as usize;
  let mut offset = 2;
  if size & 0x80 != 0 {
    let n = size & 0x7f;
    if n == 0 || n > 4 || data.len() < 2 + n {
      return Err("Invalid IMG4 length".into());
    }
    size = 0;
    for byte in &data[2..2 + n] {
      size = size * 256 + *byte as usize;
    }
    offset += n;
  }
  let end = offset + size;
  if end > data.len() {
    return Err("Truncated IMG4 payload".into());
  }
  Ok((data[0], &data[offset..end], &data[end..]))
}

fn unpack(data: &[u8]) -> WithError<Vec<u8>> {
  if data.starts_with(&0xfeedfacfu32.to_le_bytes()) {
    return Ok(data.to_vec());
  }
  let (tag, seq, _) = der(data)?;
  if tag != 0x30 {
    return Err("Expected IMG4 sequence".into());
  }
  let (_, magic, rest) = der(seq)?;
  let seq = if magic == b"IMG4" {
    let (tag, seq, _) = der(rest)?;
    if tag != 0x30 {
      return Err("Expected IM4P sequence".into());
    }
    seq
  } else {
    seq
  };
  let (_, magic, rest) = der(seq)?;
  let (_, kind, rest) = der(rest)?;
  let (_, _, rest) = der(rest)?;
  let (tag, payload, _) = der(rest)?;
  if magic != b"IM4P" || kind != b"krnl" || tag != 4 {
    return Err("Not a kernel IM4P payload".into());
  }
  if payload.starts_with(&0xfeedfacfu32.to_le_bytes()) {
    return Ok(payload.to_vec());
  }
  if !payload.starts_with(b"bvx2") {
    return Err("Unsupported kernel compression (expected LZFSE)".into());
  }
  for size in [128, 256, 512].map(|x| x * 1024 * 1024) {
    let mut out = vec![0; size];
    let len = unsafe {
      compression_decode_buffer(
        out.as_mut_ptr(),
        out.len(),
        payload.as_ptr(),
        payload.len(),
        std::ptr::null_mut(),
        0x801,
      )
    };
    if len > 0 && len < size {
      out.truncate(len);
      return Ok(out);
    }
  }
  Err("Cannot decompress kernel within 512 MiB limit".into())
}

// MARK: Mach-O fileset and CLPC tables
// Layouts: Apple mach-o/loader.h and mach-o/fixup-chains.h.
// Only scalar uint64 report actions are subscribed; state/array reports are excluded.

struct Bytes<'a>(&'a [u8]);
impl<'a> Bytes<'a> {
  fn slice(&self, off: usize, len: usize) -> WithError<&'a [u8]> {
    self
      .0
      .get(off..off.checked_add(len).ok_or("Image offset overflow")?)
      .ok_or_else(|| "Truncated kernel structure".into())
  }
  fn u32(&self, off: usize) -> WithError<u32> {
    Ok(u32::from_le_bytes(self.slice(off, 4)?.try_into()?))
  }
  fn u64(&self, off: usize) -> WithError<u64> {
    Ok(u64::from_le_bytes(self.slice(off, 8)?.try_into()?))
  }
  fn string(&self, off: usize, len: usize) -> WithError<&'a str> {
    let bytes = self.slice(off, len)?;
    let end = bytes.iter().position(|x| *x == 0).ok_or("Unterminated kernel string")?;
    Ok(std::str::from_utf8(&bytes[..end])?)
  }
  fn commands(&self, header: usize) -> WithError<Vec<Bytes<'a>>> {
    if self.u32(header)? != 0xfeedfacf || self.u32(header + 4)? != 0x0100000c {
      return Err("Expected ARM64 Mach-O image".into());
    }
    let mut rest = self.slice(header + 32, self.u32(header + 20)? as usize)?;
    let mut commands = Vec::new();
    for _ in 0..self.u32(header + 16)? {
      let size = Bytes(rest).u32(4)? as usize;
      if size < 8 || size > rest.len() {
        return Err("Invalid Mach-O load command".into());
      }
      commands.push(Bytes(&rest[..size]));
      rest = &rest[size..];
    }
    if !rest.is_empty() {
      return Err("Inconsistent Mach-O load command count".into());
    }
    Ok(commands)
  }
}

fn parse_table(data: &[u8], bundle: &str) -> WithError<Vec<String>> {
  let data = Bytes(data);
  if data.u32(12)? != 12 {
    return Err("Expected a kernel Mach-O fileset".into());
  }
  let mut segments = Vec::new();
  let mut header = None;
  let mut fixups = None;
  for cmd in data.commands(0)? {
    match cmd.u32(0)? {
      0x19 => segments.push((cmd.u64(24)?, cmd.u64(48)?, cmd.u64(40)?)),
      0x80000035 => {
        let start = cmd.u32(24)? as usize;
        if start >= cmd.0.len() {
          return Err("Invalid fileset entry name".into());
        }
        if cmd.string(start, cmd.0.len() - start)? == bundle {
          if header.is_some() {
            return Err("Duplicate CLPC fileset entry".into());
          }
          header = Some(cmd.u64(16)? as usize);
        }
      }
      0x80000034 => fixups = Some(data.slice(cmd.u32(8)? as usize, cmd.u32(12)? as usize)?),
      _ => {}
    }
  }
  let base = segments.iter().map(|x| x.0).min().ok_or("Missing kernel segments")?;
  let at = |addr: u64, len: usize| -> WithError<usize> {
    for (vm, size, off) in &segments {
      if let Some(delta) = addr.checked_sub(*vm)
        && delta.checked_add(len as u64).is_some_and(|end| end <= *size)
      {
        let off = off.checked_add(delta).ok_or("Segment offset overflow")? as usize;
        data.slice(off, len)?;
        return Ok(off);
      }
    }
    Err("Report table is outside mapped kernel segments".into())
  };
  let fixups = Bytes(fixups.ok_or("Missing chained kernel fixups")?);
  let start = fixups.u32(4)? as usize;
  let count = fixups.u32(start)? as usize;
  fixups.slice(start + 4, count.checked_mul(4).ok_or("Invalid fixup count")?)?;
  let mut formats = Vec::new();
  for i in 0..count {
    let off = fixups.u32(start + 4 + i * 4)? as usize;
    if off != 0 {
      formats.push(u16::from_le_bytes(fixups.slice(start + off + 6, 2)?.try_into()?));
    }
  }
  if formats.is_empty() || formats.iter().any(|x| *x != 8) {
    return Err("Unsupported kernel chained pointer format".into());
  }
  let pointer = |value: u64| -> WithError<u64> {
    if (value >> 30) & 3 != 0 {
      return Err("Unsupported kernel pointer cache level".into());
    }
    base.checked_add(value & 0x3fffffff).ok_or_else(|| "Kernel pointer overflow".into())
  };

  let mut symbols = BTreeMap::new();
  for cmd in data.commands(header.ok_or("Active CLPC driver not found in boot kernel")?)? {
    if cmd.u32(0)? == 2 {
      let off = cmd.u32(8)? as usize;
      let count = cmd.u32(12)? as usize;
      let strings = Bytes(data.slice(cmd.u32(16)? as usize, cmd.u32(20)? as usize)?);
      data.slice(off, count.checked_mul(16).ok_or("Invalid symbol count")?)?;
      for i in 0..count {
        let sym = off + i * 16;
        if data.slice(sym + 4, 1)?[0] & 0xee != 0x0e {
          continue;
        }
        let index = data.u32(sym)? as usize;
        if index >= strings.0.len() {
          return Err("Invalid symbol string index".into());
        }
        let name = strings.string(index, strings.0.len() - index)?;
        symbols.insert(name, data.u64(sym + 8)?);
      }
    }
  }
  let symbol =
    |name: &str| symbols.get(name).copied().ok_or_else(|| format!("Missing CLPC symbol: {name}"));
  let names = symbol("__ZN4clpc12_GLOBAL__N_119merged_report_namesE")?;
  let actions = symbol("__ZN4clpc12_GLOBAL__N_121merged_report_actionsE")?;
  let span = |addr| -> WithError<usize> {
    symbols
      .values()
      .filter(|x| **x > addr)
      .min()
      .map(|x| (*x - addr) as usize)
      .ok_or_else(|| "Cannot determine CLPC table size".into())
  };
  let action_size = span(actions)?;
  let name_size = span(names)?;
  let count = action_size / 24;
  if count == 0 || count > 1024 || action_size % 24 != 0 || name_size % count != 0 {
    return Err("Unsupported CLPC table boundaries".into());
  }
  let stride = name_size / count;
  if ![72, 80].contains(&stride) {
    return Err(format!("Unsupported CLPC name stride: {stride}").into());
  }
  let names = at(names, name_size)?;
  let actions = at(actions, action_size)?;
  let configure = symbol("__ZN4clpc21configureSimpleReportIyEENS_6report11ChannelTypeEyytbPv")?;
  let generate = symbol("__ZN4clpc20generateSimpleReportIyEEvRiR24IOBufferMemoryDescriptoryytPv")?;
  let mut counters = Vec::new();
  for i in 0..count {
    if pointer(data.u64(actions + i * 24 + 8)?)? != configure
      || pointer(data.u64(actions + i * 24 + 16)?)? != generate
    {
      continue;
    }
    let name = data.string(names + i * stride, 56)?;
    if name.is_empty() || !name.bytes().all(|x| x.is_ascii_graphic()) {
      return Err("Invalid CLPC report name".into());
    }
    let id = ((i as u64) << 32) | data.u32(names + i * stride + 64)? as u64;
    counters.push(format!("0x{id:016x}"));
  }
  if counters.is_empty() {
    return Err("No supported scalar CLPC reports found".into());
  }
  Ok(counters)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fixture(stride: usize) -> Vec<u8> {
    let mut data = vec![0; 4096];
    let put32 = |data: &mut [u8], off: usize, value: u32| {
      data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    };
    let put64 = |data: &mut [u8], off: usize, value: u64| {
      data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    };
    for header in [0, 256] {
      put32(&mut data, header, 0xfeedfacf);
      put32(&mut data, header + 4, 0x0100000c);
    }
    put32(&mut data, 12, 12);
    put32(&mut data, 16, 3);
    put32(&mut data, 20, 128);
    put32(&mut data, 32, 0x19);
    put32(&mut data, 36, 72);
    put64(&mut data, 56, 0x100000);
    put64(&mut data, 80, 4096);
    put32(&mut data, 104, 0x80000035);
    put32(&mut data, 108, 40);
    put64(&mut data, 120, 256);
    put32(&mut data, 128, 32);
    data[136..140].copy_from_slice(b"clpc");
    put32(&mut data, 144, 0x80000034);
    put32(&mut data, 148, 16);
    put32(&mut data, 152, 768);
    put32(&mut data, 156, 64);
    put32(&mut data, 772, 28);
    put32(&mut data, 796, 1);
    put32(&mut data, 800, 8);
    data[810] = 8;

    let symbols = [
      ("__ZN4clpc12_GLOBAL__N_119merged_report_namesE", 2048),
      ("names_end", 2048 + 3 * stride),
      ("__ZN4clpc12_GLOBAL__N_121merged_report_actionsE", 3008),
      ("actions_end", 3080),
      ("__ZN4clpc21configureSimpleReportIyEENS_6report11ChannelTypeEyytbPv", 4000),
      ("__ZN4clpc20generateSimpleReportIyEEvRiR24IOBufferMemoryDescriptoryytPv", 4008),
    ];
    let mut strings = vec![0];
    for (i, (name, off)) in symbols.iter().enumerate() {
      put32(&mut data, 1024 + i * 16, strings.len() as u32);
      data[1024 + i * 16 + 4] = 0x0e;
      put64(&mut data, 1024 + i * 16 + 8, 0x100000 + *off as u64);
      strings.extend(name.bytes());
      strings.push(0);
    }
    data[1408..1408 + strings.len()].copy_from_slice(&strings);
    put32(&mut data, 272, 2);
    put32(&mut data, 276, 48);
    for (i, value) in [2, 24, 1024, 6, 1408, strings.len() as u32, 0x1b, 24].iter().enumerate() {
      put32(&mut data, 288 + i * 4, *value);
    }
    for i in 0..3 {
      data[2048 + i * stride] = b'a' + i as u8;
      put32(&mut data, 2048 + i * stride + 64, 100 + i as u32);
      put64(&mut data, 3008 + i * 24 + 8, 4000);
      put64(&mut data, 3008 + i * 24 + 16, if i == 2 { 4016 } else { 4008 });
    }
    data
  }

  #[test]
  fn discovers_ids_from_both_table_layouts_and_excludes_other_actions() {
    for stride in [72, 80] {
      assert_eq!(
        parse_table(&fixture(stride), "clpc").unwrap(),
        ["0x0000000000000064", "0x0000000100000065"]
      );
    }
  }

  #[test]
  fn rejects_truncated_images_unknown_layouts_and_pointer_formats() {
    let data = fixture(80);
    for size in 0..3080 {
      assert!(parse_table(&data[..size], "clpc").is_err(), "accepted {size} bytes");
    }
    assert!(parse_table(&fixture(88), "clpc").is_err());
    assert!(parse_table(&data, "different.driver").is_err());
    let mut data = data;
    data[810] = 7;
    assert!(parse_table(&data, "clpc").is_err());
    assert!(unpack(&[0x30, 0x80]).is_err());
    assert!(unpack(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_err());
  }

  fn measurements() -> (Vec<String>, Vec<Phase>) {
    let counters = ["cpu", "gpu", "ane"].map(String::from).to_vec();
    let mut phases = Vec::new();
    for (load, values) in [
      ("idle", [1., 0.1, 0.]),
      ("cpu", [12., 0.1, 0.]),
      ("gpu", [2., 7., 0.]),
      ("ane", [3., 0.1, 0.8]),
    ] {
      let raw =
        counters.iter().zip(values).map(|(x, watts)| (x.clone(), (watts * 1e9) as i64)).collect();
      let legacy_watts = counters.iter().zip(values).map(|(x, watts)| (x.clone(), watts)).collect();
      phases.push(Phase {
        load: load.into(),
        samples: vec![Sample { seconds: 1., raw, legacy_watts, powermetrics_watts: None }],
      });
    }
    (counters, phases)
  }

  #[test]
  fn verifies_independent_matches_but_not_zeros_missing_samples_or_ambiguous_ids() {
    let (mut counters, mut phases) = measurements();
    assert!(classify(&counters, &phases, false).iter().all(|x| x.status == "verified"));
    counters.push("combined".into());
    for phase in &mut phases {
      let raw = &mut phase.samples[0].raw;
      raw.insert("combined".into(), raw["cpu"] + raw["gpu"] + raw["ane"]);
    }
    let mappings = classify(&counters, &phases, false);
    assert!(mappings.iter().all(|x| x.status == "verified"));
    assert!(mappings.iter().all(|x| !x.matching_ids.iter().any(|id| id == "combined")));
    for phase in &mut phases {
      phase.samples[0].legacy_watts.insert("ane".into(), 0.);
      let value = phase.samples[0].raw["cpu"];
      phase.samples[0].raw.insert("duplicate".into(), value);
    }
    counters.push("duplicate".into());
    let mappings = classify(&counters, &phases, false);
    assert_eq!(mappings[0].status, "ambiguous");
    assert_eq!(mappings[2].status, "no_active_reference");
    phases[0].samples[0].raw.remove("gpu");
    assert_eq!(classify(&counters, &phases, false)[1].status, "no_match");
  }

  #[test]
  fn load_patterns_narrow_candidates_without_claiming_units_or_resolving_scaled_counters() {
    let (mut counters, mut phases) = measurements();
    for (load, values) in [
      ("idle", [1., 0.1, 0.]),
      ("cpu-light", [3., 0.1, 0.]),
      ("idle", [1., 0.1, 0.]),
      ("ane", [3., 0.1, 0.8]),
      ("idle", [1., 0.1, 0.]),
      ("gpu", [2., 7., 0.]),
      ("idle", [1., 0.1, 0.]),
      ("cpu", [12., 0.1, 0.]),
      ("idle", [1., 0.1, 0.]),
      ("all", [10., 6., 0.6]),
      ("idle", [1., 0.1, 0.]),
    ] {
      let raw = counters.iter().zip(values).map(|(x, w)| (x.clone(), (w * 1e9) as i64)).collect();
      phases.push(Phase {
        load: load.into(),
        samples: vec![Sample { seconds: 1., raw, ..Default::default() }],
      });
    }
    for id in ["scaled-cpu", "combined", "stuck"] {
      counters.push(id.into());
    }
    for phase in &mut phases {
      let row = &mut phase.samples[0];
      row.legacy_watts.clear();
      row.raw.insert("scaled-cpu".into(), row.raw["cpu"] * 2);
      row.raw.insert("combined".into(), row.raw["cpu"] + row.raw["gpu"] + row.raw["ane"]);
      row.raw.insert("stuck".into(), 42_000_000_000);
    }
    let mappings = classify(&counters, &phases, false);
    assert!(mappings.iter().all(|x| x.status == "no_active_reference"));
    assert_eq!(mappings[0].behavioral_ids, ["cpu", "scaled-cpu"]);
    assert_eq!(mappings[1].behavioral_ids, ["gpu"]);
    assert_eq!(mappings[2].behavioral_ids, ["ane"]);
    let out = summary(&mappings);
    assert!(out.contains("CPU: no active reference"));
    assert!(out.contains("Candidates (unverified)"));
    assert!(out.contains("scaled-cpu"));

    // Background CPU work during the first GPU test must not erase the quiet repeat.
    phases[2].samples[0].raw.insert("cpu".into(), 16_000_000_000);
    assert_eq!(classify(&counters, &phases, false)[0].behavioral_ids, ["cpu", "scaled-cpu"]);
    // A noisy pre-load idle must not discard the CPU channel if the post-load control is quiet.
    let cpu = phases.iter().rposition(|x| x.load == "cpu").unwrap();
    phases[cpu - 1].samples[0].raw.insert("cpu".into(), 14_000_000_000);
    assert_eq!(classify(&counters, &phases, false)[0].behavioral_ids, ["cpu", "scaled-cpu"]);

    // A one-off burst or missing recovery after a repeat must not pass.
    let gpu = phases.iter().rposition(|x| x.load == "gpu").unwrap();
    phases[gpu].samples[0].raw.insert("gpu".into(), 100_000_000);
    let ane = phases.iter().rposition(|x| x.load == "ane").unwrap();
    phases[ane + 1].samples[0].raw.insert("ane".into(), 800_000_000);
    let mappings = classify(&counters, &phases, false);
    assert!(mappings[1].behavioral_ids.is_empty());
    assert!(mappings[2].behavioral_ids.is_empty());
  }

  #[test]
  fn human_summary_shows_all_verified_components_and_handles_low_power_matches() {
    let (counters, mut phases) = measurements();
    // A valid match may fall just below the candidate activity threshold.
    phases[3].samples[0].raw.insert("ane".into(), 90_000_000);
    phases[3].samples[0].legacy_watts.insert("ane".into(), 0.11);
    let mappings = classify(&counters, &phases, false);
    assert!(mappings.iter().all(|x| x.status == "verified"));
    let out = summary(&mappings);
    for component in ["CPU", "GPU", "ANE"] {
      assert!(out.contains(&format!("{component}: verified")));
    }
  }

  #[test]
  fn reference_parser_requires_one_complete_finite_sample() {
    let sample = br#"<?xml version="1.0"?><plist version="1.0"><dict>
      <key>elapsed_ns</key><integer>1000000000</integer><key>processor</key><dict>
      <key>cpu_power</key><real>12000</real><key>gpu_power</key><real>100</real>
      <key>ane_power</key><real>800</real></dict></dict></plist>"#;
    let data: Vec<u8> = sample.iter().copied().chain([0]).collect();
    let parsed = parse_reference(&data).unwrap();
    assert_eq!(parsed["cpu"], 12.);
    assert!((parsed["ane"] - 0.8).abs() < 1e-9);
    // A pipe has delivered the document, but not the next sample's NUL separator.
    struct NotReady;
    impl Read for NotReady {
      fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
      }
    }
    let mut stream = BufReader::new(std::io::Cursor::new(sample).chain(NotReady));
    assert_eq!(read_reference(&mut stream).unwrap().unwrap()["cpu"], 12.);
    let frames = [data.clone(), data.clone()].concat();
    let mut stream = std::io::Cursor::new(frames);
    assert!(read_reference(&mut stream).unwrap().is_some());
    assert!(read_reference(&mut stream).unwrap().is_some());
    assert!(read_reference(&mut stream).unwrap().is_none());
    assert!(parse_reference(&[data.clone(), data.clone()].concat()).is_err());
    let invalid = String::from_utf8(data).unwrap().replace("12000", "nan");
    assert!(parse_reference(invalid.as_bytes()).is_err());
  }
}
