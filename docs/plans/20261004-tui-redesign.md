# TUI Redesign: btop-style Layout and Process List (Phase 1)

## Overview
- Redesign the interactive TUI in a btop-inspired style: a full-width CPU box on top, a left column with GPU / MEM / POWER boxes, and a process list on the right.
- Replace single-accent coloring with load gradients (green → yellow → red) in the terminal's own colors (built-in themes until Task 12), and use braille history graphs.
- Add a process list (PID, NAME, USER, CPU%, MEM, POWER W, GPU%) with sorting, filtering and selection, so macmon covers the "what is eating my Mac" use case that currently requires btop/htop/Activity Monitor.
- Differentiators vs btop: per-process power (W) and per-process GPU %, both sudoless.
- All existing metrics stay: E-CPU / P-CPU (aggregate + per-core, scaled/active ratio), GPU, RAM / SWAP, CPU / GPU / ANE / total / system power with avg/max, CPU / GPU temperature, fans.
- `pipe`, `serve`, `debug`, `stress` and the public library API are untouched.

## Context (from discovery)
- Files involved: `src_app/tui.rs` (811 lines, all TUI code), `src_app/config.rs` (persisted UI settings in `~/.config/macmon.json`), `src_app/main.rs` (module wiring).
- Patterns: ratatui 0.30 + crossterm; input thread and sampler thread send `Event`s over `mpsc`; history stored newest-first in `Vec` capped at `MAX_SPARKLINE`; config saved on every toggle; inline `mod tests` per file; 2-space indent, max width 100.
- Dependencies: `ratatui`, `libc` (has `proc_listallpids`, `proc_pidinfo`, `proc_pidpath`, `proc_pid_rusage`, structs up to `rusage_info_v4` only), `core-foundation` (non-optional).
- Verified on this machine without sudo:
  - libproc (`PROC_PIDTBSDINFO`, `PROC_PIDTASKINFO`, `proc_pid_rusage`) works only for processes of the current user (551 of 868); for others only `proc_pidpath` works.
  - `rusage_info_v6.ri_energy_nj` is readable for own processes → per-process watts.
  - `/bin/ps` is setuid root: `ps -A -o pid=,ppid=,uid=,rss=,time=,comm=` returns CPU time and RSS for all processes; takes ~20 ms.
  - IORegistry `AGXDeviceUserClient` entries expose `IOUserClientCreator` (`"pid 631, WindowServer"`) and `AppUsage[].accumulatedGPUTime` (ns) for all processes.
- `rusage_info_v6` layout taken from the SDK `sys/resource.h` (fields up to `ri_page_cache_hits`, `ri_reserved[6]`).

## Development Approach
- **testing approach**: Regular (code first, then tests in the same task)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `make test` after each change, `make check` before finishing each task
- maintain backward compatibility of `~/.config/macmon.json` (old configs must load)

## Testing Strategy
- **unit tests**: pure logic — gradients, color fallback, layout computation, ps parsing, CPU/GPU/energy deltas, sort/filter/selection, config migration.
- **render tests**: ratatui `TestBackend` — render `App` with synthetic metrics/processes at several terminal sizes, assert no panic and presence of key labels/cells.
- no e2e framework in the project; manual run checklist in Post-Completion.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview
- Split `src_app/tui.rs` into `src_app/tui/` modules so the redesign doesn't produce a 2k-line file.
- Input thread forwards raw key events (`Event::Key`); the app interprets them by mode (normal / filter input), so typing a filter doesn't trigger `q`/`c`/etc.
- ~~Theme = named palette (border, title, text, dim, selection, 3-stop load gradient). Colors are RGB; when the terminal doesn't advertise truecolor (`COLORTERM` ≠ `truecolor`/`24bit`) they are mapped to the nearest xterm-256 index.~~ Superseded in Task 12 by "Colors: terminal palette".
- Custom widgets: `BrailleGraph` (filled area graph, 2 samples per cell, 4 dots per row, vertical gradient) and `Meter` (horizontal bar with gradient fill). ~~`v` switches graphs to the current block-style `Sparkline`.~~ Braille only since Task 12.
- A pure `compute_layout(area, panels, per_core) -> LayoutPlan` decides box rectangles; panels toggle with `1`–`5`; the process panel auto-hides below a minimum size so macmon still works in a small window.
- Process data comes from a separate `procs` thread (own `ProcSampler`), paused while the process panel is hidden, so users who don't need it pay nothing.

## Technical Details

