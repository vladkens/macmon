# TUI Redesign: Original Metric Boxes over a Process List (Phase 1)

Current design: Tasks 19–20 (summary in "Current design" under Technical Details). Earlier layouts, themes and graph styles below are kept as history and marked as superseded.

## Overview
- Redesign the interactive TUI ~~in a btop-inspired style: a full-width CPU box on top, a left column with GPU / MEM / POWER boxes, and a process list on the right~~ (Layout V3 since Task 11: one metrics box on top, full-width process list below; since Task 19 the metrics box holds the original macmon boxes in the top 40 % of the screen).
- Replace single-accent coloring with load gradients (green → yellow → red) in the terminal's own colors (built-in themes until Task 12), and use braille history graphs (solid block bars since Task 17, filling their whole box since Task 19).
- Add a process list (PID, NAME, USER, CPU%, MEM, POWER W, GPU%) with sorting, filtering and selection, so macmon covers the "what is eating my Mac" use case that currently requires btop/htop/Activity Monitor.
- Differentiators vs btop: per-process power (W) and per-process GPU %, both sudoless.
- All existing metrics stay: E-CPU / P-CPU (aggregate ~~+ per-core~~ (per-core view removed in Task 16), scaled/active ratio), GPU, RAM / SWAP, CPU / GPU / ANE / total / system power with avg/max, CPU / GPU temperature, fans.
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
- Input thread forwards raw key events (`Event::Key`); the app interprets them by mode (normal / filter input), so typing a filter doesn't trigger `q` / `p` / `v` / etc.
- ~~Theme = named palette (border, title, text, dim, selection, 3-stop load gradient). Colors are RGB; when the terminal doesn't advertise truecolor (`COLORTERM` ≠ `truecolor`/`24bit`) they are mapped to the nearest xterm-256 index.~~ Superseded in Task 12 by "Colors: terminal palette".
- ~~Custom widgets: `BrailleGraph` (filled area graph, 2 samples per cell, 4 dots per row, vertical gradient) and `Meter` (horizontal bar with gradient fill). `v` switches graphs to the current block-style `Sparkline`.~~ Superseded: braille only in Task 12, solid block bars in Task 17, `Meter` removed and a multi-row `Graph` in Task 19, `Gauge` on `v` in Task 20 (see "Graph style").
- A pure ~~`compute_layout(area, panels, per_core) -> LayoutPlan`~~ `compute_layout(area, procs, clusters) -> LayoutPlan` (Task 19) decides box rectangles; ~~panels toggle with `1`–`5`~~ `p` shows / hides the process list (Task 16); the process panel auto-hides below a minimum height so macmon still works in a small window.
- Process data comes from a separate `procs` thread (own `ProcSampler`), paused while the process panel is hidden, so users who don't need it pay nothing.

## Technical Details

### Current design (Tasks 19–20)
- Metrics box in the top 40 % of the screen (at least 8 rows), the process list in the rest; the metrics take the whole screen when the list is hidden (`p`) or auto-hidden (fewer than 3 process rows).
- Inside the metrics box the original macmon boxes: one per CPU cluster, GPU and RAM on top, CPU / GPU / ANE power below, widths split evenly. Titles step down to fit (Task 20); the chip on the outer title, the power summary on its bottom border.
- Multi-row solid bar graphs (eighths per row, three levels in Apple Terminal), per-column load color, power graphs in the low color scaled to their visible peak; `v` switches the cluster / GPU / RAM boxes to gauges.
- Colors from the terminal palette (see "Colors: terminal palette"); process list, keys and mouse as in "Process panel" and "Keys (final)".
- ➕ Task 21: `?` help overlay, `←` / `→` sort, the selected process (PID + path) or a POWER note on the bottom border of the process box, clickable footer hints; mouse capture only while the process list is shown.

### Layout V3 (user decision after Task 10 — replaces Layout A below; superseded by Task 16, then by the metric boxes of Task 19)
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
- ➕ Since Task 14 every power row has avg / max: `CPU   4.50W avg  3.10 max  8.20  45°C ⣀⣠⣤⣴⣶⣾⣿⣷`, `SYS  16.00W avg 14.00 max 20.00  fan 1200rpm`, `all   9.50W avg  8.00 max 12.00`. The column is as wide as its full text plus an 8-cell graph (46 cells with one fan) while the strips keep 36 cells, then shrinks to 31 cells (numbers with avg / max); graphs go first, then temperatures, then avg / max. Fans move to a row of their own when they don't fit after SYS.
- Panel keys: `1` CPU strips + cores, `2` GPU strip, `3` RAM/SWAP strips, `4` power column, `5` proc box. Hidden rows shrink the top box; with every metric hidden the proc box takes the full height; with proc hidden the top box takes the full height.
- Key hints: global + proc hints on the bottom border of the bottom-most box (proc hints drop first, `q quit` always stays).
- ➕ Since Task 16 (user review): the top box is as tall as its content and the proc box takes the rest (no `PROC_HEIGHT_PCT`, no growing strips); no cores row, no `d`, no panel keys — metrics are always visible and `p` shows / hides the proc box; header = chip info left, `macmon vX` right; hints right-aligned ~~`q quit | r scaled | -/+ 1000ms | / filter | s sort`~~ `q quit | p procs | r scaled | -/+ 1000ms` (➕ Task 18); power rows `CPU` / `GPU` / `ANE` / `Power` (all_power) / `Total` (sys_power) + fans. See the Task 16 mockup.

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
- ➕ As built (Task 13): no query in SSH sessions (`SSH_TTY` or `SSH_CONNECTION` non-empty), where replies are most likely to come after the drain window; those sessions get the discrete steps.

### Graph style (user decision after Task 11, revised after Task 16)
- ~~Braille only~~ — superseded: one-row braille has only 4 levels and low loads read as a dotted line. User picked variant B from a comparison page shown during the review (not public): solid block bars `▁▂▃▄▅▆▇█`, 8 levels per row, one sample per cell, each bar colored by its own value on the load gradient. ~~Still one canonical style: no `v` key, no toggle.~~ (`v` is back in Task 20, see below.)
- Power graphs (the power column until Task 19, the power boxes since): block bars in the low (green) color, no gradient.
- ➕ Task 20: `v` is back (as in the original): the CPU cluster, GPU and RAM boxes switch between the history graph and a gauge (bar filled to the current load in its load color); power boxes always show graphs.
- ➕ Review 1: Apple Terminal draws gaps between eighth blocks; as in v0.7.2 (`bar_set()` with `THREE_LEVELS` for `TERM_PROGRAM=Apple_Terminal`, lost in Task 12) the bars there use three levels: 1/8 blank, 2/8–6/8 `▄`, 7/8–8/8 `█`. ratatui's `Sparkline` / `Gauge` don't replace the own widgets: `Sparkline` rounds bars down (small non-zero samples blank) and scales to the peak of all its data, not the visible columns; `Gauge` with an empty label still paints a reversed blank cell in its middle.

### Config migration
- `color`, `theme`, ~~`view_type`~~ fields dropped (serde ignores unknown fields, old files keep loading); `view_type` is back since Task 20.
- New: ~~`panels` (5 bools, default all on)~~ (dropped in Task 16), `proc_sort` (default `Cpu`), `proc_sort_desc` (default `true`).
- ➕ Task 16: `panels` and `per_core_view` dropped (ignored on load); new `show_procs` (default `true`, key `p`).
- ➕ Task 20: `view_type` back with the released values (`"Sparkline"` = graph, `"Gauge"`); unknown values fall back to the graph.
- ➕ Review 1: every field falls back on its own: a bad value (wrong type, unknown name) gets that field's default, the others keep theirs (released versions reset the whole file). Under `sudo` (which keeps `HOME`) only an existing file is rewritten, so no root-owned file or directory is created. The file path is a field of `Config`, so tests save to a temp file.

