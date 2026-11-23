Done: 2026-11-23, PR #150

# GPU power in the POWER column

The POWER column of the process list is CPU energy only (`ri_energy_nj`), so the process that keeps the GPU busy can read ~0 W (#146). The kernel bills GPU work to processes only as time, never as energy. macmon shares the GPU power it already samples (`gpu_power`) between processes by the GPU time it already reads per process, and POWER shows CPU power plus that share.

## Behavior

- Each process sample gets the average `gpu_power` over its interval. A process' share is `gpu_power × Δgpu_ns(p) / Σ Δgpu_ns(all processes)`, with the raw GPU time deltas (not the capped GPU%), so the shares add up to `gpu_power` even when several clients overlap, and to 0 when no process used the GPU.
- Every process with GPU time gets a share, other users' processes included (WindowServer). Processes that start or exit inside the interval have no delta, and their energy goes to the others; so does that of a new process of another user until the next `ps` run (up to `PS_PERIOD`).
- A process' GPU time grows by what each of its clients gained since the last read, tracked by registry entry ID: a closed client doesn't take its time back, and a client new to the read (or missed by the last walk) starts from its current time instead of bringing its whole time at once. GPU% uses the same time.
- POWER = CPU power + GPU share, for processes with an energy counter (own processes, macOS 13+). Others keep `-`: a GPU share alone would look like the whole power. Sorting, group totals, the gradient and the `power:` filter (TUI and `serve --processes`) use the sum.
- The TUI samples metrics every tick and processes at least `PROCS_MIN_GAP` apart: the GPU energy of the metrics since the last process sample, weighed by their real windows, gives its average power. Showing the panel ends the metrics window there (unless it is shorter than `PROCS_MIN_GAP`), so the first list's power covers its own window; the charts get one more point then. `serve` samples both at the same interval and passes `gpu_power` as is.
- Prometheus: `macmon_process_power_watts` stays CPU power (its HELP already says so), a new `macmon_process_gpu_power_watts` carries the GPU share for every exported process. In a VM no process power is exported, as the chip power gauges are left out there.
- The help and the readme say that POWER is CPU power plus an estimated GPU share, and the readme which Prometheus metric is which part.

Compatibility: the TUI column and the `power:` filter now include the GPU share; Prometheus gets a new metric and drops process power in a VM, where it read 0; JSON, the library API and the config don't change.

## Decisions

- A sum in POWER instead of a `GPU W` column: a separate column sorts nearly as GPU% does and costs 7 cells, while the sum is what ranks a GPU-bound process against a CPU-bound one.
- `ProcInfo` keeps the two parts apart (`cpu_power_w`, `gpu_power_w`) and sums them in `power_w()`, so Prometheus exports them unmixed and PromQL can add them.
- No dim or split rendering in the cell: the help explains the sum, and GPU% next to it shows the GPU side.
- ANE, media engines and DRAM stay unbilled: there is no per-process time for them.

## Checklist

- [x] `ProcSampler::sample(gpu_w)`: GPU time deltas, the share per process, `cpu_power_w` / `gpu_power_w` / `power_w()`.
- [x] TUI: average GPU power between process samples; POWER, sorting, groups and the `power:` filter use the sum; help line.
- [x] `serve`: `gpu_power` to the sampler, `macmon_process_gpu_power_watts`, no process power in a VM.
- [x] Readme: POWER in the interactive mode section, the metrics of `--processes`.
- [x] Tests: shares with overlapping and zero GPU time, a process without clients, a reused pid, client time per entry ID; the sum in POWER, its sort, group totals and the `power:` filter; Prometheus output.
- [x] `make check`, `make test`, `make loc REF=origin/main`.
- [x] By hand on M5 / macOS 27.2: the TUI under `macmon stress all` sorted by POWER, `serve --processes "gpu:>0"` with `curl /metrics`.

## Result

POWER is CPU power plus the GPU share, and `serve --processes` exports the share as `macmon_process_gpu_power_watts`. On M5 / macOS 27.2.1 under `macmon stress all` the stress process read 80 % GPU and 16.6 W in the TUI (11.8 W GPU, 7.5 W CPU); in `serve` the shares of the stress process, WindowServer and Rex added up to `gpu_power` (15.81 against 15.80 W). Under `macmon stress gpu`, which submits long command buffers, the IORegistry gives the stress process 7–9 % of GPU time and WindowServer 88 % (5 % at idle), so most of the GPU power goes to WindowServer, which shows `-`: GPU% shows the same split. Untested: other chips and macOS versions, games and ML workloads.
