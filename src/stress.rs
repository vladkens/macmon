use std::hint::spin_loop;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn align_to_next_second() -> Instant {
  let now_wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
  let nanos = now_wall.subsec_nanos();
  let wait =
    if nanos == 0 { Duration::ZERO } else { Duration::from_nanos(1_000_000_000 - nanos as u64) };
  Instant::now() + wait
}

fn worker(start_at: Instant, stop_at: Option<Instant>) {
  let period = Duration::from_secs(2);
  let busy_for = Duration::from_secs(1);
  let mut cycle_at = start_at;

  loop {
    if stop_at.is_some_and(|stop_at| Instant::now() >= stop_at) {
      break;
    }

    let now = Instant::now();
    if cycle_at > now {
      thread::sleep(cycle_at - now);
    }

    let busy_until = cycle_at + busy_for;
    while Instant::now() < busy_until {
      spin_loop();
    }

    cycle_at += period;
    let now = Instant::now();
    if cycle_at > now {
      thread::sleep(cycle_at - now);
    }
  }
}

pub fn run(workers: usize, duration_sec: Option<u64>) {
  let workers = workers.max(1);
  let start_at = align_to_next_second();
  let stop_at = duration_sec.map(|duration| start_at + Duration::from_secs(duration));
  let mut threads = Vec::with_capacity(workers);

  for _ in 0..workers {
    threads.push(thread::spawn(move || worker(start_at, stop_at)));
  }

  for thread in threads {
    let _ = thread.join();
  }
}
