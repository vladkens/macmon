# macmon

Rootless Apple Silicon monitor and Rust library. Shared sampling and metric types live in `src/metrics.rs`, macOS API access in `src/sources.rs`, and CLI commands in `src/app/`.

The TUI is in `src/app/tui/`. Per-process usage (`src/app/procs.rs`) comes from libproc for the user's own processes, the setuid `/bin/ps` for other users' processes, and GPU time from the IORegistry (`IOAccelerator` children).

## Development

- Keep CLI-only code and dependencies behind the `app` feature; preserve library builds with `--no-default-features`.
- Keep normal metric collection rootless and preserve fallbacks for older systems.
- For Rust changes, run `make check` and focused tests for the changed behavior. `make prepare` applies formatting and fixes; do not use it as a read-only check.
- TUI colors only through `tui/theme.rs` (terminal ANSI colors, no RGB), so macmon follows the terminal's theme.
- Keep `~/.config/macmon.json` compatible with released versions: same field names and values (`view_type` is `"Sparkline"` / `"Gauge"`).
- Release builds abort on panic: every terminal mode the TUI enables must be undone in `restore_term_once`.
- The TUI saves settings on key presses: for manual runs set `HOME` to a temporary directory.

## Metric sources

- Before investigating or changing power sources, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and the relevant evidence in [docs/clpc-discovery.md](docs/clpc-discovery.md).
- Check counter availability before treating zero CPU/ANE readings as zero consumption.
- Verify CLPC report IDs, CPU/GPU/ANE assignments, and units against an independent reference under separate component loads. Table positions and response to load alone do not establish a mapping.
- For sampling changes, validate rootless readings with `macmon pipe` on the affected hardware and macOS version. Report runtime results separately from build and unit-test results, and identify untested configurations.
