# TUI Redesign: btop-style Layout and Process List (Phase 1)

## Overview
- Redesign the interactive TUI in a btop-inspired style: a full-width CPU box on top, a left column with GPU / MEM / POWER boxes, and a process list on the right.
- Replace single-accent coloring with themes and load gradients (green → yellow → red), and use braille history graphs by default.
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
- Theme = named palette (border, title, text, dim, selection, 3-stop load gradient). Colors are RGB; when the terminal doesn't advertise truecolor (`COLORTERM` ≠ `truecolor`/`24bit`) they are mapped to the nearest xterm-256 index.
- Custom widgets: `BrailleGraph` (filled area graph, 2 samples per cell, 4 dots per row, vertical gradient) and `Meter` (horizontal bar with gradient fill). `v` switches graphs to the current block-style `Sparkline`.
- A pure `compute_layout(area, panels, per_core) -> LayoutPlan` decides box rectangles; panels toggle with `1`–`5`; the process panel auto-hides below a minimum size so macmon still works in a small window.
- Process data comes from a separate `procs` thread (own `ProcSampler`), paused while the process panel is hidden, so users who don't need it pay nothing.

## Technical Details

### Layout (A)
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

### Themes
- Built-in: `default`, `nord`, `dracula`, `gruvbox`, `tokyo-night`, `mono` (ANSI/terminal default colors only).
- `c` cycles themes; saved as `theme: String` in config; unknown name → `default`.

### Config migration
- `color` field dropped (serde ignores unknown fields, old files keep loading).
- `view_type`: `Sparkline` → `Braille`, `Gauge` → `Block` via `#[serde(alias)]`.
- New: `theme`, `panels` (5 bools, default all on), `proc_sort` (default `Cpu`), `proc_sort_desc` (default `true`).

### Process sampling (`src_app/procs.rs`)
- `ProcInfo { pid, ppid, name, user, cpu_pct, mem_bytes, power_w: Option<f32>, gpu_pct }`.
- Own processes: `proc_listallpids` → `proc_pidinfo(PROC_PIDTBSDINFO)` (uid, ppid, name) → `proc_pid_rusage(RUSAGE_INFO_V6)` (user+system time, `ri_phys_footprint`, `ri_energy_nj`). `ri_*_time` are mach absolute units → convert with `mach_timebase_info`. Name = basename of `proc_pidpath`, fallback `pbi_name`.
- Foreign processes (libproc failed): one `ps -A -o pid=,ppid=,uid=,rss=,time=,comm=` per tick; parse `[[dd-]hh:]mm:ss.ss`; memory = RSS; power = `None`. Skipped when running as root.
- GPU: walk `IOAccelerator` children, read `IOUserClientCreator` + sum `AppUsage[].accumulatedGPUTime` per pid.
- CPU % follows Activity Monitor convention (100% = one core). Deltas keyed by pid; negative delta or changed start time → treat as new process (no spike).
- User names via `getpwuid_r`, cached per uid.
- Thread `run_procs_thread` uses the same interval `Arc<RwLock<u32>>`, sends `Event::Procs(Vec<ProcInfo>)`; `AtomicBool` pauses it while the proc panel is hidden or auto-hidden.

### Process panel
- Columns by priority (dropped right-to-left on narrow widths): PID, NAME (flex), CPU%, MEM, GPU%, POWER, USER.
- Sort: `s` cycles CPU → MEM → POWER → GPU → PID → NAME; `S` reverses. Shown in title.
- Filter: `/` enters input mode, case-insensitive substring on name or pid; `Enter` keeps, `Esc` clears; shown in title.
- Selection: `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End`; follows the selected pid across refreshes; `Esc` (normal mode) clears.
- Unavailable values render as `-` in dim color; values colored by theme gradient.

### Keys (final)
- `q` / `Ctrl-C` quit · `c` theme · `v` graph style · `d` per-core · `r` ratio mode · `-`/`+` interval · `1`–`5` panels · `/` filter · `s`/`S` sort · arrows / PgUp / PgDn / Home / End selection · `Esc` cancel.

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

- [ ] implement `BrailleGraph` widget: newest-first data, max value, right-aligned, 2 samples per cell, 4 levels per row, per-row gradient color, optional overlay label
- [ ] implement `Meter` widget: label, filled `▰`/empty `▱` (or block chars), gradient color by ratio, right-aligned percent
- [ ] keep block-style fallback via ratatui `Sparkline` behind one `graph()` helper selected by `ViewType`
- [ ] write tests rendering into a `Buffer`: empty data, full data (`⣿`), half height, odd sample count, zero-size area
- [ ] write tests for `Meter` fill width at 0%, 50%, 100% and narrow widths
- [ ] run `make test` and `make check` - must pass before next task

### Task 4: Layout engine with panel toggles and auto-hide

**Files:**
- Create: `src_app/tui/layout.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`

- [ ] implement `compute_layout(area, panels, per_core) -> LayoutPlan` (optional rects for cpu, gpu, mem, power, proc, per-core grid)
- [ ] auto-hide proc panel below `PROC_MIN_WIDTH`/`PROC_MIN_HEIGHT`; left column full width when proc hidden; proc full width when it's the only panel
- [ ] keys `1`–`5` toggle panels and persist in config
- [ ] write tests: 200x50 all panels, 80x24 (proc auto-hidden), only proc, only cpu, all hidden, per-core off
- [ ] write tests: rects never overlap and stay inside the area for a grid of sizes
- [ ] run `make test` and `make check` - must pass before next task

### Task 5: Render metric panels in the new layout

**Files:**
- Create: `src_app/tui/panels.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/tui/store.rs`

