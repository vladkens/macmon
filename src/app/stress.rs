//! CPU, GPU, and ANE stress-test workloads.

use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::ffi::{CStr, CString, c_char, c_void};
use std::hash::Hasher;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{ptr, thread};

use core_foundation::attributed_string::CFMutableAttributedString;
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

type Id = *mut c_void;
type Sel = *mut c_void;

// MARK: Workload control

#[derive(Clone, Copy, Debug)]
enum Mode {
  Cyclic,
  Full,
}

#[derive(Clone)]
struct WorkloadControl {
  start_at: Instant,
  stop_at: Option<Instant>,
  pulse: Option<Duration>,
  cancelled: Arc<AtomicBool>,
}

impl WorkloadControl {
  fn new(start_at: Instant, duration_sec: Option<u64>) -> Self {
    Self {
      start_at,
      stop_at: duration_sec.map(|seconds| start_at + Duration::from_secs(seconds)),
      pulse: None,
      cancelled: Arc::new(AtomicBool::new(false)),
    }
  }

  fn running(&self) -> bool {
    !self.cancelled.load(Ordering::Relaxed)
      && self.stop_at.is_none_or(|deadline| Instant::now() < deadline)
  }

  fn cancel(&self) {
    self.cancelled.store(true, Ordering::Relaxed);
  }

  fn idle_for(&self, now: Instant) -> Duration {
    let Some(elapsed) = now.checked_duration_since(self.start_at) else {
      return self.start_at.duration_since(now);
    };
    let Some(phase) = self.pulse else { return Duration::ZERO };
    let phase_ns = phase.as_nanos();
    let offset = elapsed.as_nanos() % (2 * phase_ns);
    if offset < phase_ns {
      Duration::ZERO
    } else {
      let remaining = 2 * phase_ns - offset;
      Duration::new((remaining / 1_000_000_000) as u64, (remaining % 1_000_000_000) as u32)
    }
  }

  fn wait_for_active(&self) -> bool {
    while self.running() {
      let now = Instant::now();
      let idle = self.idle_for(now);
      if idle.is_zero() {
        return true;
      }
      let remaining = self.stop_at.map(|end| end.saturating_duration_since(now)).unwrap_or(idle);
      thread::sleep(idle.min(remaining).min(Duration::from_millis(50)));
    }
    false
  }
}

// Stop sibling workloads on errors and unwinding, including an unbounded run.
struct CancelOnDrop<'a>(&'a WorkloadControl);

impl Drop for CancelOnDrop<'_> {
  fn drop(&mut self) {
    self.0.cancel();
  }
}

// MARK: Objective-C and Metal bindings

#[repr(C)]
#[derive(Clone, Copy)]
struct MtlSize {
  width: usize,
  height: usize,
  depth: usize,
}

#[link(name = "objc")]
unsafe extern "C" {
  fn objc_getClass(name: *const c_char) -> Id;
  fn objc_msgSend();
  fn sel_registerName(name: *const c_char) -> Sel;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
  fn MTLCreateSystemDefaultDevice() -> Id;
}

const NS_UTF8_STRING_ENCODING: u64 = 4;
const MTL_RESOURCE_STORAGE_MODE_PRIVATE: u64 = 2 << 4;
const FULL_GPU_WORK_ITEMS: usize = 1_048_576;
const FULL_GPU_ITERATIONS: u32 = 4096;
const FULL_GPU_INFLIGHT: usize = 3;
// Vision OCR also schedules auxiliary GPU work. Short, single in-flight batches
// let it progress alongside the GPU stress kernel and keep pulse tails bounded.
const SHARED_GPU_ITERATIONS: u32 = 256;

const GPU_SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void stress_kernel(device float4 *out [[buffer(0)]],
                          constant uint &iterations [[buffer(1)]],
                          uint id [[thread_position_in_grid]]) {
  float4 x = float4(float(id & 1023u) * 0.001f + 1.0f,
                    float((id >> 10u) & 1023u) * 0.001f + 2.0f,
                    float((id >> 20u) & 1023u) * 0.001f + 3.0f,
                    4.0f);

  for (uint i = 0; i < iterations; i++) {
    x = sin(x) * cos(x + 0.37f) + sqrt(abs(x) + 1.0f);
  }

  out[id] = x;
}
"#;

// MARK: CPU workload

