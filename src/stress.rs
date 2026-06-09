use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::ffi::{CString, c_char, c_void};
use std::hash::Hasher;
use std::hint::black_box;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Id = *mut c_void;
type Sel = *mut c_void;

#[derive(Clone, Copy, Debug)]
enum Mode {
  Cyclic,
  Full,
}

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

fn align_to_next_second() -> Instant {
  let now_wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
  let nanos = now_wall.subsec_nanos();
  let wait =
    if nanos == 0 { Duration::ZERO } else { Duration::from_nanos(1_000_000_000 - nanos as u64) };
  Instant::now() + wait
}

fn worker(mode: Mode, start_at: Instant, stop_at: Option<Instant>, seed: u64) {
  let period = Duration::from_secs(2);
  let busy_for = Duration::from_secs(1);
  let mut cycle_at = start_at;
  let mut state = seed;

  loop {
    if stop_at.is_some_and(|stop_at| Instant::now() >= stop_at) {
      break;
    }

    let now = Instant::now();
    if cycle_at > now {
      thread::sleep(cycle_at - now);
    }

    match mode {
      Mode::Cyclic => {
        let busy_until = cycle_at + busy_for;
        while Instant::now() < busy_until {
          state = cpu_work(state);
        }

        cycle_at += period;
        let now = Instant::now();
        if cycle_at > now {
          thread::sleep(cycle_at - now);
        }
      }
      Mode::Full => {
        while stop_at.is_none_or(|stop_at| Instant::now() < stop_at) {
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

fn spawn_cpu(
  workers: usize,
  mode: Mode,
  start_at: Instant,
  stop_at: Option<Instant>,
) -> Vec<thread::JoinHandle<()>> {
  let workers = workers.max(1);
  let mut threads = Vec::with_capacity(workers);

  for worker_id in 0..workers {
    let seed = worker_id as u64 + 1;
    threads.push(thread::spawn(move || worker(mode, start_at, stop_at, seed)));
  }

  threads
}

fn join_cpu(threads: Vec<thread::JoinHandle<()>>) {
  for thread in threads {
    let _ = thread.join();
  }
}

pub fn run_pattern(workers: usize, duration_sec: Option<u64>) {
  let start_at = align_to_next_second();
  let stop_at = duration_sec.map(|duration| start_at + Duration::from_secs(duration));
  join_cpu(spawn_cpu(workers, Mode::Cyclic, start_at, stop_at));
}

pub fn run_full(workers: usize, duration_sec: Option<u64>) -> Result<(), Box<dyn Error>> {
  let start_at = Instant::now();
  let stop_at = duration_sec.map(|duration| start_at + Duration::from_secs(duration));
  let threads = spawn_cpu(workers, Mode::Full, start_at, stop_at);

  println!("Full stress: cpu-workers={workers} gpu=on");

  let gpu_result = run_gpu(
    Mode::Full,
    FULL_GPU_WORK_ITEMS,
    FULL_GPU_ITERATIONS,
    FULL_GPU_INFLIGHT,
    start_at,
    stop_at,
  );

  join_cpu(threads);
  gpu_result
}

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

fn run_gpu(
  mode: Mode,
  work_items: usize,
  iterations: u32,
  inflight: usize,
  start_at: Instant,
  stop_at: Option<Instant>,
) -> Result<(), Box<dyn Error>> {
  let work_items = work_items.max(1);
  let iterations = iterations.max(1);
  let inflight = inflight.max(1);
  let buffer_length =
    work_items.checked_mul(16).ok_or("work-items value is too large to allocate the GPU buffer")?;

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
    let mut submitted = 0usize;
    let mut pending = Vec::with_capacity(inflight);
    let mut last_report = start_at;
    let mut cycle_at = start_at;

    loop {
      if stop_at.is_some_and(|stop_at| Instant::now() >= stop_at) {
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
        Mode::Full => stop_at.unwrap_or(Instant::now() + Duration::from_secs(60)),
      };

      loop {
        let now = Instant::now();
        if stop_at.is_some_and(|stop_at| now >= stop_at) {
          break;
        }
        if matches!(mode, Mode::Cyclic) && now >= busy_until {
          break;
        }

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

        pending.push(command_buffer);
        submitted += 1;

        if pending.len() >= inflight {
          let command_buffer = pending.remove(0);
          msg_void(command_buffer, "waitUntilCompleted");
        }

        let now = Instant::now();
        if now.duration_since(last_report) >= Duration::from_secs(1) {
          let elapsed = now.duration_since(start_at).as_secs_f64();
          println!(
            "elapsed={elapsed:.1}s dispatches={submitted} rate={:.1}/s",
            submitted as f64 / elapsed
          );
          last_report = now;
        }
      }

      if matches!(mode, Mode::Cyclic) {
        for command_buffer in pending.drain(..) {
          msg_void(command_buffer, "waitUntilCompleted");
        }

        cycle_at += Duration::from_secs(2);
        let now = Instant::now();
        if cycle_at > now {
          thread::sleep(cycle_at - now);
        }
      }
    }

    for command_buffer in pending {
      msg_void(command_buffer, "waitUntilCompleted");
    }
  }

  Ok(())
}
