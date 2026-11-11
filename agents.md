# macmon

Rootless Apple Silicon monitor and Rust library. Sampling and metric types live in `src/metrics.rs`, macOS API access in `src/sources.rs`, the CLI in `src/app/` and the TUI in `src/app/tui/`. Per-process usage (`src/app/procs.rs`) comes from libproc for own processes, the setuid `/bin/ps` for other users' and the IORegistry (`IOAccelerator` children) for GPU time. [docs/roadmap.md](docs/roadmap.md) is the maintainer's backlog of ideas: change it only when the person asks.

## Boundaries

- Break JSON or Prometheus output, the library API or `~/.config/macmon.json` (same field names and values, `view_type` is `"Sparkline"` / `"Gauge"`), or add a dependency, only with the person's approval. Change dependencies only via `cargo add` / `remove` / `upgrade`.
- Keep CLI-only code and dependencies behind the `app` feature, so the library builds with `--no-default-features`.
- Keep metric collection rootless, with fallbacks for older systems.
- Everything reaches `main` as a squash-merged PR.

## Code

- TUI colors only through `src/app/tui/theme.rs`, as ANSI colors (no RGB), to follow the terminal's theme.
- Release builds abort on panic: undo every terminal mode the TUI enables in `restore_term_once`.
- The TUI saves settings on key presses: set `HOME` to a temporary directory for manual runs.
- Before power-source work, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and [docs/clpc-discovery.md](docs/clpc-discovery.md). Zero CPU/ANE readings may be unavailable counters, not zero consumption. Verify CLPC report IDs, CPU/GPU/ANE assignments and units against an independent reference under separate component loads; table positions and response to load alone prove nothing.
- A counter missing from one source may be in another: check `Energy Model`, `PMP`, CLPC and SMC before calling it unavailable. A zero that never moves means the source doesn't work. `powermetrics` doesn't show DRAM.
- New metric fields are optional, `null` without a source, and appear in `macmon debug`; existing fields read 0 without one.
- Validate sampling changes with `macmon pipe` on the affected hardware and macOS version; report runtime results separately from build and tests, naming untested configurations.
- Keep a PR under ~200 production lines (`make loc REF=origin/main`).

## Workflow

- `make check` and `make test` pass before every commit.
- Commits and PR titles are one line in the history's style (`feat: …`, `fix: …`, `docs: …`), no body. The PR body says what changed, its compatibility (JSON, Prometheus, library API, config), the checks run, and what was and wasn't tested by hand.
- Write a plan (`docs/plans/yyyymmdd-<name>.md`: behavior, decisions, a checklist; tick only its boxes while working) only when asked, agreed with the person, for large work: several PRs, open design decisions or work handed to agents; never for a change that fits one PR. Its final PR adds a first line `Done: yyyy-mm-dd, PR #N` and a short `## Result` and moves it to `docs/plans/completed/`; never delete a plan.
- Docs are only `readme.md`, the rustdoc, the roadmap, plans and research notes in `docs/`. `changelog.md` changes only in the person's `release: vX.Y.Z` PR.