fn align_to_next_second() -> Instant {
  let now_wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
  let nanos = now_wall.subsec_nanos();
  let wait =
    if nanos == 0 { Duration::ZERO } else { Duration::from_nanos(1_000_000_000 - nanos as u64) };
  Instant::now() + wait
}

fn worker(mode: Mode, control: WorkloadControl, seed: u64) {
  let period = Duration::from_secs(2);
  let busy_for = Duration::from_secs(1);
  let mut cycle_at = control.start_at;
  let mut state = seed;

  loop {
    if !control.running() {
      break;
    }

    let now = Instant::now();
    if cycle_at > now {
      thread::sleep(cycle_at - now);
    }

    match mode {
      Mode::Cyclic => {
        let busy_until = cycle_at + busy_for;
        while control.running() && Instant::now() < busy_until {
          state = cpu_work(state);
        }

        cycle_at += period;
        let now = Instant::now();
        if cycle_at > now {
          thread::sleep(cycle_at - now);
        }
      }
      Mode::Full => {
        while control.wait_for_active() {
          state = cpu_work(state);
        }
      }
    }
  }

  black_box(state);
}

#[inline(never)]
fn cpu_work(mut state: u64) -> u64 {
  for _ in 0..256 {
    let mut hasher = DefaultHasher::new();
    hasher.write_u64(black_box(state));
    hasher.write_u64(state.rotate_left(17));
    hasher.write_u64(state.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    hasher.write_u64(state ^ 0xbf58_476d_1ce4_e5b9);
    state = hasher.finish();
  }

  black_box(state)
}

fn spawn_cpu(workers: usize, mode: Mode, control: &WorkloadControl) -> Vec<thread::JoinHandle<()>> {
  let workers = workers.max(1);
  let mut threads = Vec::with_capacity(workers);

  for worker_id in 0..workers {
    let seed = worker_id as u64 + 1;
    let control = control.clone();
    threads.push(thread::spawn(move || worker(mode, control, seed)));
  }

  threads
}

fn join_cpu(threads: Vec<thread::JoinHandle<()>>) {
  for thread in threads {
    let _ = thread.join();
  }
}

// MARK: Workload runners

pub fn run_pattern(workers: usize, duration_sec: Option<u64>) {
  let control = WorkloadControl::new(align_to_next_second(), duration_sec);
  join_cpu(spawn_cpu(workers, Mode::Cyclic, &control));
}

pub fn run_cpu(workers: usize, duration_sec: Option<u64>) {
  let control = WorkloadControl::new(Instant::now(), duration_sec);
  join_cpu(spawn_cpu(workers, Mode::Full, &control));
}

pub fn run_gpu(duration_sec: Option<u64>) -> Result<(), Box<dyn Error>> {
  let control = WorkloadControl::new(Instant::now(), duration_sec);

  run_gpu_workload(
    Mode::Full,
    FULL_GPU_WORK_ITEMS,
    FULL_GPU_ITERATIONS,
    FULL_GPU_INFLIGHT,
    &control,
  )
}

pub fn run_all(
  workers: usize,
  duration_sec: Option<u64>,
  pulse_sec: Option<u64>,
  ane: &mut AneLoad,
) -> Result<(), Box<dyn Error>> {
  let mut control = WorkloadControl::new(Instant::now(), duration_sec);
  control.pulse = pulse_sec.map(Duration::from_secs);
  run_combined(
    workers,
    control,
    |control| {
      run_gpu_workload(Mode::Full, FULL_GPU_WORK_ITEMS, SHARED_GPU_ITERATIONS, 1, control)
        .map_err(|error| error.to_string())
    },
    |control| ane.run_while(|| control.wait_for_active()),
  )
}

fn run_combined(
  workers: usize,
  control: WorkloadControl,
  gpu: impl FnOnce(&WorkloadControl) -> Result<(), String> + Send,
  ane: impl FnOnce(&WorkloadControl) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
  let cpu = spawn_cpu(workers, Mode::Full, &control);
  let result = thread::scope(|scope| {
    let gpu = scope.spawn(|| {
      let _cancel = CancelOnDrop(&control);
      gpu(&control)
    });
    // Vision objects remain on the thread where they were prepared; no unsafe Send.
    let ane_result = {
      let _cancel = CancelOnDrop(&control);
      ane(&control)
    };
    let gpu_result = gpu.join().map_err(|_| "GPU workload thread panicked")?;
    ane_result?;
    gpu_result.map_err(Into::into)
  });
  join_cpu(cpu);
  result
}

// MARK: Objective-C helpers

fn cstr(value: &str) -> CString {
  CString::new(value).expect("static strings must not contain null bytes")
}

fn sel(name: &str) -> Sel {
  let name = cstr(name);
  unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn msg_id(receiver: Id, selector: &str) -> Id {
  let send: extern "C" fn(Id, Sel) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector))
}

