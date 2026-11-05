# macmon

Rootless Apple Silicon monitor and Rust library. Sampling and metric types live in `src/metrics.rs`, macOS API access in `src/sources.rs`, the CLI in `src/app/` and the TUI in `src/app/tui/`. Per-process usage (`src/app/procs.rs`) comes from libproc for the user's own processes, the setuid `/bin/ps` for other users' and GPU time from the IORegistry (`IOAccelerator` children). Planned work is in [docs/roadmap.md](docs/roadmap.md), not GitHub issues; research notes are in `docs/`.

## Boundaries

- Break JSON or Prometheus output, the library API or `~/.config/macmon.json` (same field names and values, `view_type` is `"Sparkline"` / `"Gauge"`), or add a dependency, only with the person's approval. Change dependencies only with `cargo add` / `cargo remove` / `cargo upgrade`, never by hand.
- Keep CLI-only code and dependencies behind the `app` feature, so the library builds with `--no-default-features`.
- Keep metric collection rootless, with fallbacks for older systems.
- Everything reaches `main` as a squash-merged PR.

## Code

- TUI colors only through `src/app/tui/theme.rs`, as terminal ANSI colors (no RGB), so macmon follows the terminal's theme.
- Release builds abort on panic: undo every terminal mode the TUI enables in `restore_term_once`.
- The TUI saves settings on key presses: set `HOME` to a temporary directory for manual runs.
- Before investigating or changing power sources, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and [docs/clpc-discovery.md](docs/clpc-discovery.md). Zero CPU/ANE readings may be unavailable counters, not zero consumption. Verify CLPC report IDs, CPU/GPU/ANE assignments and units against an independent reference under separate component loads; table positions and response to load alone prove nothing.
- A counter missing from one source may be in another: check `Energy Model`, `PMP`, CLPC and SMC before calling it unavailable. A zero that never moves means the source doesn't work. `powermetrics` doesn't show DRAM.
- New metric fields are optional, `null` without a source, and appear in `macmon debug`. Existing fields keep their names and read 0.
- Validate sampling changes with `macmon pipe` on the affected hardware and macOS version, and report runtime results apart from build and test results, naming untested configurations.
- Keep a PR under ~200 production lines (`make loc REF=origin/main`).

## Workflow

- `make check` and `make test` pass before every commit.
- Commits and PR titles are one line in the history's style (`feat: …`, `fix: …`, `perf: …`, `docs: …`, `chore: …`), no body. The PR body says what changed, its compatibility (JSON, Prometheus, library API, config), the checks run, and what was and wasn't tested by hand.
- Multi-step work follows a plan in `docs/plans/yyyymmdd-<name>.md`, agreed with the person: behavior, decisions and a checklist, with only its boxes ticked during the work. The PR that finishes it adds `Done: yyyy-mm-dd, PR #N` as the first line and a short `## Result`, moves it to `docs/plans/completed/` and ticks its roadmap item; never delete a plan.
- Docs are only `readme.md`, the rustdoc, `docs/roadmap.md`, `docs/plans/` and the research notes in `docs/`. `changelog.md` changes only in the person's `release: vX.Y.Z` PR.