### Layout V3 (user decision after Task 10 — replaces Layout A below)
```
╭─ M3 Pro · 6E+6P · 18GPU · 36GB ───────────────────── 14:32 · macmon ─╮
│ E-CPU  42% 1.8GHz ⣀⣠⣤⣴⣶⣾⣿⣷⣶⣤⣀⣀⣠⣤⣶⣿⣿⣷⣶⣤⣀⣠ │ CPU  4.2W 58°C ▁▂▃▅▇▅▃    │
│ P-CPU  77% 3.2GHz ⣿⣿⣷⣶⣤⣀⣀⣠⣤⣶⣿⣿⣷⣶⣤⣀⣠⣴⣾⣿⣿⣷ │ GPU  1.1W 45°C ▁▁▂▃▂▁▁    │
│ GPU    23% 1.4GHz ⣀⣀⣀⣠⣤⣤⣀⣀⣀⣀⣠⣤⣴⣶⣤⣀⣀⣀⣀⣀⣀⣠ │ ANE  0.0W      ▁▁▁▁▁▁▁    │
│ RAM    59% 21/36G ▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱ │ SYS 18.3W  fan 1200rpm    │
│ SWAP    4% 1.2/4G ▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱ │ all 12.4W avg 9 max 21    │
│ cores  E ▃▅▂▁▂▁  P ▇▆█▅▃▇                │                           │
╰──────────────────────────────────────────────────────────────────────╯
╭─ proc 612 ── /filter ──────────────────────────────────────── cpu ↓ ─╮
│ PID    NAME                USER      CPU%     MEM   POWER   GPU%     │
│ ...  (full width, ~60% of the screen height)                         │
╰─ q quit · c theme · … · ↑↓ select · / filter · s sort ───────────────╯
```
- Two full-width boxes: metrics on top, process list at the bottom with `PROC_HEIGHT_PCT = 60` of the height. The top box gets the rest but never less than its minimum content height; if the proc box would get fewer than `PROC_MIN_ROWS` rows it auto-hides (height only — width no longer matters).
- Top box title: chip info left (`M3 Pro · 6E+6P · 18GPU · 36GB`), clock + `macmon vX · interval` right.
- Left part = one strip per row: `{label} {pct}% {freq/usage} {graph or meter}`:
  - one strip per CPU cluster, rendered from a generic list of clusters (2 today: E/P on M1–M4, P/S on M5; a third tier appears without layout changes once the library exposes it — M6 has 6E + 4P + 2S);
  - GPU strip; RAM meter strip; SWAP meter strip only when `swap_total > 0`;
  - when the top box has spare rows (tall terminal or proc hidden), the extra rows go to the graph strips (clusters + GPU) so graphs grow taller; the label stays on the strip's first row.
- Cores row(s) (`d` toggles): one vertical bar per core (`▁`…`█`, gradient-colored), grouped by cluster (`E ▃▅▂▁  P ▇▆█▅`). Core counts range from 8 (M1: 4E+4P) to 36 (M5 Ultra: 24P+12S); M3 Ultra has 32 (8E+24P). If one line doesn't fit: one line per die on multi-die chips (`D0 …`, `D1 …`), then wrap per cluster. Each wrap adds a row to the top box.
- Right part = power column (~28 cells): CPU W + temp + sparkline, GPU W + temp + sparkline, ANE W + sparkline, SYS W + fans (only when available), total W with avg / max. On narrow widths (< ~70) the power rows move under the strips.
- Panel keys: `1` CPU strips + cores, `2` GPU strip, `3` RAM/SWAP strips, `4` power column, `5` proc box. Hidden rows shrink the top box; with every metric hidden the proc box takes the full height; with proc hidden the top box takes the full height.
- Key hints: global + proc hints on the bottom border of the bottom-most box (proc hints drop first, `q quit` always stays).

### Layout (A) — superseded by Layout V3
```
╭─ cpu ── M3 Pro · 6E+6P · 18GPU · 36GB ── 14:32 ── macmon v0.9 · 1000ms ─╮
│ E-CPU 42% @ 1.8GHz  ⣀⣠⣤⣴⣶⣾⣿⣷⣶   │ E0 ▰▰▰▱▱ 42%   P0 ▰▰▰▰▱ 77%         │
│ P-CPU 77% @ 3.2GHz  ⣿⣿⣷⣶⣤⣀⣀⣠⣤   │ E1 ▰▰▱▱▱ 31%   P1 ▰▰▰▰▰ 95%         │
╰────────────────────────────────────────────────────────────────────────╯
╭─ gpu 23% @ 1.4GHz · 45°C ─╮╭─ proc ─ /filter ──────────────── cpu ↓ ─╮
╭─ mem ─ RAM / SWAP meters ─╮│ PID NAME USER CPU% MEM POWER GPU%      │
╭─ power ─ CPU/GPU/ANE W ───╮│ ...                                    │
╰── global key hints ───────╯╰── proc key hints ──────────────────────╯
```
- CPU box: chip info in title, clock center, version + interval right. Left: two stacked graphs E-CPU and P-CPU (label overlaid top-left, ratio per `r`). Right: per-core meters grid (E cores then P cores, multi-column when cores > rows, die prefix on multi-die). `d` hides the per-core grid (graphs take full width). CPU temp in title.
- GPU box: graph, title `gpu 23% @ 1.4GHz · 45°C`.
- MEM box: RAM meter + SWAP meter (SWAP only if `swap_total > 0`), RAM graph if height allows.
- POWER box: title `power 12.4W · avg 9.1W · max 21W`; rows CPU / GPU / ANE with W, temp, mini sparkline; footer row SYS W + fans RPM (only when available).
- Panel keys: `1` cpu, `2` gpu, `3` mem, `4` power, `5` proc. Hidden proc panel → left column takes full width. Only proc visible → full width.
- Auto-hide: proc panel hidden when width < `PROC_MIN_WIDTH` (≈ 100) or height < `PROC_MIN_HEIGHT` (≈ 20); thresholds are consts, covered by tests.