unsafe fn msg_id_id(receiver: Id, selector: &str, arg: Id) -> Id {
  let send: extern "C" fn(Id, Sel, Id) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), arg)
}

unsafe fn msg_id_id_id_error(receiver: Id, selector: &str, a: Id, b: Id, error: *mut Id) -> Id {
  let send: extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), a, b, error)
}

unsafe fn msg_id_id_error(receiver: Id, selector: &str, arg: Id, error: *mut Id) -> Id {
  let send: extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), arg, error)
}

unsafe fn msg_id_usize_u64(receiver: Id, selector: &str, length: usize, options: u64) -> Id {
  let send: extern "C" fn(Id, Sel, usize, u64) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), length, options)
}

unsafe fn msg_usize(receiver: Id, selector: &str) -> usize {
  let send: extern "C" fn(Id, Sel) -> usize =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector))
}

unsafe fn msg_void(receiver: Id, selector: &str) {
  let send: extern "C" fn(Id, Sel) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector));
}

unsafe fn msg_void_id(receiver: Id, selector: &str, arg: Id) {
  let send: extern "C" fn(Id, Sel, Id) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), arg);
}

unsafe fn msg_void_id_usize_usize(receiver: Id, selector: &str, a: Id, b: usize, c: usize) {
  let send: extern "C" fn(Id, Sel, Id, usize, usize) =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), a, b, c);
}

unsafe fn msg_void_ptr_usize_usize(
  receiver: Id,
  selector: &str,
  a: *const c_void,
  b: usize,
  c: usize,
) {
  let send: extern "C" fn(Id, Sel, *const c_void, usize, usize) =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), a, b, c);
}

unsafe fn msg_void_size_size(receiver: Id, selector: &str, a: MtlSize, b: MtlSize) {
  let send: extern "C" fn(Id, Sel, MtlSize, MtlSize) =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), a, b);
}

unsafe fn ns_string(value: &str) -> Id {
  let class_name = cstr("NSString");
  let class = unsafe { objc_getClass(class_name.as_ptr()) };
  let allocated = unsafe { msg_id(class, "alloc") };
  let bytes = value.as_ptr().cast::<c_void>();
  let send: extern "C" fn(Id, Sel, *const c_void, usize, u64) -> Id =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(
    allocated,
    sel("initWithBytes:length:encoding:"),
    bytes,
    value.len(),
    NS_UTF8_STRING_ENCODING,
  )
}

fn require_id(value: Id, message: &str) -> Result<Id, Box<dyn Error>> {
  if value.is_null() { Err(message.to_string().into()) } else { Ok(value) }
}

// MARK: GPU workload

// GPU work can run on a worker thread with no implicit Cocoa autorelease pool.
struct AutoreleasePool(Id);

impl AutoreleasePool {
  fn new() -> Result<Self, Box<dyn Error>> {
    let pool = unsafe { msg_id(objc_getClass(cstr("NSAutoreleasePool").as_ptr()), "new") };
    Ok(Self(require_id(pool, "failed to create GPU autorelease pool")?))
  }
}

impl Drop for AutoreleasePool {
  fn drop(&mut self) {
    unsafe { msg_void(self.0, "drain") };
  }
}

struct PendingCommand(Id);

impl PendingCommand {
  fn retain(command: Id) -> Self {
    unsafe { msg_void(command, "retain") };
    Self(command)
  }

  fn wait(&self) {
    unsafe { msg_void(self.0, "waitUntilCompleted") };
  }
}

impl Drop for PendingCommand {
  fn drop(&mut self) {
    unsafe { msg_void(self.0, "release") };
  }
}

