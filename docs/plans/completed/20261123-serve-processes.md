Done: 2026-11-23, PR #149

# Processes in `serve`

`macmon serve --processes "<query>"` exports the processes that match a TUI filter query to `/metrics`, as proposed in #133: `--processes "gpu:>0"` for whoever uses the GPU, `--processes "app:llama-server"` for one app. Without the option nothing per-process is exported, as today.

## Behavior

- `--processes <QUERY>`: the filter syntax of the TUI (`/`), parsed with `Query`. An empty query matches every process.
- With the option, the sampling loop of `serve` also samples processes with `ProcSampler`, at the same interval, and keeps the rows that match. Without it, no process is sampled.
- `/metrics` gets one series per matching process, labels `chip`, `pid` and `name` (escaped like every label):
  - `macmon_process_cpu_percent`: CPU, 100 is one busy core, as the CPU% column.
  - `macmon_process_cpu_seconds_total` (counter): user + system CPU time since the process started, as the TIME column.
  - `macmon_process_memory_bytes`: physical footprint of own processes, resident size of others', as the MEM column.
  - `macmon_process_gpu_percent`: share of the interval the GPU spent on the process, as the GPU% column.
  - `macmon_process_power_watts`: only for processes with an energy counter (own processes, macOS 13+), as the POWER column.
- The values are those of the last sample interval, as every other macmon gauge. A process that exits drops out of the next output.
- `--install` and `--install --agent` write `--processes <QUERY>` into the plist, so the service exports the same processes.
- `/json` doesn't change: processes in JSON are the roadmap item "Processes in `pipe` and JSON".
- The help of the option and the readme say that process names and pids go out on the port, which has no authentication, and that the query decides how many series there are.

Compatibility: a new option and new metrics only; existing Prometheus and JSON output, the library API and the config don't change.

## Decisions

- Gauges of the last interval, as the TUI columns, instead of counters of GPU time and energy. The process sampler already computes these values; a monotonic GPU time counter needs per-client bookkeeping (GPU clients close and the per-process sum drops), which the branch of #133 did in ~180 lines. CPU time is a counter for free. A GPU time counter can follow if someone needs exact averages over long scrape intervals.
- Labels `pid` and `name` only: the query already picks the processes, and `user` or `path` would only add text to every series.
- Processes are sampled in the existing loop, right after the metrics: no second thread and no second timer.
- `SharedMetrics` holds the metrics and the matching processes together, so one scrape never mixes two samples.

## Checklist

- [x] `--processes` option, process sampling in the `serve` loop, metrics in `/metrics`.
- [x] `--processes` in the plist of `--install` and `--install --agent`.
- [x] Readme: the option, an example, the note about the open port and the series count.
- [x] Tests: Prometheus output of a process with and without power, label escaping, the plist with and without the option, the CLI.
- [x] `make check`, `make test`, `make loc REF=origin/main`.
- [x] By hand on M5 / macOS 27.2: `macmon serve --processes "gpu:>0"` and `"app:Safari"` under `macmon stress gpu`, `curl /metrics`; CPU cost of `serve` with and without the option.

## Result

`serve --processes` exports the five process metrics for the matching processes; a query with a bad word is refused, as it would match every process. On M5 / macOS 27.2.1 `gpu:>0` under `macmon stress gpu` showed the stress process, WindowServer and Rex; `""` exported 822 processes. A release build idle at 1 s used 0.45 s of CPU in 30 s with the option against 0.23 s without. Untested: `--install` with a query by hand, other chips and macOS versions.