### Colors: terminal palette (user decision after Task 11 — replaces built-in themes)
- No own themes and no `c` key: every color comes from the terminal — default fg/bg (`Color::Reset`) and the 16 ANSI colors, so macmon follows the user's terminal theme.
- Borders and dim text: ANSI bright black (8). Selected process row: reverse video.
- Load gradient (graphs, meters, values): terminal green (2) → yellow (3) → red (1).
- At startup (raw mode on, before the input thread starts) query the real palette: OSC 4 for indexes 1/2/3 (+ OSC 10/11 for fg/bg), followed by a DA1 (`ESC [ c`) sentinel so terminals that ignore OSC 4 don't cost the full timeout; overall timeout ≈ 150 ms; drain late replies so they never reach the key handler. Parse `ESC ] 4 ; n ; rgb:R/G/B` with 1–4 hex digits per channel, terminated by BEL or ST.
- Palette known + truecolor (`COLORTERM` = `truecolor`/`24bit`) → smooth RGB interpolation between the terminal's own green/yellow/red. Otherwise → discrete steps (green / yellow / red ANSI indexes), still the terminal's colors.
- ➕ As built (Task 12): the query runs only with truecolor (otherwise the palette is unused); OSC 10/11 are not queried, since nothing uses fg/bg (borders / dim are ANSI 8, selection is reverse video, titles and text are the default fg). Replies are read from `/dev/tty` with `select(2)` (macOS `poll(2)` doesn't support devices). Colors count only within 150 ms; without the DA1 reply by then, input is read and dropped until it arrives, at most 500 ms more, so replies up to ~650 ms late can't become key presses (later ones still could). Discrete steps: green up to 1/3, yellow up to 2/3, red above.

### Graph style (user decision after Task 11)
- Braille only: no `v` key and no block mode. Power-column mini graphs are braille too. Core bars (one `▁`…`█` cell per core) stay — they show current values, not history.

### Config migration
- `color`, `theme`, `view_type` fields dropped (serde ignores unknown fields, old files keep loading).
- New: `panels` (5 bools, default all on), `proc_sort` (default `Cpu`), `proc_sort_desc` (default `true`).

### Process sampling (`src_app/procs.rs`)
- `ProcInfo { pid, ppid, name, user, cpu_pct, mem_bytes, power_w: Option<f32>, gpu_pct }`.
- Own processes: `proc_listallpids` → `proc_pidinfo(PROC_PIDTBSDINFO)` (uid, ppid, name) → `proc_pid_rusage(RUSAGE_INFO_V6)` (user+system time, `ri_phys_footprint`, `ri_energy_nj`). `ri_*_time` are mach absolute units → convert with `mach_timebase_info`. Name = basename of `proc_pidpath`, fallback `pbi_name`.
- Foreign processes (libproc failed): one `ps -A -o pid=,ppid=,uid=,rss=,time=,comm=` per tick; parse `[[dd-]hh:]mm:ss.ss`; memory = RSS; power = `None`. Skipped when running as root.
- GPU: walk `IOAccelerator` children, read `IOUserClientCreator` + sum `AppUsage[].accumulatedGPUTime` per pid.
- CPU % follows Activity Monitor convention (100% = one core). Deltas keyed by pid; negative delta, changed start time or changed command → treat as new process (no spike).
- User names via `getpwuid_r`, cached per uid.
- Thread `run_procs_thread` uses the same interval `Arc<RwLock<u32>>`, sends `Event::Procs(Vec<ProcInfo>)`; `AtomicBool` pauses it while the proc panel is hidden or auto-hidden.

### Process panel
- Columns by priority (dropped right-to-left on narrow widths): PID, NAME (flex), CPU%, MEM, GPU%, POWER, USER.
- Sort: `s` cycles CPU → MEM → POWER → GPU → PID → NAME; `S` reverses. Shown in title.
- Filter: `/` enters input mode, case-insensitive substring on name or pid; `Enter` keeps, `Esc` clears; shown in title.
- Selection: `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End`; follows the selected pid across refreshes; `Esc` (normal mode) clears.
- Unavailable values render as `-` in dim color; values colored by theme gradient.

### Keys (final)
- `q` / `Ctrl-C` quit · `d` per-core · `r` ratio mode · `-`/`+` interval · `1`–`5` panels · `/` filter · `s`/`S` sort · arrows / PgUp / PgDn / Home / End selection · `Esc` cancel.

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): code, tests, docs in this repo.
- **Post-Completion** (no checkboxes): manual checks on real terminals and hardware, screenshot update.