fn run_gpu_workload(
  mode: Mode,
  work_items: usize,
  iterations: u32,
  inflight: usize,
  control: &WorkloadControl,
) -> Result<(), Box<dyn Error>> {
  let work_items = work_items.max(1);
  let iterations = iterations.max(1);
  let inflight = inflight.max(1);
  let buffer_length =
    work_items.checked_mul(16).ok_or("work-items value is too large to allocate the GPU buffer")?;
  let _pool = AutoreleasePool::new()?;

  unsafe {
    let device =
      require_id(MTLCreateSystemDefaultDevice(), "Metal is not available on this system")?;
    let command_queue =
      require_id(msg_id(device, "newCommandQueue"), "failed to create Metal command queue")?;

    let source = ns_string(GPU_SHADER);
    let mut error = ptr::null_mut();
    let library = require_id(
      msg_id_id_id_error(
        device,
        "newLibraryWithSource:options:error:",
        source,
        ptr::null_mut(),
        &mut error,
      ),
      "failed to compile Metal shader",
    )?;
    msg_void(source, "release");

    let function_name = ns_string("stress_kernel");
    let function = require_id(
      msg_id_id(library, "newFunctionWithName:", function_name),
      "failed to find Metal stress kernel",
    )?;
    msg_void(function_name, "release");

    let mut error = ptr::null_mut();
    let pipeline = require_id(
      msg_id_id_error(device, "newComputePipelineStateWithFunction:error:", function, &mut error),
      "failed to create Metal compute pipeline",
    )?;
    let buffer = require_id(
      msg_id_usize_u64(
        device,
        "newBufferWithLength:options:",
        buffer_length,
        MTL_RESOURCE_STORAGE_MODE_PRIVATE,
      ),
      "failed to allocate Metal buffer",
    )?;

    let max_threads = msg_usize(pipeline, "maxTotalThreadsPerThreadgroup").clamp(1, 256);
    let threads_per_group = MtlSize { width: max_threads, height: 1, depth: 1 };
    let threads_per_grid = MtlSize { width: work_items, height: 1, depth: 1 };
    let mut pending: Vec<PendingCommand> = Vec::with_capacity(inflight);
    let mut cycle_at = control.start_at;

    loop {
      if !control.running() {
        break;
      }

      let busy_until = match mode {
        Mode::Cyclic => {
          let now = Instant::now();
          if cycle_at > now {
            thread::sleep(cycle_at - now);
          }
          cycle_at + Duration::from_secs(1)
        }
        Mode::Full => control.stop_at.unwrap_or(Instant::now() + Duration::from_secs(60)),
      };

      loop {
        let now = Instant::now();
        if !control.running() {
          break;
        }
        if !control.idle_for(now).is_zero() {
          // Finish submitted commands before the shared idle phase; do not queue
          // more GPU work while CPU and ANE have stopped submitting theirs.
          for command in pending.drain(..) {
            command.wait();
          }
          if !control.wait_for_active() {
            break;
          }
          continue;
        }
        if matches!(mode, Mode::Cyclic) && now >= busy_until {
          break;
        }

        let _batch_pool = AutoreleasePool::new()?;
        let command_buffer = require_id(
          msg_id(command_queue, "commandBuffer"),
          "failed to create Metal command buffer",
        );
        let command_buffer = command_buffer?;
        let encoder = require_id(
          msg_id(command_buffer, "computeCommandEncoder"),
          "failed to create Metal compute encoder",
        )?;

        msg_void_id(encoder, "setComputePipelineState:", pipeline);
        msg_void_id_usize_usize(encoder, "setBuffer:offset:atIndex:", buffer, 0, 0);
        msg_void_ptr_usize_usize(
          encoder,
          "setBytes:length:atIndex:",
          (&iterations as *const u32).cast::<c_void>(),
          std::mem::size_of::<u32>(),
          1,
        );
        msg_void_size_size(
          encoder,
          "dispatchThreads:threadsPerThreadgroup:",
          threads_per_grid,
          threads_per_group,
        );
        msg_void(encoder, "endEncoding");
        msg_void(command_buffer, "commit");

        // Keep in-flight commands alive after their per-submission pool drains.
        pending.push(PendingCommand::retain(command_buffer));

        if pending.len() >= inflight {
          let command_buffer = pending.remove(0);
          command_buffer.wait();
        }
      }

      if matches!(mode, Mode::Cyclic) {
        for command_buffer in pending.drain(..) {
          command_buffer.wait();
        }

        cycle_at += Duration::from_secs(2);
        let now = Instant::now();
        if cycle_at > now {
          thread::sleep(cycle_at - now);
        }
      }
    }

    for command_buffer in pending {
      command_buffer.wait();
    }
  }

  Ok(())
}

// MARK: Vision, CoreML, and graphics bindings