### Process sampling (`src_app/procs.rs`)
- `ProcInfo { pid, ~~ppid,~~ name, user, cpu_pct, mem_bytes, power_w: Option<f32>, gpu_pct }` (➕ Review 1: `ppid` dropped until the process tree of Phase 2 needs it).
- Own processes: `proc_listallpids` → `proc_pidinfo(PROC_PIDTBSDINFO)` (uid, ppid, name) → `proc_pid_rusage(RUSAGE_INFO_V6)` (user+system time, `ri_phys_footprint`, `ri_energy_nj`). `ri_*_time` are mach absolute units → convert with `mach_timebase_info`. Name = basename of `proc_pidpath`, fallback `pbi_name`.
- Foreign processes (libproc failed): one `ps -A -o pid=,uid=,rss=,time=,comm=` per tick; parse ~~`[[dd-]hh:]mm:ss.ss`~~ `mm:ss.ss` (➕ Review 1: macOS `ps` prints only minutes, growing past 59); memory = RSS; power = `None`. Skipped when running as root.
- GPU: walk `IOAccelerator` children, read `IOUserClientCreator` + sum `AppUsage[].accumulatedGPUTime` per pid.
- CPU % follows Activity Monitor convention (100% = one core). Deltas keyed by pid; negative delta, changed start time or changed command → treat as new process (no spike).
- User names via `getpwuid_r`, cached per uid.
- Thread `run_procs_thread` uses the same interval `Arc<RwLock<u32>>`, sends `Event::Procs(Vec<ProcInfo>)`; `AtomicBool` pauses it while the proc panel is hidden or auto-hidden.

