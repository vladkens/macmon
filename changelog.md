## v0.7.2 – 2026-05-02

### Fixes
- Fixed sparkline chart rendering gaps in Apple Terminal
- Fixed frequency scale on MacBook Neo (#57)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.7.1...v0.7.2

---

## v0.7.1 – 2026-04-07

### Fixes
- Fixed CPU usage always showing 0% on Ultra chips (M1/M2/M3 Ultra) (#55)
- Fixed GPU temperature sometimes reporting bogus values (#54, by @gtalusan)
- Fixed potential data race when sharing metrics between threads
- Fixed memory size showing 0GB on systems with 256GB or more RAM

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.7.0...v0.7.1

---

## v0.7.0 – 2026-04-01

### Features
- Added HTTP server mode with JSON and Prometheus metrics endpoints (#34, #53)
- Added launchd service install/uninstall commands for the HTTP server (#34)
- Added `cpu_usage_pct` metric (#28)
- Added RAM usage percentage display in the label (#31)
- Exposed macmon as a library crate for programmatic use (#52, by @tasleson)

### Fixes
- Fixed crash on Apple M5 Max due to renumbered voltage-states keys (by @swiftraccoon)
- Fixed processor count parsing and dynamic E/P/S core labels on M5 (by @swiftraccoon)
- Fixed bogus sensor temperature readings being included in averages (#50, by @gtalusan)

### Docs
- Added Nix installation instructions (by @thibmaek)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.6.1...v0.7.0

---

## v0.6.1 – 2025-06-02

### Features
- Added SoC info output in pipe/JSON mode via `--soc-info` flag (by @aliasaria)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.6.0...v0.6.1

---

## v0.6.0 – 2025-02-26

### Features
- Added timestamp field in pipe mode output (#23)

### Fixes
- Fixed temperature smoothing on M3/M4 chips when sensor values are unavailable (#12)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.5.1...v0.6.0

---

## v0.5.1 – 2024-12-22

### Improvements
- Improved CPU average temperature calculation to include efficiency cores via SMC

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.5.0...v0.5.1

---

## v0.5.0 – 2024-12-20

### Features
- Added hotkeys to change the refresh interval interactively (#16)
- Allowed `--interval` flag to be specified in any argument position (#18)

### Fixes
- Fixed CPU power reporting for Ultra chips (#17)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.4.2...v0.5.0

---

## v0.4.2 – 2024-12-17

### Features
- Added RAM power metric and sample count limit option to the pipe command

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.4.1...v0.4.2

---

## v0.4.1 – 2024-12-14

### Fixes
- Fixed crash when running on virtual machines

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.4.0...v0.4.1

---

## v0.4.0 – 2024-12-01

### Features
- Added raw metrics output in JSON format via pipe command

### Fixes
- Fixed GPU frequency reporting (#11)

### Improvements
- Added smooth interpolation of temperature and power values between updates (#10)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.3.3...v0.4.0

---

## v0.3.3 – 2024-10-25

### Fixes
- Fixed excessively high values reported on M3 chips (#9)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.3.2...v0.3.3

---

## v0.3.2 – 2024-10-22

Internal maintenance release — no user-facing changes.

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.3.1...v0.3.2

---

## v0.3.1 – 2024-10-18

### Fixes
- Fixed RAM sparkline max value calculation (by @gianlucatruda)

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.3.0...v0.3.1

---

## v0.3.0 – 2024-10-06

### Features
- Added ability to switch chart type
- Added settings persistence between sessions

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.2.2...v0.3.0

---

## v0.2.2 – 2024-07-03

### Fixes
- Fixed IOHid crash

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.2.1...v0.2.2

---

## v0.2.1 – 2024-06-25

### Features
- Added total system power display
- Added `--no-color` mode

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.2.0...v0.2.1

---

## v0.2.0 – 2024-06-25

### Features
- Added CPU/GPU average temperature display
- Added ability to change colors
- Added version label to the UI
- Improved E-CPU and P-CPU frequency calculation from per-core metrics

**Full Changelog**: https://github.com/vladkens/macmon/compare/v0.1.0...v0.2.0

---

## v0.1.0 – 2024-06-16

Initial release.

**Full Changelog**: https://github.com/vladkens/macmon/commits/v0.1.0

---