## Implementation Steps

### Task 1: Split TUI into modules and forward raw key events

**Files:**
- Create: `src_app/tui/mod.rs`
- Create: `src_app/tui/store.rs`
- Delete: `src_app/tui.rs`
- Modify: `src_app/config.rs` (➕ config path disabled under `cfg!(test)` so tests never read/overwrite `~/.config/macmon.json`)

- [x] move `src_app/tui.rs` to `src_app/tui/mod.rs`; move `RatioSeries`, `FreqSample`, `FreqStore`, `CoreId`, `CpuFreqStore`, `PowerStore`, `MemoryStore`, `TempStore`, `FanStore` to `src_app/tui/store.rs` unchanged (plus `avg2`, `MAX_SPARKLINE`, `MAX_TEMPS` which only stores use)
- [x] replace per-key `Event` variants with `Event::Key(KeyEvent)`; handle keys in `App::handle_key` (same behavior as today; returns `ControlFlow::Break` on quit)
- [x] write tests for stores (`PowerStore` avg/max, `TempStore` zero fallback, `CpuFreqStore` missing core push)
- [x] write tests for `App::handle_key` mapping (q, Ctrl-C, c, v, d, r, +, =, -)
- [x] write render smoke test with `TestBackend` and synthetic `Metrics` (current layout, 120x40)
- [x] run `make test` and `make check` - must pass before next task

### Task 2: Themes, gradients and config migration

**Files:**
- Create: `src_app/tui/theme.rs`
- Modify: `src_app/config.rs`
- Modify: `src_app/tui/mod.rs`

- [x] add `Theme` struct and 6 built-in themes; `gradient(t: f64) -> Color` with 3-stop interpolation
- [x] add truecolor detection and RGB → xterm-256 fallback
- [x] config: replace `color` with `theme`, rename `ViewType` variants to `Braille`/`Block` with serde aliases, add `panels`, `proc_sort`, `proc_sort_desc`; `c` cycles themes
- [x] apply theme to current widgets (borders, titles, graph colors) so the app stays usable mid-refactor
- [x] write tests for gradient endpoints/midpoint, 256 mapping (pure colors, grays), theme cycle wrap, unknown theme fallback
- [x] write tests for loading old config JSON (`color`, `view_type: "Gauge"`/`"Sparkline"`) and empty JSON defaults
- [x] run `make test` and `make check` - must pass before next task

### Task 3: Braille graph and meter widgets

