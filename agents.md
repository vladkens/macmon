# macmon

Rootless Apple Silicon monitor and Rust library. Shared sampling and metric types live in `src_lib/metrics.rs`, macOS API access in `src_lib/sources.rs`, and CLI commands in `src_app/`.

## Development

- Keep CLI-only code and dependencies behind the `app` feature; preserve library builds with `--no-default-features`.
- Keep normal metric collection rootless and preserve fallbacks for older systems.
- For Rust changes, run `make check` and focused tests for the changed behavior. `make prepare` applies formatting and fixes; do not use it as a read-only check.

## Metric sources

- Before investigating or changing power sources, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and the relevant evidence in [docs/clpc-discovery.md](docs/clpc-discovery.md).
- Check counter availability before treating zero CPU/ANE readings as zero consumption.
- Verify CLPC report IDs, CPU/GPU/ANE assignments, and units against an independent reference under separate component loads. Table positions and response to load alone do not establish a mapping.
- For sampling changes, validate rootless readings with `macmon pipe` on the affected hardware and macOS version. Report runtime results separately from build and unit-test results, and identify untested configurations.