- [ ] CPU box: title with chip info / clock / version+interval, E-CPU and P-CPU graphs, per-core meter grid (multi-column, die prefix), CPU temp
- [ ] GPU box, MEM box (RAM/SWAP meters + RAM graph), POWER box (CPU/GPU/ANE rows, SYS + fans footer, avg/max in title)
- [ ] global key hints in the bottom border of the bottom-left box; `v`, `d`, `r`, `-`/`+` keep working
- [ ] remove old render functions that are no longer used
- [ ] write render tests at 200x50, 120x40, 80x24, 60x15: no panic, labels `E-CPU`, `P-CPU`, `GPU`, `RAM`, `ANE` present when their panels are visible
- [ ] write render tests: multi-die cores show `D0`/`D1` prefix; SWAP row hidden when `swap_total == 0`; fans/SYS hidden when unavailable
- [ ] run `make test` and `make check` - must pass before next task

### Task 6: Own-process sampler (libproc + rusage v6)

**Files:**
- Create: `src_app/procs.rs`
- Modify: `src_app/main.rs`

- [ ] define `rusage_info_v6` (`#[repr(C)]`, per SDK), `ProcInfo`, `ProcSampler`
- [ ] collect pids, bsd info, rusage; mach timebase conversion; name from `proc_pidpath` basename
- [ ] pure delta function: (prev counters, cur counters, elapsed) → cpu %, power W; handle first sample, negative delta, pid reuse
- [ ] write tests for delta math (1 core busy = 100%, idle = 0, energy 1e9 nJ over 1 s = 1 W, negative delta → 0, new pid → no spike)
- [ ] write test that sampling the current process returns its own pid with non-empty name (runs on macOS CI)
- [ ] run `make test` and `make check` - must pass before next task

### Task 7: Foreign processes via `ps` fallback and user names

**Files:**
- Modify: `src_app/procs.rs`

- [ ] run `ps -A -o pid=,ppid=,uid=,rss=,time=,comm=` only when some pids failed libproc and euid != 0
- [ ] parse lines (names with spaces, `m:ss.ss`, `h:mm:ss`, `d-hh:mm:ss`), merge into results with `power_w = None`
- [ ] uid → user name via `getpwuid_r` with cache; fallback to numeric uid
- [ ] write tests for ps line parsing and time parsing (valid, malformed, empty)
- [ ] write tests for merge: libproc entries win over ps entries for the same pid
- [ ] run `make test` and `make check` - must pass before next task

### Task 8: Per-process GPU usage from IORegistry

**Files:**
- Modify: `src_app/procs.rs`

- [ ] IOKit FFI in app: match `IOAccelerator`, iterate children, read `IOUserClientCreator` and `AppUsage`
- [ ] parse creator string `"pid <n>, <name>"`; sum `accumulatedGPUTime` per pid; GPU % from delta / elapsed, clamped to 0..=100
- [ ] release all IOKit/CF objects (no leaks per tick)
- [ ] write tests for creator string parsing (valid, missing pid, garbage) and per-pid aggregation/delta
- [ ] write test that reading IORegistry doesn't error on the CI machine (result may be empty)
- [ ] run `make test` and `make check` - must pass before next task

### Task 9: Process sampling thread

**Files:**
- Modify: `src_app/tui/mod.rs`

- [ ] `run_procs_thread(tx, msec, active: Arc<AtomicBool>)` sending `Event::Procs`; sleeps while inactive
- [ ] `active` follows proc panel visibility (toggle + auto-hide) on every render
- [ ] app state stores latest `Vec<ProcInfo>`; panel shows "collecting…" until the first delta sample
- [ ] write tests for the visibility → active flag logic
- [ ] run `make test` and `make check` - must pass before next task

### Task 10: Process panel (table, sort, filter, selection)

**Files:**
- Create: `src_app/tui/proc_view.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`

- [ ] `ProcView` state: sort key/direction (persisted), filter string, input mode, selected pid, scroll offset
- [ ] table rendering with column priority by width, gradient-colored values, `-` for unavailable, selected row highlight, title with filter and sort
- [ ] keys: `/` input mode (chars, Backspace, Enter, Esc), `s`/`S`, arrows/PgUp/PgDn/Home/End, `Esc` clears selection; normal-mode keys ignored while typing
- [ ] proc key hints in the bottom border of the proc box
- [ ] write tests for sorting by each key and direction, filter by name/pid (case-insensitive), selection follows pid after re-sort, clamp when list shrinks, scroll keeps selection visible
- [ ] write tests for column dropping at narrow widths and render of the panel with synthetic `ProcInfo`
- [ ] write tests that `q`/`c` while typing a filter add characters instead of quitting/changing theme
- [ ] run `make test` and `make check` - must pass before next task

### Task 11: Verify acceptance criteria
- [ ] verify all requirements from Overview are implemented (all old metrics visible, themes, braille, panels, process list with POWER/GPU)
- [ ] verify edge cases: tiny window, no swap, no fans, multi-die, theme fallback without truecolor
- [ ] run full test suite: `make test`
- [ ] run `make check`
- [ ] run `cargo run --release` manually and walk through every key

### Task 12: [Final] Update documentation
- [ ] update `readme.md`: features list, Controls section, note on process data without sudo
- [ ] add entry to `changelog.md`
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Ghostty / iTerm2 / Apple Terminal (truecolor and 256-color paths), light and dark terminal backgrounds.
- Small window (e.g. 60x15) and huge window; resize while running.
- M-series with many cores (Max/Ultra) for the per-core grid; Mac without fans (MacBook Air).
- Compare CPU% / MEM / GPU% for a few processes with Activity Monitor.
- CPU overhead of macmon itself with proc panel on vs off.

**Release**:
- new screenshot for `assets` branch / README.
- Phase 2 (separate plan): kill / signals, process tree, details on Enter.