**Files:**
- Create: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/mod.rs` (➕ current render code switched to the new widgets so they aren't dead code)

- [x] implement `BrailleGraph` widget: newest-first data, max value, right-aligned, 2 samples per cell, 4 levels per row, per-row gradient color, optional overlay label (implemented as `widgets::Graph`, braille is the default style; row color is capped by the cell's value so 1-row graphs still reflect load; non-zero values get at least one dot; auto max = largest visible sample)
- [x] implement `Meter` widget: label, filled `▰`/empty `▱` (or block chars), gradient color by ratio, right-aligned percent (`block_chars(true)` → `█`/`░`, used in `ViewType::Block`; narrow widths drop the bar first, then the label)
- [x] keep block-style fallback via ratatui `Sparkline` behind one `graph()` helper selected by `ViewType` (bars colored by value)
- [x] ➕ migrate current render functions to `graph()` / `Meter` (old `Gauge` mode removed, per-core view shows meters, `bar_set()` moved to widgets)
- [x] write tests rendering into a `Buffer`: empty data, full data (`⣿`), half height, odd sample count, zero-size area
- [x] write tests for `Meter` fill width at 0%, 50%, 100% and narrow widths
- [x] run `make test` and `make check` - must pass before next task

### Task 4: Layout engine with panel toggles and auto-hide

**Files:**
- Create: `src_app/tui/layout.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`

- [x] implement `compute_layout(area, panels, per_core) -> LayoutPlan` (optional rects for cpu, gpu, mem, power, proc, per-core grid) (CPU box = 1/3 of the height, min 6; `cpu_graphs` / `cores` are inside the CPU box borders, cores take the right half; left column 40% wide; POWER keeps 6 rows, GPU / MEM share the rest; zero-size boxes are `None`)
- [x] auto-hide proc panel below `PROC_MIN_WIDTH`/`PROC_MIN_HEIGHT`; left column full width when proc hidden; proc full width when it's the only panel (proc is never auto-hidden when it's the only visible panel)
- [x] keys `1`–`5` toggle panels and persist in config
- [x] ➕ `LayoutPlan::bottom_left()` picks the box for the global key hints; current render code switched to `compute_layout` (interim boxes, empty `proc` placeholder, "all panels hidden" hint)
- [x] write tests: 200x50 all panels, 80x24 (proc auto-hidden), only proc, only cpu, all hidden, per-core off
- [x] write tests: rects never overlap and stay inside the area for a grid of sizes
- [x] run `make test` and `make check` - must pass before next task

### Task 5: Render metric panels in the new layout

**Files:**
- Create: `src_app/tui/panels.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/tui/store.rs`

- [x] CPU box: title with chip info / clock / version+interval, E-CPU and P-CPU graphs, per-core meter grid (multi-column, die prefix), CPU temp (titles `cpu 45°C` · chip info, clock centered, `macmon vX · 1000ms` right; ➕ `Titles` fits titles on the border without overlap: first left title truncated, then later left / right / center dropped; core grid is column-major with balanced rows, labels `E0` / `D1 P3`)
- [x] GPU box, MEM box (RAM/SWAP meters + RAM graph), POWER box (CPU/GPU/ANE rows, SYS + fans footer, avg/max in title) (POWER rows show W, temp and a history graph, per-row avg/max only when a graph of ≥ 8 cells still fits; boxes with < 3 inner rows put CPU / GPU / ANE on one line; fans / SYS moved out of the title, fixing the overwritten `power` title in the 40%-wide box)
- [x] global key hints in the bottom border of the bottom-left box; `v`, `d`, `r`, `-`/`+` keep working (hints that don't fit are dropped whole from the end, `q quit` stays first)
- [x] remove old render functions that are no longer used (➕ also the unused `MemoryStore` swap history / max fields)
- [x] write render tests at 200x50, 120x40, 80x24, 60x15: no panic, labels `E-CPU`, `P-CPU`, `GPU`, `RAM`, `ANE` present when their panels are visible
- [x] write render tests: multi-die cores show `D0`/`D1` prefix; SWAP row hidden when `swap_total == 0`; fans/SYS hidden when unavailable
- [x] run `make test` and `make check` - must pass before next task

### Task 6: Own-process sampler (libproc + rusage v6)

**Files:**
- Create: `src_app/procs.rs`
- Modify: `src_app/main.rs`

- [x] define `rusage_info_v6` (`#[repr(C)]`, per SDK), `ProcInfo`, `ProcSampler` (layout checked by a test: 464 bytes, `rusage_info_v4` prefix; ➕ `mod procs` is `#[allow(dead_code)]` in `main.rs` until Task 9 wires it in)
- [x] collect pids, bsd info, rusage; mach timebase conversion; name from `proc_pidpath` basename (➕ falls back to `RUSAGE_INFO_V4` without energy → `power_w = None` on macOS < 13; name cached per pid while start time and `pbi_comm` stay the same; `user` is the numeric uid until Task 7, `gpu_pct` is 0 until Task 8; pid identity = `ri_proc_start_abstime`)
- [x] pure delta function: (prev counters, cur counters, elapsed) → cpu %, power W; handle first sample, negative delta, pid reuse (`usage()`; first sample / reuse / backwards counter / zero elapsed → 0 % and 0 W)
- [x] write tests for delta math (1 core busy = 100%, idle = 0, energy 1e9 nJ over 1 s = 1 W, negative delta → 0, new pid → no spike)
- [x] write test that sampling the current process returns its own pid with non-empty name (runs on macOS CI) (also checks ppid, uid, memory, and CPU % > 0 after a 50 ms busy loop)
- [x] run `make test` and `make check` - must pass before next task

### Task 7: Foreign processes via `ps` fallback and user names

**Files:**
- Modify: `src_app/procs.rs`