#[repr(C)]
struct Rect {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreML", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
  fn CGColorSpaceCreateDeviceRGB() -> Id;
  fn CGColorSpaceRelease(space: Id);
  fn CGBitmapContextCreate(
    data: Id,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bytes_per_row: usize,
    space: Id,
    bitmap_info: u32,
  ) -> Id;
  fn CGContextRelease(context: Id);
  fn CGContextSetRGBFillColor(context: Id, red: f64, green: f64, blue: f64, alpha: f64);
  fn CGContextFillRect(context: Id, rect: Rect);
  fn CGContextSetTextPosition(context: Id, x: f64, y: f64);
  fn CGBitmapContextCreateImage(context: Id) -> Id;
  fn CGImageRelease(image: Id);
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
  static kCTFontAttributeName: CFStringRef;
  fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const c_void) -> CFTypeRef;
  fn CTLineCreateWithAttributedString(string: CFTypeRef) -> CFTypeRef;
  fn CTLineDraw(line: CFTypeRef, context: Id);
}

// MARK: ANE workload

// Diagnostic ANE activity via Vision OCR, not a guarantee of full ANE utilization.
// Keep Objective-C ownership explicit, including on NSError and early-return paths.
struct Object(Id);

impl Object {
  fn from_owned(value: Id) -> Result<Self, Box<dyn Error>> {
    if value.is_null() {
      return Err("failed to create a Vision OCR object".into());
    }

    Ok(Self(value))
  }

  fn new(class: &str) -> Result<Self, Box<dyn Error>> {
    unsafe {
      let class = objc_getClass(cstr(class).as_ptr());
      Self::from_owned(msg_id(class, "new"))
    }
  }

  fn string(value: &str) -> Result<Self, Box<dyn Error>> {
    Self::from_owned(unsafe { ns_string(value) })
  }

  fn array(values: &[Id]) -> Result<Self, Box<dyn Error>> {
    unsafe {
      let class = objc_getClass(cstr("NSArray").as_ptr());
      let send: extern "C" fn(Id, Sel, *const Id, usize) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
      Self::from_owned(send(
        msg_id(class, "alloc"),
        sel("initWithObjects:count:"),
        values.as_ptr(),
        values.len(),
      ))
    }
  }
}

impl Drop for Object {
  fn drop(&mut self) {
    unsafe { msg_void(self.0, "release") };
  }
}

struct Context(Id);

impl Drop for Context {
  fn drop(&mut self) {
    unsafe { CGContextRelease(self.0) };
  }
}

struct Image(Id);

impl Drop for Image {
  fn drop(&mut self) {
    unsafe { CGImageRelease(self.0) };
  }
}

fn cf_owned(value: CFTypeRef) -> Result<CFType, Box<dyn Error>> {
  if value.is_null() {
    return Err("failed to create OCR text graphics".into());
  }

  Ok(unsafe { CFType::wrap_under_create_rule(value) })
}

unsafe fn bool_message(receiver: Id, selector: &str, arg: Id) -> bool {
  let send: extern "C" fn(Id, Sel, Id) -> i8 =
    unsafe { std::mem::transmute(objc_msgSend as *const ()) };
  send(receiver, sel(selector), arg) != 0
}

fn vision_error(context: &str, error: Id) -> Box<dyn Error> {
  unsafe {
    if !error.is_null() {
      let description = msg_id(error, "localizedDescription");
      let send: extern "C" fn(Id, Sel) -> *const c_char =
        std::mem::transmute(objc_msgSend as *const ());
      let text = send(description, sel("UTF8String"));
      if !text.is_null() {
        return format!("{context}: {}", CStr::from_ptr(text).to_string_lossy()).into();
      }
    }
  }

  context.to_string().into()
}

pub(crate) struct AneLoad {
  request: Object,
  requests: Object,
  options: Object,
  font: CFType,
  completed: u64,
}

