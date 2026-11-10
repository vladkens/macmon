# Root config persistence

Skip all configuration persistence while effective UID is zero; remove path-based ownership handback and document session-only preferences under sudo. Preserve ordinary-user persistence.

JSON, Prometheus, library API and configuration schema remain unchanged. No dependencies are added.

- [x] Implement the scoped fix.
- [x] Add regression coverage.
- [x] Check nightly formatting.
- [x] Run make check and make test on the isolated branch.
- [ ] Submit upstream pull request.

Tests cover file and parent-directory symlinks, unchanged target ownership/content, and no directory creation through the root save path. A real root process is not exercised.