- [x] run `ps -A -o pid=,ppid=,uid=,rss=,time=,comm=` only when some pids failed libproc and euid != 0 (runs `/bin/ps` by absolute path; the `ps` child's own pid is dropped from its output)
- [x] parse lines (names with spaces, `m:ss.ss`, `h:mm:ss`, `d-hh:mm:ss`), merge into results with `power_w = None` (memory = RSS; ➕ both sources produce a `Raw` row, rates are computed once in `ProcSampler::update`; `ps` has no start time, so a process is the same while pid + start time + command match — a changed command reads as a new process, also after `exec`; names still come from `proc_pidpath`, falling back to the basename of `comm`)
- [x] uid → user name via `getpwuid_r` with cache; fallback to numeric uid (cache = `ProcSampler::users`)
- [x] write tests for ps line parsing and time parsing (valid, malformed, empty)
- [x] write tests for merge: libproc entries win over ps entries for the same pid (➕ also `update()` rates for ps rows without power, command change → no spike, user names; the live sampling test checks `launchd` comes in as `root`)
- [x] run `make test` and `make check` - must pass before next task

### Task 8: Per-process GPU usage from IORegistry

**Files:**
- Modify: `src_app/procs.rs`

- [x] IOKit FFI in app: match `IOAccelerator`, iterate children, read `IOUserClientCreator` and `AppUsage` (children walked in the service plane, the clients are `!registered`; ➕ walk retried up to 3 times when `IOIteratorIsValid` reports a registry change mid-walk; read once per tick next to the tick timestamp)
- [x] parse creator string `"pid <n>, <name>"`; sum `accumulatedGPUTime` per pid; GPU % from delta / elapsed, clamped to 0..=100 (GPU time is `Counters::gpu_ns`, so pid reuse / exec follow the same no-spike rules; missing time on either side or time going backwards (a client closed) → 0 % without touching CPU %)
- [x] release all IOKit/CF objects (no leaks per tick) (`IoObject` / `IoIter` release on drop, CF values use `core-foundation` wrappers; checked manually: 30000 reads keep the task's mach send rights flat and the footprint doesn't grow with the iteration count; one read ≈ 0.6 ms)
- [x] write tests for creator string parsing (valid, missing pid, garbage) and per-pid aggregation/delta (➕ also `AppUsage` parsing on synthetic CF arrays and `update()` GPU rates)
- [x] write test that reading IORegistry doesn't error on the CI machine (result may be empty)
- [x] run `make test` and `make check` - must pass before next task

### Task 9: Process sampling thread

**Files:**
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/main.rs` (➕)

- [x] `run_procs_thread(tx, msec, active: Arc<AtomicBool>)` sending `Event::Procs`; sleeps while inactive (polls the flag every 100 ms; a pause drops the `ProcSampler`, so after a resume the first sample is a silent baseline followed by a 250 ms warm-up sample, then one sample per interval; exits when the receiver is gone, returns its `JoinHandle`)
- [x] `active` follows proc panel visibility (toggle + auto-hide) on every render (`App::set_procs_visible(plan.proc.is_some())`; false until the first frame)
- [x] app state stores latest `Vec<ProcInfo>`; panel shows "collecting…" until the first delta sample (`App::procs: Option<Vec<ProcInfo>>`; hiding the panel drops the list and samples arriving while hidden, so a re-shown panel never shows stale rows; interim panel body is "N processes" until Task 10)
- [x] ➕ remove `#[allow(dead_code)]` from `mod procs` in `src_app/main.rs` (`ProcSampler::sample()` returns zero CPU / power on its first call) (no narrower allow needed: the derived `PartialEq` on `ProcInfo` reads its fields)
- [x] write tests for the visibility → active flag logic (render-driven: auto-hide, `5` toggle, only-proc, all hidden; collecting → count; hidden panel drops samples; real thread: no samples while paused, own pid once active, at most one in-flight sample after pausing, exits on receiver drop)
- [x] run `make test` and `make check` - must pass before next task

### Task 10: Process panel (table, sort, filter, selection)

**Files:**
- Create: `src_app/tui/proc_view.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`

- [x] `ProcView` state: sort key/direction (persisted), filter string, input mode, selected pid, scroll offset (`ProcView` owns the latest list, replacing `App::procs`; sort changes are saved via `Config::set_proc_sort`, ➕ `ProcSort::next` / `label`; ties sort by pid, processes without power go last when sorted by power; a selected process that disappears hands the selection to the row at the same position, clamped to the last row)
- [x] table rendering with column priority by width, gradient-colored values, `-` for unavailable, selected row highlight, title with filter and sort (title `proc 412` / `proc 12/412`, `/filter█` while typing, `cpu ↓` right; NAME min 8 cells, takes the rest; one blank cell before the right border; zero values dim; CPU / GPU gradient by %/100, MEM by share of RAM, POWER by W/10; sorted column header in title color)
- [x] keys: `/` input mode (chars, Backspace, Enter, Esc), `s`/`S`, arrows/PgUp/PgDn/Home/End, `Esc` clears selection; normal-mode keys ignored while typing (➕ proc keys act only while the panel is on screen, hiding it ends input mode; Ctrl-C quits even while typing; `Esc` in normal mode clears the filter when nothing is selected; navigation keys also work while typing; no selection → the table shows its top)
- [x] proc key hints in the bottom border of the proc box (➕ `draw_hints` shared with the global hints; when the proc box also holds the global hints, the proc hints follow them and give way first)
- [x] write tests for sorting by each key and direction, filter by name/pid (case-insensitive), selection follows pid after re-sort, clamp when list shrinks, scroll keeps selection visible
- [x] write tests for column dropping at narrow widths and render of the panel with synthetic `ProcInfo`
- [x] write tests that `q`/`c` while typing a filter add characters instead of quitting/changing theme
- [x] run `make test` and `make check` - must pass before next task

### Task 11: ➕ Switch to Layout V3 (full-width proc list at the bottom)

User decision after Task 10: process list full width at the bottom (60% of the height), all metrics compacted into one strips box on top. See "Layout V3" in Technical Details.

**Files:**
- Modify: `src_app/tui/layout.rs`
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/tui/store.rs`
- Modify: `src_app/tui/widgets.rs` (➕ `Meter` is a plain bar, `Graph` label overlay removed, `core_bar`)
- Modify: `src_app/config.rs` (only if panel semantics need it; old configs must keep loading) (➕ not needed: `Panels` fields map 1:1 to the V3 rows)

- [x] rework `compute_layout` for V3: top metrics box + full-width proc box with `PROC_HEIGHT_PCT = 60`, top box min content height, proc auto-hide by height only (`PROC_MIN_ROWS`), spare rows grow the graph strips (`compute_layout(area, panels, per_core, &Content)`; `PROC_MIN_ROWS = 3` process rows + borders + header; one padding cell inside the left / right border; spare rows split evenly over the cluster / GPU graphs, extra ones to the first; ➕ without graph strips the top box keeps only the rows it needs and the proc box takes the rest, so hidden rows really shrink it; `LayoutPlan::bottom()` = proc box, else top box)
- [x] render strips from a generic list of CPU clusters (built from `Metrics` in the store, so tests can inject 3 clusters), then GPU, RAM, SWAP; power column on the right, moved under the strips on narrow widths (`store::CpuClusters` fed by `cluster_samples(soc, metrics)`; strip = `E-CPU  42% 1.8GHz {graph}`, RAM / SWAP `56% 20/36G {meter}`, percent colored by the gradient; power column `POWER_WIDTH = 30` behind a ` │ ` separator when the box is ≥ `POWER_SIDE_MIN_WIDTH = 70` wide; rows `CPU 4.50W 45°C {graph}`, GPU, ANE, `SYS 12.00W  fan 1200rpm` (only when available), `all 6.60W avg 6.6 max 6.6`; title: chip summary left (`M3 Pro · 6E+6P · 18GPU · 36GB`, core counts from the clusters), `clock · macmon vX · 1000ms` right, only the clock when that doesn't fit)
- [x] cores row: one bar per core grouped by cluster, wrap per die then per cluster; `d` toggles it (`layout::core_lines`: one line → one per die (`D0 …`) → one per die and cluster → clusters wrapped in balanced chunks; `cores` label on the first line, bars `▁`…`█` aligned under the percent digits)
- [x] panel keys `1`–`5` mapped to CPU / GPU / MEM rows, power column, proc box (`1` hides the cluster strips and the cores row)
- [x] remove Layout A code that becomes unused (left column, CPU box with per-core meter grid, separate GPU / MEM / POWER boxes) (➕ also `Titles` center title, `grid_cells`, `pad_labels`, `CpuFreqStore::has_multiple_dies`, `MemoryStore` RAM history)
- [x] write layout tests at 200x50, 120x40, 100x30, 80x24, 72x24, 60x15: proc gets ~60% of the height, proc hidden → top box full height, all metrics hidden → proc full height, boxes inside the area and non-overlapping (plus `core_lines` wrap levels, every core exactly once at widths 0..60, 3 clusters)
- [x] write render tests for core configs with synthetic data: M1 (4E+4P), M4 Max (4E+12P), three clusters like M6 (6E+4P+2S), M3 Ultra (8E+24P, 2 dies), M5 Ultra (24P+12S, 2 dies) at widths 72 and 100 — every core bar rendered, nothing overflows the box, die wrap when one line doesn't fit (Ultras wrap per die at 72, one line at 100)
- [x] run `make test` and `make check` - must pass before next task

### Task 12: ➕ Terminal palette colors and braille-only graphs

User decision after Task 11: follow the terminal's color scheme instead of built-in themes, and keep one canonical braille style. See "Colors: terminal palette" and "Graph style" in Technical Details.

**Files:**
- Create: `src_app/tui/palette.rs` (terminal palette query + reply parsing; may replace `theme.rs`)
- Modify: `src_app/tui/theme.rs` (or delete if fully replaced)
- Modify: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/proc_view.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`

- [x] replace built-in themes with one terminal-palette theme (default fg/bg + ANSI 16; borders/dim = bright black; selected row = reverse video); remove `c` and the `theme` config field (`theme.rs` kept for `Theme`: border / dim `DarkGray` (ANSI 8), title / text `Reset` (titles stay bold), `selected` = default fg + `REVERSED` so a selected row reads as one bar instead of reversing each gradient color; xterm-256 mapping and the 6 themes removed)
- [x] startup palette query: OSC 4 (1/2/3) + OSC 10/11 with a DA1 sentinel and ≈150 ms timeout, run before the input thread; drain late replies; smooth gradient between the queried colors when truecolor, discrete ANSI green/yellow/red otherwise (➕ new `palette.rs`; ⚠️ OSC 10/11 dropped: nothing uses fg/bg; query skipped without truecolor; `run_loop` now enters raw mode, queries, then starts the input thread; drain = read until the DA1 reply, at most 500 ms after the 150 ms timeout; I/O behind a `TimedRead` trait, real `Tty` = `/dev/tty` + `select(2)`; checked end to end with a scripted fake terminal on a pty: answered → RGB from the queried colors, late (300 ms) / silent → ANSI steps and no leaked key presses, no `COLORTERM` → no query)
- [x] braille only: remove `ViewType`, `v`, the `view_type` config field, the block fallback in `Graph`, `Meter` block chars and `bar_set()`; power-column mini graphs use braille (`graph()` → `Graph::new(data, theme)`; key hints now `q quit  d cores  r scaled  -/+ 1000ms  1-5 panels`)
- [x] write tests for OSC reply parsing: BEL and ST terminators, 1–4 hex digits per channel, several replies in one buffer, garbage, partial/truncated replies, DA1 sentinel (➕ also query flow on a fake terminal: query bytes, replies byte by byte, DA1-only stops early, drain stops at DA1 and leaves later input, read error; and on a real pty: `select` timeout, answered query, late replies drained)
- [x] write tests for the gradient: palette + truecolor → RGB between the queried colors; no palette or no truecolor → only ANSI indexed colors (no RGB anywhere in a rendered frame) (rendered frames with strips, cores, power, procs and a selected row at 200x50 / 80x24 / 60x15: only `Reset` / `DarkGray` / green / yellow / red, all three load colors present; smooth: every RGB cell between green–yellow or yellow–red, the rest `Reset` / `DarkGray`)
- [x] write tests: old configs with `color` / `theme` / `view_type` still load; `c` and `v` do nothing; update render tests that relied on themes or block mode (selection test checks `REVERSED` over the whole row; `graphs_are_braille` replaces the view-type tests)
- [x] run `make test` and `make check` - must pass before next task

### Task 13: Verify acceptance criteria
- [ ] verify all requirements from Overview are implemented (all old metrics visible, terminal palette colors, braille, panels, process list with POWER/GPU)
- [ ] verify edge cases: tiny window, no swap, no fans, multi-die, palette query unanswered / no truecolor
- [ ] ➕ skip the palette query in SSH sessions (`SSH_TTY` / `SSH_CONNECTION` set): replies later than ~650 ms leak into the key handler (found in Task 12), and high latency links are where that happens; fall back to ANSI steps there; add a unit test for the skip decision
- [ ] run full test suite: `make test`
- [ ] run `make check`
- [ ] run `cargo run --release` manually and walk through every key

### Task 14: [Final] Update documentation
- [ ] update `readme.md`: features list, Controls section, note on process data without sudo
- [ ] add entry to `changelog.md`
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Ghostty / iTerm2 / Apple Terminal / inside tmux: palette query answered vs not (smooth vs stepped gradient), no stray characters from late replies, light and dark terminal themes.
- Small window (e.g. 60x15) and huge window; resize while running.
- M-series with many cores (Max/Ultra) for the per-core grid; Mac without fans (MacBook Air).
- Compare CPU% / MEM / GPU% for a few processes with Activity Monitor.
- CPU overhead of macmon itself with proc panel on vs off.

**Release**:
- new screenshot for `assets` branch / README.
- Phase 2 (separate plan): kill / signals, process tree, details on Enter.

**Library follow-up** (separate plan):
- M6 has three CPU tiers (6E + 4P + 2S). The library exposes only two clusters (`ecpu_*` / `pcpu_*`): `cpu_tier_counts` reads perflevel0 and the last perflevel only, and `MCPU` channels are classified as the E slot, so on M6 the P and E tiers most likely merge into one cluster with a wrong label. Needs a verified fix on real M6 hardware and a public API for N clusters; the TUI strips are already generic over clusters.
- Per-process watts looked low in a spot check (ghostty ~20% CPU → ~0.06 W): compare `ri_energy_nj` against Activity Monitor / `powermetrics --show-process-energy`.