impl AneLoad {
  pub(crate) fn prepare() -> Result<Self, Box<dyn Error>> {
    eprintln!("ANE OCR · preparing Vision and warming up; Ctrl-C to stop");
    let _pool = Object::new("NSAutoreleasePool")?;
    let request = Object::new("VNRecognizeTextRequest")?;
    let stage = Object::string("VNComputeStageMain")?;
    unsafe {
      let ane_class = objc_getClass(cstr("MLNeuralEngineComputeDevice").as_ptr());
      if ane_class.is_null()
        || !bool_message(
          request.0,
          "respondsToSelector:",
          sel("supportedComputeStageDevicesAndReturnError:"),
        )
      {
        return Err("ANE OCR requires macOS 14 or later with Neural Engine support".into());
      }

      // VNRequestTextRecognitionLevelAccurate = 0. Do not opt into background processing.
      let set_level: extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc_msgSend as *const ());
      set_level(request.0, sel("setRecognitionLevel:"), 0);
      let set_bool: extern "C" fn(Id, Sel, i8) = std::mem::transmute(objc_msgSend as *const ());
      set_bool(request.0, sel("setUsesLanguageCorrection:"), 0);
      let language = Object::string("en-US")?;
      let languages = Object::array(&[language.0])?;
      msg_void_id(request.0, "setRecognitionLanguages:", languages.0);

      let mut error = ptr::null_mut();
      let get_devices: extern "C" fn(Id, Sel, *mut Id) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
      let stages =
        get_devices(request.0, sel("supportedComputeStageDevicesAndReturnError:"), &mut error);
      if stages.is_null() {
        return Err(vision_error("failed to enumerate OCR compute devices", error));
      }

      let devices = msg_id_id(stages, "objectForKey:", stage.0);
      let get_at: extern "C" fn(Id, Sel, usize) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
      let device = (0..msg_usize(devices, "count"))
        .map(|i| get_at(devices, sel("objectAtIndex:"), i))
        .find(|&device| bool_message(device, "isKindOfClass:", ane_class))
        .ok_or("Vision OCR does not support ANE for its main stage on this system")?;
      let assign: extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc_msgSend as *const ());
      assign(request.0, sel("setComputeDevice:forComputeStage:"), device, stage.0);
    }

    let requests = Object::array(&[request.0])?;
    let font_name = CFString::new("Helvetica");
    let font = cf_owned(unsafe {
      CTFontCreateWithName(font_name.as_concrete_TypeRef(), 32.0, ptr::null())
    })?;
    let mut load =
      Self { request, requests, options: Object::new("NSDictionary")?, font, completed: 0 };
    let lines = load.recognize(0)?;
    eprintln!("ANE OCR · main stage assigned to ANE; warmup recognized {lines} text regions");
    Ok(load)
  }

  fn text_image(&self, iteration: u64) -> Result<Image, Box<dyn Error>> {
    unsafe {
      let space = CGColorSpaceCreateDeviceRGB();
      if space.is_null() {
        return Err("failed to create OCR color space".into());
      }

      // kCGImageAlphaNoneSkipLast = 5; CoreGraphics owns the pixel allocation.
      let context = CGBitmapContextCreate(ptr::null_mut(), 1600, 1200, 8, 0, space, 5);
      CGColorSpaceRelease(space);
      if context.is_null() {
        return Err("failed to create OCR image context".into());
      }

      let context = Context(context);
      CGContextSetRGBFillColor(context.0, 1.0, 1.0, 1.0, 1.0);
      CGContextFillRect(context.0, Rect { x: 0.0, y: 0.0, width: 1600.0, height: 1200.0 });
      CGContextSetRGBFillColor(context.0, 0.0, 0.0, 0.0, 1.0);
      for row in 0..20 {
        let text = CFString::new(&format!(
          "Neural Engine OCR test. Page {iteration}, line {row}. Read this text 1234567890."
        ));
        let mut attributed = CFMutableAttributedString::new();
        attributed.replace_str(&text, CFRange::init(0, 0));
        attributed.set_attribute(
          CFRange::init(0, attributed.char_len()),
          kCTFontAttributeName,
          &self.font,
        );
        let line = cf_owned(CTLineCreateWithAttributedString(attributed.as_CFTypeRef()))?;
        CGContextSetTextPosition(context.0, 40.0, 1130.0 - row as f64 * 54.0);
        CTLineDraw(line.as_CFTypeRef(), context.0);
      }

      let image = CGBitmapContextCreateImage(context.0);
      if image.is_null() {
        return Err("failed to create OCR image".into());
      }

      Ok(Image(image))
    }
  }

  fn recognize(&mut self, iteration: u64) -> Result<usize, Box<dyn Error>> {
    let _pool = Object::new("NSAutoreleasePool")?;
    let image = self.text_image(iteration)?;
    unsafe {
      let class = objc_getClass(cstr("VNImageRequestHandler").as_ptr());
      let init: extern "C" fn(Id, Sel, Id, Id) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
      let handler = Object::from_owned(init(
        msg_id(class, "alloc"),
        sel("initWithCGImage:options:"),
        image.0,
        self.options.0,
      ))?;
      let mut error = ptr::null_mut();
      let perform: extern "C" fn(Id, Sel, Id, *mut Id) -> i8 =
        std::mem::transmute(objc_msgSend as *const ());
      if perform(handler.0, sel("performRequests:error:"), self.requests.0, &mut error) == 0 {
        return Err(vision_error("ANE OCR request failed", error));
      }

      let count = msg_usize(msg_id(self.request.0, "results"), "count");
      if count == 0 {
        return Err("ANE OCR returned no text observations".into());
      }

      Ok(count)
    }
  }

  pub(crate) fn run(&mut self, duration_sec: Option<u64>) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();
    let duration = duration_sec.map(Duration::from_secs);
    self.run_while(|| duration.is_none_or(|limit| start.elapsed() < limit))
  }

  fn run_while(&mut self, mut running: impl FnMut() -> bool) -> Result<(), Box<dyn Error>> {
    while running() {
      self.recognize(self.completed + 1)?;
      self.completed += 1;
    }

    Ok(())
  }

  pub(crate) fn completed_requests(&self) -> u64 {
    self.completed
  }
}

