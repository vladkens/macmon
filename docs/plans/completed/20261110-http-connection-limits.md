Done: 2026-11-10, PR #137

# Http connection limits

Bound HTTP workers to 32, close idle connections after five seconds, and bound response writes with an absolute deadline. Handle worker creation and accept failures without panicking.

JSON, Prometheus, library API and configuration schema remain unchanged. No dependencies are added.

- [x] Implement the scoped fix.
- [x] Add regression coverage.
- [x] Check nightly formatting.
- [x] Run make check and make test on the isolated branch.
- [x] Submit upstream pull request.

Loopback tests cover admission limits, expiry, normal routes, and a non-reading client.

## Result

The server limits active workers and I/O lifetimes; regression tests and make check/test pass.