### Process panel
- Columns by priority (dropped right-to-left on narrow widths): PID, NAME (flex), CPU%, MEM, GPU%, POWER, USER.
- Sort: `s` cycles CPU → MEM → POWER → GPU → PID → NAME → USER (USER since Task 18); `S` reverses. ~~Shown in title.~~ The arrow sits next to the sorted column's header (Task 18).
- Filter: `/` enters input mode, case-insensitive substring on name or pid; `Enter` keeps, `Esc` clears; shown in title (➕ Review 1: a filter too long for the border shows its end, `/…ari█`; with no room next to the count it takes the count's place, so the text, cursor and click target never vanish).
- Selection: `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End`; follows the selected pid across refreshes; `Esc` (normal mode) clears.
- Unavailable values render as `-` in dim color; values colored by theme gradient.
- ➕ Task 18: title `proc N ─ / filter` (filter text instead once set / typing), no sort in the title; the sort arrow sits next to the sorted column header; USER sorts too (`s` cycle ends with USER); mouse: header click sorts / reverses, `/ filter` click starts input, row click selects, wheel moves selection + scroll by 3.
- ➕ Task 21: title `proc N ─ / filter ─ ~~← sort →~~ s sort` (➕ Review 2: `s sort`, a clickable hint for `s`, left out while a filter is typed); a newly chosen column sorts in its own direction; the sorted column is never dropped, USER drops before NAME < 16; selection only from ↑/↓ or a click (the wheel and paging scroll without one), dropped with its process; bottom border: selected PID + path or `POWER: own processes only`, then the key hints.

### Keys (final)
- `q` / `Ctrl-C` quit · ~~`d` per-core~~ · `r` ratio mode · `-`/`+` interval · ~~`1`–`5` panels~~ `p` process list (➕ Task 16) · `/` filter · `s`/`S` sort · arrows / PgUp / PgDn / Home / End selection · `Esc` cancel.
- ➕ Task 18: footer ` q quit | p procs | r scaled | -/+ 1000ms ` (global keys only); mouse in the process list (see Process panel).
- ➕ Task 20: `v` graph / gauge; footer ` q quit | p procs | v chart | r scaled | -/+ 1000ms `.
- ➕ Task 21: `?` help; ~~`←` / `→` sort~~ (removed in Review 2); footer ` q quit | ? help | p procs | v graph | r scaled | -/+ 1000ms ` (toggle state shown, `p procs` left out while ~~auto-hidden~~ the window has no room for the list, every hint clickable), ` Enter keep | Esc clear | ↑↓ select ` while typing a filter; Esc clears selection and filter.
- ➕ Review 2 (user): no `←` / `→` sort keys; PgUp / PgDn / Home / End still work but are listed nowhere (help, readme), as Mac laptop keyboards lack them; `p` does nothing and saves nothing whenever the window has no room for the list, whether it is shown or hidden.

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
- [x] verify all requirements from Overview are implemented (all old metrics visible, terminal palette colors, braille, panels, process list with POWER/GPU) (checked against Layout V3 in render tests and a real run: E/P cluster strips with scaled / active ratio (`r`) and per-core bars (`d`), GPU, RAM / SWAP, CPU / GPU / ANE power with temps and braille history, SYS + fans, total with avg / max, terminal colors only (borders ANSI 8, gradient ANSI 2/3/1 or RGB between the queried colors), panels `1`–`5`, process list with POWER W and GPU % (real run: WindowServer GPU 10 %, Chrome renderer 0.96 W, foreign processes with `-` power); `src_lib`, `pipe` / `serve` / `debug` / `stress` untouched (`main.rs` only gained `mod procs`). ⚠️ per-unit avg / max of the old UI (CPU / GPU / ANE rows, SYS) are not shown: V3 gives those rows a history graph and keeps avg / max on the total only, as designed — left for the user to decide; restored in Task 14)
- [x] verify edge cases: tiny window, no swap, no fans, multi-die, palette query unanswered / no truecolor (covered by tests: `renders_any_size_and_panel_set` down to 1x1, `swap_row_hidden_without_swap`, `fans_and_sys_hidden_when_unavailable`, `core_rows_for_real_chips`, palette query / gradient tests; real binary on a pty: resize to 60x15 auto-hides the process box, 30x8 / 12x4 / 1x1 don't crash, 200x50 brings everything back; unanswered query → ANSI steps, no leaked keys; no `COLORTERM` → no query)
- [x] ➕ skip the palette query in SSH sessions (`SSH_TTY` / `SSH_CONNECTION` set): replies later than ~650 ms leak into the key handler (found in Task 12), and high latency links are where that happens; fall back to ANSI steps there; add a unit test for the skip decision (`palette::should_query(truecolor)`; empty variables count as unset; test `query_skipped_without_truecolor_or_over_ssh`; real run with `SSH_TTY` set: no query bytes, ANSI colors only)
- [x] ➕ process table: one blank cell after the left border too (found in the real run: 5-digit pids, most of them on a running Mac, touched the border as `│64845 macmon`; the right side already had one, the metrics box has one on both sides); the selected row stays reverse video from border to border (test `proc_table_keeps_a_blank_cell_at_both_borders`)
- [x] run full test suite: `make test`
- [x] run `make check`
- [x] run `cargo run --release` manually and walk through every key (automated part: the release binary on a pty with a `pyte` screen, every key checked on screen and in the saved config (`HOME` in the scratchpad): `d`, `r`, `+` / `=` / `-`, `1`–`5`, `s` × 6, `S`, `/` typing with `q` / Backspace / Enter / Esc, ↑ ↓ PgUp PgDn Home End, Esc clears selection then filter, `c` / `v` do nothing, `q` and Ctrl-C (also while typing) exit 0, resize; 86/86 checks. Walking through it in a real terminal by hand: skipped - not automatable, see Post-Completion)

### Task 14: ➕ Restore per-row power avg / max

The user asked at the start to keep every existing label. The old UI showed avg / max for CPU, GPU and ANE power (`CPU 4.20W (3.10, 8.20) 58°C`) and for SYS (`Total 18.30W (17.10, 21.40)`); V3 dropped them (⚠️ found in Task 13).

**Files:**
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/layout.rs` (only if the power column width changes) (➕ it does: `PowerSize`, `STRIPS_MIN_WIDTH`)
- Modify: `src_app/tui/mod.rs` (➕ render tests)

- [x] power rows CPU / GPU / ANE show current W, avg and max (same values as `PowerStore::top_value` / `avg_value` / `max_value`), plus temp for CPU / GPU; the SYS row shows SYS W with avg / max, fans stay on that row or the next one when they don't fit (`CPU   4.50W avg  3.10 max  8.20  45°C`, avg / max `{:>5.2}` with dim labels like the `all` row; rows built from parts (`PowerRow`: head, stats, temp, tail = fans, history); fans stay on the SYS row when `SYS … max 20.00  fan 1200rpm` fits, otherwise get a row of their own)
- [x] widen the power column on wide terminals so the braille history keeps ≥ 8 cells next to the numbers; when space is short drop the graph first, then the temperature, and avg / max last (current W always stays) (➕ `layout::PowerSize { rows, width, min_width, fans_inline }` measured from the rows replaces `Content::power_rows` and `POWER_WIDTH = 30`: the side column is `width` (text + 8 graph cells, or the SYS row with fans if wider) while the strips keep `STRIPS_MIN_WIDTH = 36`, then shrinks down to `min_width` (numbers with avg / max, 31 cells); fans on their own row add a row to the layout; `fit_power` picks the parts for the whole column so they line up, and a dropped graph doesn't come back when temps / avg / max go. Widths: 200 / 120 / 100 → 46 cells with graph, 80 → 37 (temps, no graph), 72 → 31 (no temps); under the strips (< 70) the full inner width, e.g. 60 → 56 with graph)
- [x] keep the `all` row with total avg / max (same columns as the other rows now, two decimals)
- [x] write render tests: avg / max visible for CPU / GPU / ANE / SYS at 200x50 and 120x40; at 80x24 and 60x15 numbers stay and the graph is dropped first; nothing overflows the column or the box (`power_rows_show_avg_and_max` with samples where current / avg / max differ; `narrow_power_column_drops_graph_then_temp_then_stats` at 89, 88, 80x24, 72, 60x15, 48, 44, 40, 34 columns: exact row text per level (no cut numbers), graphs only with everything else, fans inline or not, padding / separator / borders intact; unit tests `power_parts_drop_graph_then_temp_then_stats`, `widest_power_row_decides_for_every_row`, layout tests `power_column_widens_on_wide_screens`, `fans_move_to_own_row_in_narrow_power_column`; existing tests updated to the new rows and widths, `core_rows_for_real_chips` checks the one-line case at 120 instead of 100 since the strips are narrower at 100 now)
- [x] run `make test` and `make check` - must pass before next task

### Task 15: [Final] Update documentation
- [x] update `readme.md`: features list, Controls section, note on process data without sudo (features: per-core load, braille charts, process list, terminal colors, toggleable panels; `c` / `v` gone; Controls split into global keys and process list keys as in the key handlers; settings file and auto-hide noted; "Process data without sudo": CPU % / memory / GPU % for every process, power only for the current user's processes (`-` otherwise, all with `sudo`), CPU % as in Activity Monitor; screenshot left for Post-Completion)
- [x] add entry to `changelog.md` (no "Unreleased" convention in the file: added an `## Unreleased` entry in the same style without a version or date, Full Changelog link `v0.8.2...main`; to be renamed at release)
- [x] move this plan to `docs/plans/completed/` (➕ moved back to `docs/plans/` for Task 16)

### Task 16: ➕ Polish V3 after user review

User review of the running app (M2, ~110x26): the stretched strips left ugly blank space, graphs covered only half the strip, the header clock / interval and the hint line were noise, and the cores row / panel keys were never wanted. Decisions:
- Top box height = its content rows (one row per strip, one per power row); the process list takes all remaining height. No `PROC_HEIGHT_PCT`, no growing strips.
- Graph history long enough to fill the widest strip; avg / max keep the original 128-sample window.
- Header: chip info left, `macmon vX` right — no clock, no interval.
- Hints: original style, right-aligned on the bottom border of the bottom-most box: `q quit | r scaled | -/+ 1000ms`, plus `/ filter | s sort` when the process list is visible. Other keys (`S`, arrows, PgUp/PgDn, Home/End, Esc) keep working but aren't listed.
- Power labels as in the original UI: `Power` = CPU + GPU + ANE (`all_power`), `Total` = system (`sys_power`).
- No cores row and no `d`; no panel keys `1`–`5`. New `p` shows / hides the process list (persisted); metrics are always visible.

Target (100 columns):
```
╭─ M2 · 4E+4P · 10GPU · 24GB ────────────────────────────────────────────────────── macmon v0.8.2 ─╮
│ E-CPU  22% 1.7GHz ⣀⣀⣠⣤⣀⣀⣀⣠⣀⣀⣀⣀⣠⣤⣴⣤⣀⣀⣀⣀⣀⣀⣀⣠⣤⣀⣀  │ CPU    0.00W avg  0.00 max  0.00  43°C ⣀⣀⣀⣠⣀⣀⣀⣀ │
│ P-CPU   6% 1.6GHz ⣤⣀⣀⣀⣠⣀⣀⣀⣀⣠⣤⣴⣤⣀⣀⣀⣀⣀⣀⣀⣠⣤⣀⣀⣀⣀⣀  │ GPU    0.12W avg  0.03 max  0.17  41°C ⣀⣀⣀⣠⣀⣀⣀⣀ │
│ GPU     9% 0.4GHz ⣀⣠⣀⣀⣀⣀⣠⣤⣴⣤⣀⣀⣀⣀⣀⣀⣀⣠⣤⣀⣀⣀⣀⣀⣠⣤⣴  │ ANE    0.00W avg  0.00 max  0.00       ⣀⣀⣀⣠⣀⣀⣀⣀ │
│ RAM    71% 17/24G ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱  │ Power  0.12W avg  0.03 max  0.17                │
│                                                │ Total  7.46W avg  8.77 max 22.85  fan 0rpm      │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ proc 952 ─────────────────────────────────────────────────────────────────────────────── cpu ↓ ─╮
│   PID  NAME                                      USER          CPU%    MEM   POWER   GPU%        │
│   631  WindowServer                              _windowser    47.8   115M       -   14.7        │
│   ...  (all remaining height)                                                                    │
╰───────────────────────────────────────────── q quit | r scaled | -/+ 1000ms | / filter | s sort ─╯
```

**Files:**
- Modify: `src_app/tui/layout.rs`
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/store.rs`
- Modify: `src_app/tui/proc_view.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`
- Modify: `readme.md`
- Modify: `changelog.md`

- [x] top box height = max(strip rows, power rows) + borders; process box gets all remaining height; remove `PROC_HEIGHT_PCT` and spare-row growth; process list auto-hides only when the remaining height is below `PROC_MIN_ROWS` (`compute_layout(area, procs, &Content)`; under the strips (< 70 columns) strips + power rows; one row per strip; a hidden or auto-hidden process list leaves the top box at its content height (blank screen below, no stretching); a screen shorter than the content cuts the rows that don't fit)
- [x] graph history sized to fill the widest possible strip (newest on the right, full width once enough samples exist); `PowerStore` avg / max still over the last 128 samples (`store::HISTORY_LEN = 2048` samples = 1024 braille cells, strips of terminals up to ~1100 columns; `STATS_LEN = 128` for avg / max; the per-core stores went with the cores row, so `ClusterStore` keeps one `FreqStore`)
- [x] header: chip info left, `macmon vX` right; remove the clock and the interval (the version is dropped when it doesn't fit next to the chip info)
- [x] hints: right-aligned on the bottom border of the bottom-most box, ` q quit | r scaled | -/+ 1000ms ` (+ ` / filter | s sort ` when the process list is visible); drop items from the end when narrow, `q quit` always stays (ends with `─╯` like the right title; keys bold, ` | ` dim; `q quit` stays as long as it fits, plain border below 12 columns; the hints don't change while typing a filter; old `draw_hints` / `render_proc_hints` removed)
- [x] power labels `Power` (all_power) and `Total` (sys_power); fans on the Total row or their own row when they don't fit (labels 5 cells, `CPU    4.50W avg …` as in the mockup; order CPU, GPU, ANE, Power, Total; widths: full column 47 cells, min 32, fans inline from 45; at 80 columns the column is 37 cells, so temperatures go there now)
- [x] remove the cores row, `d`, the `per_core` config field and the core-bar code; remove keys `1`–`5` and the `panels` config field; add `p` to show / hide the process list, persisted in config (old configs with `panels` / `per_core` still load) (config field `show_procs`; removed `core_lines` / `CoreLine` / `ClusterCores`, `core_bar`, `CpuFreqStore` / `CoreId`, `Panels`, the "all panels hidden" hint; `ProcView::selected_pid` is test-only now)
- [x] update `readme.md` Controls / features and the `changelog.md` Unreleased entry for the key changes (readme: `p`, no `d` / `1`–`5`, Power / Total explained; changelog: `d` removal under Breaking Changes, `p` instead of panel toggles, long history charts)
- [x] write tests: top box height equals its content rows at 200x50, 120x40, 80x24 and the process box gets the rest; graph fills the full strip width once history is full; header has no clock / interval; hint text and right alignment, narrow-width truncation; `d` and `1`–`5` do nothing; `p` toggles and persists; old configs with `panels` / `per_core` load (plus `matches_target_layout_at_100_columns`: the mockup row by row; layout tests rewritten for content height; `power_stats_cover_latest_samples_only`, `freq_store_keeps_long_history`; real binary on a pty at 110x26 / 100x30 / 60x20 matches the mockup, `p` hides the list and saves `show_procs: false`)
- [x] run `make test` and `make check` - must pass before next task

### Task 17: ➕ Solid block graphs and original power format (variant B)

User compared three drawn variants (comparison page linked in "Graph style") and picked B. Target at 100 columns (same data as the page):
```
╭─ M2 · 4E+4P · 10GPU · 24GB ────────────────────────────────────────────────────── macmon v0.8.2 ─╮
│ E-CPU   26% 1.8GHz  ▃▂▃▃▂▃▅▃▂▂▃▃▂▃▃▆▃▂▃▂▃▃▂▃▅▃▂▂▃ │ CPU    2.94W (3.70, 6.95)  62°C ▃▅▂▃▃▇▃▂▃▃▂▃ │
│ P-CPU   20% 3.5GHz  ▂▁▂▂▅▂▁▂▂▂▁▂▇▂▂▁▂▂▂▄▂▁▂▂▅▂▁▂▂ │ GPU    0.62W (0.51, 0.66)  60°C ▄▄▅▄▄▄▅▄▄▄▄▅ │
│ GPU     20% 0.4GHz  ▂▂▂▂▂▂▂▂▃▂▂▂▂▂▂▂▂▂▃▂▂▂▂▂▂▂▂▂▃ │ ANE    0.00W (0.00, 0.00)                    │
│ RAM     80% 19/24G  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱ │ Power  3.56W (4.20, 7.41)                    │
│ SWAP    57% 1.7/3G  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱ │ Total 11.82W (13.25, 16.42)  fan 1223rpm     │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯
```
The strip text prefix (`E-CPU  26% 1.8GHz `) keeps its current implemented format; only the graph cells change on the left.

**Files:**
- Modify: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/layout.rs` (power column width)
- Modify: `src_app/tui/store.rs` (only if history sizing changes)
- Modify: `readme.md` / `changelog.md` (only where they mention braille)

- [x] graphs: replace braille with solid block bars `▁`…`█` (one sample per cell, 8 levels, a non-zero value gets at least `▁`, zero is blank, newest on the right); each bar colored by its own value on the load gradient (ANSI steps / RGB as before); remove the braille drawing code (`widgets::Graph` draws one row; level = ⌈value · 8 / max⌉ as in the comparison page, in u128 so nothing overflows; color = `gradient(value / max)`, or one color via `Graph::color`; braille dot tables / `dot_level` removed; `HISTORY_LEN` 2048 → 1024 samples, still strips of terminals up to ~1100 columns)
- [x] power rows in the original format `CPU    2.94W (3.70, 6.95)` (label padded to 6, watts right-aligned, parentheses and comma dim), temperature column aligned across CPU / GPU rows, then a block graph in the low color filling the rest of the column (≥ 12 cells at 100 columns); `Power` and `Total` rows numbers only, fans after Total (avg / max as `{:.2}` without padding, so the temperatures start one cell after the widest CPU / GPU / ANE numbers: the gap is part of the temperature part and goes with it; graphs in `gradient(0.0)`, scaled to the largest visible sample)
- [x] power column width sized from the new text; when space is short drop the graph first, then the temperature, then avg / max (current W always stays) (no layout code change: `PowerSize` is measured from the rows, `POWER_GRAPH_MIN` 8 → 12; with the fixture: full column 44 cells (31 text + gap + 12 graph), min 27 (`Total 12.00W (12.00, 12.00)`), fans inline from 40. Widths: 200 / 100 / 87 → 44 with graphs, 86 → 43 without, 80 → 37 with temperatures, 74 → 31 still with them, 72 → 29, 70 → 27 numbers only; under the strips 60 → 56 with a 24-cell graph)
- [x] RAM / SWAP meters unchanged
- [x] write tests: block levels for 0, tiny, 50%, 100% values; per-bar gradient colors (no RGB without palette + truecolor); 100-column render matches the target above row by row (data from the test fixture, layout and widths exact); power format and drop order at 200, 100, 80, 72, 60 columns; nothing overflows (widget: `graph_bar_levels`, right alignment, visible-sample scale, per-bar colors smooth / ANSI, one color, first row only; render: `matches_target_layout_at_100_columns` compares rows 0–6 as whole strings (49-cell strips with the current prefix, separator, 44-cell power column), `strip_bars_follow_their_own_load_power_bars_stay_low` (ANSI and smooth), `power_temperatures_and_graphs_line_up` (12 W CPU, no sensors), dim parentheses / comma, `narrow_power_column_drops_graph_then_temp_then_stats` at 200 … 30 columns, `graphs_are_block_bars` replaces `graphs_are_braille`; whole-frame ANSI-only test unchanged; layout / fit tests updated to the new widths. Real binary on a pty at 110x26 / 100x30 / 60x20 matches the target layout)
- [x] run `make test` and `make check` - must pass before next task

➕ Merged `main` after Task 17 (6656de7): CLPC power fixes — CPU power is real now on this machine; `procs.rs` `IOObjectRelease` aligned to `-> u32` to match `find_clpc.rs` (clashing extern declarations).

### Task 18: ➕ Footer as in the original, process controls in the process box, mouse support

User review: the footer ` q quit | r scaled | -/+ 1000ms | / filter | s sort ` mixed global and process keys and put the interval in the middle; sorting should work by clicking. Target:
```
╭─ proc 631 ─ / filter ────────────────────────────────────────────────────╮
│    PID  NAME                         USER    CPU%  MEM ↓  POWER  GPU%    │
│  21723  com.apple.Virtualization.VM  user    16.5   4.0G  0.32W   0.0    │
│   1998  ghostty                      user     3.9   723M  0.15W   1.1    │
╰─────────────────────────────── q quit | p procs | r scaled | -/+ 1000ms ─╯
```
- Footer (bottom border of the bottom-most box, right-aligned): ` q quit | p procs | r scaled | -/+ 1000ms ` — global keys only, original order, interval last. Same line whether or not the process list is visible.
- Process controls live in the process box: `/ filter` label in its top border (typing shows the filter text there instead, as now); the sort arrow `↓` / `↑` sits next to the active column header instead of in the title.
- Mouse (crossterm mouse capture, enabled with the alternate screen and disabled on exit / panic): click a column header → sort by it, click the active one again → reverse; click `/ filter` → start typing; click a process row → select it; wheel → scroll the list. Clicks outside these targets do nothing. Keys `s` / `S` / `/` and navigation keep working.

**Files:**
- Modify: `src_app/tui/mod.rs` (mouse capture on/off, `Event::Mouse`, dispatch)
- Modify: `src_app/tui/proc_view.rs` (header hit-testing, sort arrow by the header, filter label, row click, wheel)
- Modify: `src_app/tui/panels.rs` (footer text)
- Modify: `readme.md` / `changelog.md` (controls, mouse, text selection note)

- [x] footer ` q quit | p procs | r scaled | -/+ 1000ms ` right-aligned on the bottom-most box, dropping items from the end when narrow (`q quit` stays); remove `/ filter` and `s sort` from it (`render_key_hints(f, area)` without the `procs` flag; all four from 46 columns, three from 33, two from 22, `q quit` from 12)
- [x] process box top border: `proc N` then `/ filter` (or the filter text while typing / when set); sort arrow next to the active column header; remove `cpu ↓` from the title (`/` bold like a hint key, ` filter` plain; also shown while collecting; header `MEM ↓` / `MEM ↑` right-aligned like its numbers, in the title color; ➕ POWER 6 → 7 and GPU% 5 → 6 cells so every header fits its arrow and sorting never moves a column; `ProcSort::label` removed)
- [x] mouse capture on start, off on normal exit and in the panic hook; input thread forwards mouse events; hit-testing uses the rects from the last render (no layout duplicated in the handler) (➕ capture turns on after the palette query, so mouse reports can't mix with its replies; `TERM_ACTIVE` + `TermGuard`: the guard restores on every return from `run_loop` (`?` included, `term.draw` / `enter_term` errors now propagate instead of `unwrap`), the panic hook on panics (release builds abort, so no unwinding); `restore_term_once` runs once whichever comes first, every step even if one fails: all crossterm mouse modes off (1000/1002/1003/1015/1006), main screen, raw mode off. Not covered: SIGTERM / SIGKILL (as before). Input thread forwards only left clicks and the wheel (crossterm's capture reports every move; those would cost a frame each). `Titles::render` / `draw_box` return the cells of the left titles; `ProcView::targets` (box, filter label, header cells via `column_areas`, row area) is set at render and cleared when the panel hides)
- [x] click header → sort by that column (same column → reverse, persisted like `s` / `S`); click `/ filter` → filter input; click row → select that pid; wheel up / down → move the selection / scroll by 3 rows (➕ new `ProcSort::User` so every header sorts; `s` cycle ends with USER; a new column keeps the direction, like `s`; header cells are the whole column width, gaps / padding / borders do nothing; rows react across the box, blank rows below the last process do nothing; wheel only over the process box, moves the selection and the scroll offset by 3 so the selected row keeps its screen row (it moves on screen only at the top / end of the table), without a selection it starts from the top row on screen; clicks act while typing too)
- [x] readme: mouse controls and that terminal text selection needs Option (iTerm2) / Shift (Ghostty, most others) while mouse capture is on; changelog entry (readme: "Mouse (process list)" block, USER in the `s` list; changelog: mouse feature, footer / process controls, restored terminal on errors)
- [x] write tests: footer text / alignment / narrow dropping; header arrow on the active column; click on each header sorts and repeated click reverses; click on filter label enters input; row click selects; wheel scrolls; clicks on borders / metrics box do nothing; mouse events while the process list is hidden do nothing (mod: `key_hints_*`, `click_on_header_sorts_and_again_reverses` (all 7 headers, second click on the arrow cell, saved config), `click_on_filter_label_starts_typing`, `click_on_row_selects_its_process` (scrolled table too), `wheel_moves_selection_and_scrolls`, `clicks_outside_targets_do_nothing` (also right click / release / drag / move), `mouse_does_nothing_while_process_list_hidden` (`p` and auto-hide), `restoring_the_terminal_*` (every mode crossterm turns on goes off, once; every step despite write errors), `input_thread_forwards_clicks_and_wheel_only`; proc_view: arrow fits every column, `column_areas`, mouse on synthetic targets, wheel math, `clear` forgets targets, USER sort; panels: `titles_return_the_cells_of_their_text`; existing tests updated to the new title / header / widths. Real binary on a pty (pyte, SGR mouse reports): 31/31 — capture on after the palette query, footer, title, header clicks / reverse / saved config, moves and right clicks ignored, row click, wheel scroll keeps the selected row, wheel over metrics ignored, filter click + typing, `p` hides and clicks do nothing, `q` and Ctrl-C (while typing) exit 0 with every mouse mode off before the main screen returns)
- [x] run `make test` and `make check` - must pass before next task

### Task 19: ➕ Proportional boxes: original macmon metric boxes over the process list

User review on a wide (~250 column) terminal: one-row strips stretch into long threads (RAM / SWAP meters 170 cells long) and look broken; width breakpoints were rejected as "guessing the zoom". The original macmon scales fine because its boxes grow in both directions. Decision: bring back the original metric boxes, compressed into the top part of the screen, process list full width below. One structure at every size, only the scale changes. Mockup rendered at 200×50 and 110×32 on a page shown during the review (not public). The strips version is commit 1ca90ed in this branch's history.

```
╭ Apple M2 (4E+4P+10GPU 24GB) ───────────────────────────────────────────────────── macmon v0.8.2 ╮
│╭ E-CPU 20% @ 1640 MHz ──╮╭ P-CPU 26% @ 3500 MHz ──╮╭ GPU 3% @ 444 MHz ──────╮╭ RAM 20.11 / 24.0 GB (83.8%) ╮│
││        ▁▂▂▃▂▂▃▂▂▃▅▃▂▂▃││         ▃▂▅▃▂▇▅▃▂▃▅▂▃││              ▁▁▁▁▁▁▁▂▁││ ▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇││
│╰────────────────────────╯╰────────────────────────╯╰───────────────────────╯╰────────────────────────────╯│
│╭ CPU 3.44W (4.02, 6.77) ───────── 56.5°C ╮╭ GPU 0.08W (0.12, 0.25) ──── 54.1°C ╮╭ ANE 0.00W (0.00, 0.00) ──╮│
││          ▂▃▅▃▂▇▅▃▂▂▃▅▃▂▃                  ││           ▁▁▂▁▁▅▂▁▁▁▁            ││                          ││
│╰─────────────────────────────────────────╯╰─────────────────────────────────────╯╰──────────────────────────╯│
╰ Power: 3.52W (avg 4.14W, max 6.89W) | Fan 1196 RPM | Total 10.07W (11.92, 15.21) ──────────────────────────╯
╭ proc 656 ─ / filter ──────────────────────────────────────────────────────────────────────────────────────╮
│ … process table as now …                                                                                  │
╰──────────────────────────────────────────────────────────── q quit | p procs | r scaled | -/+ 1000ms ─────╯
```
(widths in this sketch are approximate; the mockup page was the reference)

- Metrics area = top `METRICS_HEIGHT_PCT = 40` % of the height (process list keeps ~60 %, the user's earlier choice). Hidden (`p`) or auto-hidden process list → the metrics area takes the full height, exactly like the original app.
- Outer box: `Apple M2 (4E+4P+10GPU 24GB)` left (original title format), `macmon vX` right; bottom border left: original power summary `Power: 3.52W (avg 4.14W, max 6.89W)` + `Fan 1196 RPM` + `Total 10.07W (11.92, 15.21)` (only parts whose sensors exist). When the metrics box is the bottom-most box, the key hints share that border right-aligned; titles never overlap (existing title-fitting rules: hints / summary parts drop from the end).
- Row 1: one box per CPU cluster (generic N: 2 today, 3 on M6), then GPU, then RAM. Titles in the original format: `E-CPU 20% @ 1640 MHz`, `GPU 3% @ 444 MHz`, `RAM 20.11 / 24.0 GB (83.8%)` with `SWAP 2.35 / 3.0 GB` as a right title when swap exists and it fits.
- Row 2: CPU, GPU, ANE power boxes: `CPU 3.44W (4.02, 6.77)` + right `56.5°C` (temperature only when available); ANE without temperature.
- Boxes split the width evenly; the inner height splits between the two rows (row 1 gets the extra row). Each box: graph fills the whole inner area.
- Graphs: multi-row solid bars `▁`…`█`, one sample per column, newest on the right, each column colored by its own value on the load gradient (CPU / GPU / RAM by ratio; power graphs in the low color, auto-scaled to the visible max like the original).
- Titles that don't fit: right title drops first, then the left title is truncated.
- Remove the one-row strips, the power column, `PowerSize` / `fit_power`, the strip prefix code and `Meter` if nothing uses it any more. Keep: palette / theme, process sampling, process view, mouse, footer, `p`, `r`, `-`/`+`.

**Files:**
- Modify: `src_app/tui/layout.rs`
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/store.rs` (only if history / stats need it)
- Modify: `src_app/tui/mod.rs`
- Modify: `readme.md` / `changelog.md`

- [x] layout: metrics area = 40 % of the height on top, process box below with the rest; full height for metrics when the process list is hidden or auto-hidden; two rows of boxes inside the metrics box, widths split evenly, heights split between rows (`compute_layout(area, procs, clusters)` → `LayoutPlan { top, boxes: Vec<(Metric, Rect)>, proc }`; `METRICS_HEIGHT_PCT = 40`, rounded, ➕ at least `METRICS_MIN_HEIGHT = 8` rows (two rows of boxes with one graph row each, as the mockup's `max(8, …)`); the process list auto-hides when it would get fewer than `PROC_MIN_ROWS` process rows; boxes tile the inner width (odd cells spread), no padding inside the outer border, the top row gets the odd row; `LayoutPlan::bottom()` removed. 200x50 → 20 rows of metrics, 110x32 → 13, 80x24 → 10, 60x15 → 8; 60x12 / 200x13 auto-hide)
- [x] metric boxes in the original title format (clusters generic over N, GPU, RAM with SWAP right title; CPU / GPU / ANE power with temperatures); outer box titles and the power summary on its bottom border (strings exactly as in 98010ed: `E-CPU  42% @ 1800 MHz` (`{:3.0}%`, `{:4} MHz`, so 1–2 digit values are padded as in the original, unlike the plan's examples), `RAM 20.00 / 36.0 GB (55.6%)` + right `SWAP 1.00 / 2.0 GB`, `CPU 4.50W (4.50, 4.50)` + right `45.0°C`, outer `Apple M3 Pro (6E+6P+18GPU 36GB)` (core counts from the clusters) + `macmon vX`, bottom border ` Power: 6.60W (avg 6.60W, max 6.60W) | Fan 1200 RPM | Total 12.00W (12.00, 12.00) ` in the original order (`FanStore::label` back to `Fan N RPM` / `Fans a/b RPM`); styles as the mockup: names bold, percent / temperature on the gradient, avg / max and the chip details dim; `MemoryStore` keeps a RAM history for the RAM graph)
- [x] multi-row solid bar graph widget (one sample per column, eighths per row, per-column gradient color or one color); remove the one-row strip / power-column code and anything left unused (`Graph` fills its whole area: level = ⌈value · rows · 8 / max⌉ eighths from the bottom row up, every cell of a column in the column's color; `max` / `color` take an `Option`; CPU / GPU percent with max 100, RAM bytes with max = total, power auto-scaled to the visible max in `gradient(0.0)`; removed `Meter`, `Strip`, `PowerSize`, `Content`, `PowerRow` / `PowerWidths` / `PowerFit` / `fit_power`, strip text / label / detail code, `format_ghz` / `format_gb`, the separator column)
- [x] key hints and power summary share the bottom border without overlap when the process list is hidden (`share_border`: `q quit` is placed first, then the summary takes the room it needs (parts drop from the end, a lone Power part is cut), then the other hints get what is left (dropped from the end); one border cell between them, two at each corner; the process box uses the same code with no summary)
- [x] readme / changelog: describe the layout (original boxes + process list), drop the strip wording (readme: interactive mode paragraph and the layout feature line; changelog: layout entry, charts fill their box, power summary on the bottom border)
- [x] write tests: layout proportions at 200x50, 110x32, 80x24 (metrics ≈ 40 %, process box the rest, boxes inside the area, no overlap); `p` / auto-hide → metrics full height; box count follows clusters (2 and 3); original title strings; right title dropped / left truncated in narrow boxes; graph levels across rows and per-column colors; footer + power summary on one border at narrow widths; existing process / mouse tests still pass (layout: proportions, exact box rects, 3 clusters, hidden / auto-hidden, inside / no overlap / rows tile the width over a size grid; widget: levels across 2–3 rows, per-column colors, one color, area clipping; panels: `share_border` drop order and no overlap at every width 0–200; render: `matches_mockup_layout_at_110_columns` row by row, `renders_original_titles`, `power_boxes_show_current_avg_max_and_temperature` (styles too), `narrow_boxes_drop_right_title_then_cut_left`, `power_summary_follows_sensors`, `footer_and_power_summary_share_the_border`, `graph_columns_follow_their_own_load_power_graphs_stay_low`, `graphs_fill_boxes_once_history_is_long_enough`; process / mouse tests moved to the new geometry (process box at row 20 of 200x50, 27 rows; auto-hide at 60x12). Real binary on a pty at 110x32 (and `p`), 80x24, 200x50, 60x15 matches the mockup's structure)
- [x] run `make test` and `make check` - must pass before next task

⚠️ Found in the real run (left for the user to decide): with the original title strings and evenly split boxes, typical widths cut titles. At 110 columns the RAM box (27 cells) shows `RAM 20.03 / 24.0 GB (8` (the percent is cut) and the power boxes (36 cells) are one cell short of the temperature, so `°C` shows from ~113 columns; `SWAP …` needs a 54-cell RAM box (~218 columns); at 80 columns the cluster titles read `E-CPU   7% @ 1`. The mockup avoided this with shorter titles (`RAM 20.1/24.0 GB 84%`, `57°C`, unpadded `E-CPU 20% @ 1640 MHz`). → resolved in Task 20.

### Task 20: ➕ Titles that fit, gauge view on `v`, RAM graph scale

User review of Task 19: the RAM title doesn't fit (total memory is already in the outer title `… 24GB`, so drop it), and the old interface had a gauge view on `v` — bring it back since the top block is the original one again. A screenshot also showed the RAM box completely filled at 70 % usage.

- RAM title: no total (it's in the outer title). ~~Right title `SWAP 2.35 / 3.0 GB` when swap exists and it fits.~~ ➕ User change during Task 20: SWAP must stay visible as long as possible, so RAM and SWAP share ONE left title that degrades in steps, percentages last: `RAM 16.81 GB (70.0%) · SWAP 2.35 / 3.0 GB` → `RAM 16.8G 70% · SWAP 2.4G 79%` → `RAM 70% · SWAP 79%` → `RAM 70% SW 79%` → cut. Without swap: `RAM 16.81 GB (70.0%)` → `RAM 16.8G 70%` → `RAM 70%` → cut. Separator ` · ` dim, percentages on the load gradient; the longest step that fits the top border wins.
- Titles degrade by whole parts instead of cutting mid-text:
  - cluster / GPU boxes: `E-CPU 42% @ 1800 MHz` → `E-CPU 42%` (frequency drops first; label + percent always stay; no alignment padding inside the title);
  - RAM: ~~`RAM 16.81 GB (70.0%)` → `RAM 70.0%`; the SWAP right title drops before anything on the left~~ the steps above;
  - power boxes: `CPU 3.44W (4.02, 6.77)` + `57°C` → temperature drops first, then `(avg, max)`; current W always stays. Temperatures as whole degrees (`57°C`).
  - only if even the minimal part doesn't fit, cut it (as now).
- `v` toggles the cluster / GPU / RAM boxes between graph and gauge, like the original: gauge = a horizontal bar across the whole inner area filled to the ratio, colored by the load gradient of that ratio, empty part blank. Power boxes always stay graphs (as in the original). Persisted in config as `view_type`; the old values `"Sparkline"` (→ graph) and `"Gauge"` are accepted again, so an old config restores the user's old choice. Footer: ` q quit | p procs | v chart | r scaled | -/+ 1000ms ` (original order with `v chart`; `p procs` after `q quit`).
- RAM graph must scale to `ram_total` (as the original `.max(val.ram_total)`), not to the visible maximum; SWAP is not graphed. Verify the other load graphs scale to 100 % and only power graphs auto-scale.

**Files:**
- Modify: `src_app/tui/panels.rs`
- Modify: `src_app/tui/widgets.rs`
- Modify: `src_app/tui/mod.rs`
- Modify: `src_app/config.rs`
- Modify: `readme.md` / `changelog.md`

- [x] RAM title without total; ~~SWAP right title~~ SWAP in the same left title (➕ user change); temperatures as whole degrees (`ram_part` / `swap_part` build each step; `RAM 20.00 GB (55.6%) · SWAP 1.00 / 2.0 GB` with the test metrics; names bold, ` · ` dim, percents on the gradient; `45°C`)
- [x] title parts with priorities: drop whole parts (frequency; GB value; temperature; avg / max; right titles first) before cutting text (`MetricBox::titles` is a list of `Titles` variants, longest first; `fit_titles` picks the first that `Titles::fits` uncut (`place_titles` places every title at full width), else the last one, which `Titles::render` cuts as before. Cluster / GPU: `E-CPU 42% @ 1800 MHz` → `E-CPU 42%` (no padding, `{:.0}%`, `{} MHz`); power: + `45°C` → without temperature → `CPU 4.50W`; RAM: the 4 / 3 steps. A step fits from its text + 6 cells (+ the right title + 3): E-CPU 26, power 35 / 28 / 15, RAM with swap 47 / 35 / 24 / 20, without 26 / 19 / 13. At 110 columns: `E-CPU 42% @ 1800 MHz`, `RAM 77% · SWAP 64%` (27-cell boxes), power with temperatures (36 cells); at 80: `E-CPU 16%`, `RAM 77% SW 64%`, `CPU 1.93W`)
- [x] `v` graph / gauge for cluster, GPU and RAM boxes; power boxes always graphs; `view_type` in config with old values accepted; footer with `v chart` (`config::ViewType { Graph, Gauge }`, `Graph` saved as `"Sparkline"` so released versions and their configs agree both ways; an unknown `view_type` (`Braille` / `Block` of earlier redesign builds, garbage) falls back to the graph without resetting the other settings; `widgets::Gauge`: every row of the box filled from the left to `round(width · ratio)` cells of `█` in `gradient(ratio)`, the rest blank, as the original's non-unicode `Gauge`; `MetricBox::gauge` = the current load, `None` for power boxes; footer ` q quit | p procs | v chart | r scaled | -/+ 1000ms `, all five from 56 columns)
- [x] RAM graph scaled to `ram_total`; load graphs to 100 %; power graphs auto-scale (⚠️ cause of the "full RAM box" not found in the code: the RAM graph has been scaled to `ram_total` since Task 19 (`max: Some(mem.ram_total)`), cluster / GPU to 100, power to the visible maximum; a real run at 69.8 % RAM drew 23 of 32 eighths in a 4-row graph. What can make it look full: RAM barely changes, so a full history is a solid block, and bars round up to the next eighth, which shows most in short boxes (70 % → `▆` in a 1-row graph, `▄` over `█` in 2 rows). Kept as is; locked by `load_graphs_scale_to_full_load_power_graphs_to_their_peak`)
- [x] readme / changelog: `v` is back, RAM title change (readme: `v` in Controls (keys in footer order), chart view in the saved settings, gauges / RAM title / shortened titles in the interactive mode paragraph; changelog: `v` / `view_type` no longer under Breaking Changes, entries for `v`, the RAM title, title steps and whole degrees, footer with `v chart`)
- [x] write tests: RAM title strings at several widths; drop order for each box kind at shrinking widths (no mid-text cut while a smaller variant fits); gauge fill width / color at 0, 50, 100 % and box sizes; `v` toggles and persists, old configs with `view_type: "Sparkline"` / `"Gauge"` load; RAM graph at 70 % of total fills ~70 % of the height; footer text (mod: `ram_title_steps_down_with_swap` / `_without_swap` (each step at its first and last width, then cut), `ram_title_styles`, `titles_step_down_before_they_are_cut` (every box width 6–60 for E-CPU, P-CPU, GPU, RAM with / without swap, CPU and ANE power against a test oracle), `power_and_cluster_titles_at_their_step_widths`, `v_switches_load_boxes_to_gauges` (fill / color per box at 200x50, fill at 110x32 / 80x24 / 60x15 / 400x120, power boxes unchanged, saved `"Gauge"` / `"Sparkline"`, back to the same frame), `load_graphs_scale_to_full_load_power_graphs_to_their_peak` (RAM 70 % after 60 %, E-CPU, GPU at graphs 1–22 rows tall: within one eighth above the load and never full; constant power full); footer / hint tests for five hints; existing title tests moved to the new strings; widgets: `gauge_fills_its_ratio_of_every_row`, `gauge_clamps_ratio_and_stays_inside_its_area`; panels: `titles_fit_only_uncut`, `fit_titles_picks_the_longest_variant_that_fits`, `share_border` cases for five hints; config: `view_type_uses_released_names`, `toggle_view_type_switches_graph_and_gauge`, old `"Gauge"` kept. Real binary on a pty at 110x32 / 80x24 / 60x15: title steps as listed above, `v` → gauges and back, `view_type: "Sparkline"` saved)
- [x] run `make test` and `make check` - must pass before next task

### Task 21: ➕ Usability pass (UX review findings, ←/→ sort, `?` help, selected process path)

A usability review drove the real binary on a pty at 24x8 … 200x50 (report: scratchpad, summarized here). The user asked for keyboard sort selection (←/→ like btop), approved a `?` help overlay, and chose "show the selected process's full path on the process box border" over a PATH column.

User decisions:
- ~~`←` / `→` move the sort to the previous / next visible column (the header arrow moves with it); `s` / `S` keep working. A hint `← sort →` sits in the process box top border after `/ filter` (clickable like `/ filter`: clicking `←`/`→` moves the sort).~~ ➕ Review 2 (user): no `←` / `→` sort keys and no `← sort →` hint; sorting is `s` / `S` and header clicks, with a clickable `s sort` hint (presses `s`) after `/ filter`, left out while a filter is typed (`s` is text then).
- `?` opens a help overlay (centered box over the screen, Esc / `?` / `q` closes it; `q` closes the overlay instead of quitting while it is open). ➕ Review 2 (user): trimmed to the essentials: Keys (q, p, v, r, -/+, /, s / S, ↑↓, Esc, ?), one Mouse line (click a header to sort, a row to select, a hint to press it), Notes (CPU% 100% = one core, scaled vs active, POWER `-`, text selection); no paging keys, wheel or filter-typing details, no MEM note; 20 rows, so it fits 80x24 and the `↑↓ scroll` title hint is gone (shorter windows still scroll with ↑↓ / the wheel). ~~Content: every key and mouse action grouped (global / process list / filter typing), plus short explanations: CPU% = 100% per core (Activity Monitor convention); `scaled` vs `active` ratio; POWER `-` = another user's process (per-process power is readable only for your own processes, run with sudo for all); MEM = physical footprint for your processes, RSS for others; text selection needs Option (iTerm2) / Shift (Ghostty, most terminals) while the mouse is captured.~~ Footer gets `? help` (after `q quit`).
- Selected process: its PID and full executable path are shown on the bottom border of the process box, left side (path cut from the left with `…` when long), together with `Esc` to clear. Nothing selected → nothing there. (➕ Review 2: `Esc clear` is a clickable hint, left out while a filter is typed, when Esc clears the filter.)

Fixes from the review (MAJOR first):
- `p` while the process list is auto-hidden (window too small): do nothing and don't save; the footer drops `p procs` while auto-hidden. ~~A hidden-by-`p` list stays a saved preference as now.~~ ➕ Review 2: the same both ways: whenever the window has no room for the list (`App::procs_fit`, from `layout::procs_fit`), shown or hidden by `p`, `p` does nothing and saves nothing, and the footer leaves `p procs` out.
- POWER `-` explained on screen: when not root, the process box bottom border (left, when nothing is selected) shows a dim note `POWER: own processes only`; when sorted by POWER the `-` rows stay last (as now). Help overlay explains it too.
- Selection drift: the wheel scrolls the view without creating a selection; PgUp/PgDn/Home/End without a selection scroll too (selection is created only by ↑/↓ or a click); a click on the selected row clears the selection; when the selected PID disappears or is filtered out, the selection is dropped (no fallback to a neighbour row).
- Filter with no matches: centered dim `no process matches "<text>"` in the table area.
- While typing a filter the footer shows ` Enter keep | Esc clear | ↑↓ select ` instead of the global hints.
- Long filter text: cut from the left (`/…irtualiza█`) so the cursor and the end of the text stay visible; never drop the filter title while typing.
- Esc in normal mode clears both the selection and the filter.
- Sort direction when a column is first chosen (by `s`, ←/→ or click): numeric columns (CPU%, MEM, POWER, GPU%) start descending, NAME / USER / PID ascending; choosing the active column again reverses as now.
- The sorted column is never dropped on narrow widths (drop another column instead); `s` and ←/→ skip columns that are not visible.
- RAM title: add final steps `RAM 77%` and `77%` before any cut; never cut inside a number. Use `SWAP` instead of `SW` if it fits, `SW` only as the last swap variant.
- Power boxes: add a variant with the temperature kept (`CPU 4.70W` + right `49°C`) between the full title and `CPU 4.70W`, so temperatures stay visible at 80 columns.
- Toggle hints show state consistently: `v graph` / `v gauge`, `r scaled` / `r active`, `-/+ 1000ms`.
- Footer hints are clickable (same targets as the keys), like `/ filter` and the headers.
- With the process list hidden, the bottom border drops the power summary's `(avg, max)` / `Fan` parts before dropping hints.
- Other users' CPU% (from `ps`, 10 ms resolution): average over the last 3 samples and skip the warm-up value for those rows, so idle daemons don't jump in 1% / 4% steps.
- Interval from `-i` is not saved to the config; only `-`/`+` changes are saved.
- NAME: add `…` when cut; drop USER before NAME would go below 16 cells.
- Mouse capture off while the process list is hidden (nothing to click), on again when it shows.
- readme: kernel_task is not listed without sudo; MEM footprint vs RSS; POWER own processes only.

**Files:**
- Modify: `src_app/tui/mod.rs`, `src_app/tui/proc_view.rs`, `src_app/tui/boxes.rs`, `src_app/tui/layout.rs` (if needed), `src_app/procs.rs` (ps CPU averaging), `src_app/config.rs` / `src_app/main.rs` (interval not saved from `-i`)
- Create: help overlay code (in `boxes.rs` or a new `src_app/tui/help.rs`)
- Modify: `readme.md`, `changelog.md`

- [x] ←/→ sort keys + clickable `← sort →` hint; direction defaults per column; sorted column never dropped; hidden columns skipped (`ProcView::move_sort` wraps around the columns on screen (`targets.headers`; all columns before the first render); ←/→ work while typing too, as ↑↓ do; `s` skips columns not on screen; `Column::sorts_desc`: CPU / MEM / POWER / GPU descending, PID / NAME / USER ascending when newly chosen by `s`, ←/→ or a click; `fit_columns(width, sort)`; the hint is a third left title, dropped first and while a long filter fills the border; clicks on it go through the app's key targets: left half `←`, right half `→`) (➕ Review 2: ←/→ and `move_sort` removed; `s sort` hint instead, `Column` alias gone, `ProcSort` throughout)
- [x] `?` help overlay with keys, mouse and explanations; `? help` in the footer (new `tui/help.rs`: sections Keys / Process list / Typing a filter / Mouse / Values, 30 lines, a box under 80 columns, centered over `Clear`; title `help`, right `Esc close`; shorter windows scroll with ↑↓ / the wheel and show `↑↓ scroll`, clamped at render; while open every key goes to it (Esc / `?` / `q` close, other keys do nothing, Ctrl-C quits), a click closes it; `?` while typing a filter is text; `App::help: Option<usize>` = scroll) (➕ Review 2: sections Keys / Mouse / Notes, 18 lines, no `↑↓ scroll` title, `Esc close` built with `Hint`)
- [x] selected process PID + path on the bottom border (left), POWER note when nothing is selected and not root (`ProcInfo::path` from `proc_pidpath`, cached with the name; empty → the name; `631 /System/…/WindowServer | Esc clear`, PID bold; the path gets the room left by all hints (cut from the left with `…`), at least half the room next to `q quit` (then hints drop), so the footer doesn't jump between processes; `Esc clear` drops first. ➕ The note shows when the POWER column is on screen and some processes have power and some don't (instead of a root check: as root every process has it, before macOS 13 none); it drops before any hint)
- [x] selection fixes: wheel / paging without creating a selection, click on selected row clears it, selection dropped when its PID disappears or is filtered out; Esc clears selection and filter (without a selection ↑/↓ select the top row on screen and PgUp / PgDn / Home / End / the wheel scroll (`scroll_to`, `scroll_offset` keeps the offset without a selection); Esc also scrolls back to the top)
- [x] filter fixes: no-match message, typing footer, long text cut from the left (dim `no process matches "zz"` centered in the table body, also after Enter while nothing matches; typing footer hints are clickable too; cutting from the left was already in place since Review 1, now also with the sort hint after the filter)
- [x] `p` ignored and not saved while auto-hidden; footer drops `p procs` then; mouse capture off while the list is hidden (~~`App::procs_auto_hidden`; a list hidden with `p` can still be shown in a small window, as a setting;~~ ➕ Review 2: `App::procs_fit`, set before each frame is drawn; `p` is ignored both ways while the list can't fit; `set_mouse_capture` after each frame follows `procs_visible()`; `handle_mouse` ignores events while the list is hidden, so footer hints are clickable only with the list on screen)
- [x] title fixes: RAM final steps without cutting numbers; power variant keeping the temperature; state-style toggle hints; clickable footer hints; summary parts drop before hints (RAM with swap: ... `RAM 70% · SWAP 79%`, `RAM 70% SWAP 79%`, `RAM 70% SW 79%`, `RAM 70%`, `70%`, then no title (fits from 22 / 20 / 13 / 9 cells); power: full + temp, full, `CPU 4.50W` + temp (from 22 cells), `CPU 4.50W`; footer `v graph` / `v gauge`; `boxes::Hint` (keys, label, key codes) and `KeyTarget` (~~cells + keys, split evenly between the keys, so `-/+` is `-` on its left half~~ ➕ review fix: cells + the cells of each key's symbol; a click presses the key of the symbol clicked or the nearest one, the left one on a tie, so `-/+ 1000ms` is `-` on `-/` and `+` from the `+` on, `↑↓ select` is `↑` only on `↑`; `Hint::pair` for two keys); `render_bottom_border` takes summary variants and returns the hint targets; `fit_bottom` picks the first variant that fits whole next to every hint, else the last one shared as before. Power summary variants: full, without avg / max, without the fans; ➕ also used without hints (process list shown), so at 80 columns the metrics box shows `Power: 6.60W | Fan 1200 RPM | Total 12.00W` instead of dropping Total)
- [x] `ps` CPU averaging over 3 samples, warm-up skipped for those rows; `-i` not saved (`Raw::ps`; `Known::cpu_history` keeps `(clock, CPU time)` of the last 3 ticks; `averaged_cpu_pct` is 0 until two snapshots exist, so the 250 ms warm-up rate is skipped, then averages since the oldest; libproc rows unchanged. `Config::run_interval` (not serialized) holds `-i`; `interval()` is the one in use; `-` / `+` step from it, clear it and save; `interval` field private)
- [x] NAME `…` and USER dropped before NAME < 16 (`DROP_ORDER` pairs each column with the NAME width it keeps: USER 16, the others 8; `cut_end` in `draw_row` for NAME and USER, e.g. `_windowse…`)
- [x] readme / changelog updates (help, ←/→, kernel_task, MEM, POWER note) (readme: Controls / Process list / Mouse blocks, sort directions, selected path, `-i` for one run, mouse capture only with the list, POWER note, `ps` averaging, `kernel_task` only with sudo; changelog: features (help, ←/→, bottom border), improvements, fix for `-i`; agents.md: `help.rs`)
- [x] write tests for each item above (keys, mouse targets, overlay open/close and `q` while open, footer variants, selection rules, filter messages, column dropping, title variants, ps averaging, interval not saved) (mod: `help_opens_over_the_screen_and_closes`, `help_scrolls_when_the_window_is_short`, `arrows_and_the_sort_hint_move_the_sort`, `footer_hints_press_their_keys`, `selected_process_and_its_path_on_the_bottom_border`, `power_note_gives_way_and_follows_the_power_column`, `typing_a_filter_changes_the_footer_and_says_when_nothing_matches`, `p_is_ignored_while_the_window_is_too_small`, `mouse_capture_turns_on_and_off_once`; footer / title / header / wheel / click tests moved to the new rules; proc_view: `arrows_move_the_sort_over_the_columns_on_screen`, `sorted_column_is_never_dropped`, `esc_clears_selection_and_filter`, `selection_is_dropped_when_its_process_exits`, `cut_text_ends_with_an_ellipsis`, column drop widths; boxes: `bottom_border_drops_summary_details_before_hints`, `text_room_keeps_the_hints_while_it_can`, `summary_room_leaves_the_corners_and_q_quit`, `key_targets_split_between_their_keys`; help: keys fit their column; procs: `ps_rows_average_cpu_over_three_intervals`, `averaged_cpu_cases`, paths; config: `interval_from_the_command_line_is_not_saved`. Real binary on a pty at 110x32 / 80x24 / 80x12: sort hint, POWER note, selected path with 5 hints, help overlay (scrolling at 80x24, `q` closes then quits), ←/→ and hint click, typing footer and no-match message, `?` typed into a filter, `-i 500` shown but 1000 saved, `-` click saved 250, `p` ignored when auto-hidden, mouse capture on / off / on as the list hides and shows)
- [x] run `make test` and `make check` - must pass before next task

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- Ghostty / iTerm2 / Apple Terminal / inside tmux: palette query answered vs not (smooth vs stepped gradient), no stray characters from late replies, light and dark terminal themes.
- Apple Terminal: graph bars in three levels (blank / `▄` / `█`) without gaps between rows.
- `cargo run --release` in a real terminal: walk through every key (Task 13 drove them only on a pty with an emulated screen).
- Over SSH: stepped gradient, no palette query, no stray characters.
- Small window (e.g. 60x15) and huge window; resize while running.
- M-series with many cores (Max/Ultra); Mac without fans (MacBook Air).
- Compare CPU% / MEM / GPU% for a few processes with Activity Monitor.
- CPU overhead of macmon itself with proc panel on vs off.

**Release**:
- new screenshot for `assets` branch / README.
- Phase 2 (separate plan): kill / signals, process tree, details on Enter.

**Library follow-up** (separate plan):
- M6 has three CPU tiers (6E + 4P + 2S). The library exposes only two clusters (`ecpu_*` / `pcpu_*`): `cpu_tier_counts` reads perflevel0 and the last perflevel only, and `MCPU` channels are classified as the E slot, so on M6 the P and E tiers most likely merge into one cluster with a wrong label. Needs a verified fix on real M6 hardware and a public API for N clusters; the TUI metric boxes are already generic over clusters.
- Per-process watts looked low in a spot check (ghostty ~20% CPU → ~0.06 W): compare `ri_energy_nj` against Activity Monitor / `powermetrics --show-process-energy`.
