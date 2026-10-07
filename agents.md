# macmon

Rootless Apple Silicon monitor and Rust library. Shared sampling and metric types live in `src_lib/metrics.rs`, macOS API access in `src_lib/sources.rs`, and CLI commands in `src_app/`.

## Structure

- `src_app/tui/`: the terminal UI. `mod.rs` runs it (threads, events, keys, terminal setup and restore), `layout.rs` splits the screen into the metrics box and the process list, `boxes.rs` draws the metric boxes and the box frame (titles, power summary, key hints), `proc_view.rs` is the process list (sort, filter, selection, mouse), `help.rs` the help overlay (`?`), `widgets.rs` the graph and gauge, `store.rs` the metric histories, `theme.rs` the colors.
- `src_app/procs.rs`: per-process usage without root: libproc for the current user's processes (CPU, footprint, energy), the setuid `/bin/ps` for other users' processes (CPU, resident size), GPU time of every process from the GPU's user clients in the IORegistry (`IOAccelerator` children).
- `src_app/config.rs`: the TUI settings in `~/.config/macmon.json`.

## Development

- Keep CLI-only code and dependencies behind the `app` feature; preserve library builds with `--no-default-features`.
- Keep normal metric collection rootless and preserve fallbacks for older systems.
- For Rust changes, run `make check` and focused tests for the changed behavior. `make prepare` applies formatting and fixes; do not use it as a read-only check.
- TUI colors only through `tui/theme.rs`: terminal colors (`Color::Reset`, ANSI), no RGB, so macmon follows the terminal's theme.
- `~/.config/macmon.json` stays backward compatible: configs of released versions must load, and values keep the names released versions use (`view_type` is saved as `"Sparkline"` / `"Gauge"`). A bad value resets only its own field.
- Release builds abort on panic, so nothing unwinds: every terminal mode the TUI turns on (raw mode, alternate screen, mouse capture, hidden cursor) must be undone in `restore_term_once`, which the panic hook calls.

## Testing

- Tests never touch the user's settings: `Config::load` has no path under `cfg!(test)`, and tests that check saving use `config::TempConfig` (a file in the temp directory).
- TUI render tests draw into ratatui's `TestBackend` and compare cells, colors and modifiers.
- For manual runs of the TUI, set `HOME` to a temporary directory: every settings key press saves `~/.config/macmon.json`.
- Check per-process values against Activity Monitor or `ps`.

## Metric sources

- Before investigating or changing power sources, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and the relevant evidence in [docs/clpc-discovery.md](docs/clpc-discovery.md).
- Check counter availability before treating zero CPU/ANE readings as zero consumption.
- Verify CLPC report IDs, CPU/GPU/ANE assignments, and units against an independent reference under separate component loads. Table positions and response to load alone do not establish a mapping.
- For sampling changes, validate rootless readings with `macmon pipe` on the affected hardware and macOS version. Report runtime results separately from build and unit-test results, and identify untested configurations.