// MARK: Tests

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pulse_phases_share_an_epoch_and_preserve_full_load_without_the_flag() {
    let start = Instant::now();
    let mut control = WorkloadControl::new(start, None);
    assert_eq!(control.idle_for(start + Duration::from_secs(100)), Duration::ZERO);
    for seconds in [1, 2, 3] {
      let phase = Duration::from_secs(seconds);
      control.pulse = Some(phase);
      assert_eq!(control.idle_for(start), Duration::ZERO);
      assert_eq!(control.idle_for(start + phase - Duration::from_nanos(1)), Duration::ZERO);
      assert_eq!(control.idle_for(start + phase), phase);
      assert_eq!(control.idle_for(start + phase * 2), Duration::ZERO);
      assert_eq!(control.idle_for(start + phase * 3), phase);
    }
    assert_eq!(control.idle_for(start - Duration::from_secs(1)), Duration::from_secs(1));
  }

  #[test]
  fn pulse_wait_obeys_deadline_and_cancellation_during_idle() {
    let start = Instant::now() - Duration::from_secs(2);
    let mut control = WorkloadControl::new(start, None);
    control.pulse = Some(Duration::from_secs(2));
    control.cancel();
    assert!(!control.wait_for_active());

    let mut control = WorkloadControl::new(start, Some(2));
    control.pulse = Some(Duration::from_secs(2));
    assert!(!control.wait_for_active());
  }

  #[test]
  fn combined_ane_error_stops_gpu_and_cpu() {
    let control = WorkloadControl::new(Instant::now(), Some(2));
    let observed = control.clone();
    let result = run_combined(
      1,
      control,
      |control| {
        while control.running() {
          thread::yield_now();
        }
        assert!(control.cancelled.load(Ordering::Relaxed));
        Ok(())
      },
      |_| Err("test ANE failure".into()),
    );
    assert_eq!(result.unwrap_err().to_string(), "test ANE failure");
    assert!(!observed.running());
  }

  #[test]
  fn combined_gpu_error_stops_ane_and_cpu() {
    let control = WorkloadControl::new(Instant::now(), Some(2));
    let result = run_combined(
      1,
      control,
      |_| Err("test GPU failure".into()),
      |control| {
        while control.running() {
          thread::yield_now();
        }
        assert!(control.cancelled.load(Ordering::Relaxed));
        Ok(())
      },
    );
    assert_eq!(result.unwrap_err().to_string(), "test GPU failure");
  }

  #[test]
  fn combined_workloads_run_concurrently_with_the_same_deadline() {
    let control = WorkloadControl::new(Instant::now(), Some(1));
    let deadline = control.stop_at;
    let ready = std::sync::Barrier::new(2);
    run_combined(
      1,
      control,
      |control| {
        assert_eq!(control.stop_at, deadline);
        ready.wait();
        while control.running() {
          thread::yield_now();
        }
        Ok(())
      },
      |control| {
        assert_eq!(control.stop_at, deadline);
        ready.wait();
        Ok(())
      },
    )
    .unwrap();
  }
}
