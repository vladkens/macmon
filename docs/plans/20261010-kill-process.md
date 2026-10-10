# Kill the selected process (#84)

`k` in the TUI opens a small confirmation popup for the selected process: `t` sends SIGTERM, `f`
SIGKILL. One key, no signal menu, no tracking after the signal: the list shows whether the process
is gone, and one that ignores SIGTERM gets `k` then `f`.

## Behavior

- `k` works while the process list is visible, a process is selected, no filter is typed and the
  help is closed. The popup is centered over the process list (`Clear` + `draw_box`, like the
  help): `Kill <pid>` in the title, the name (cut to fit), then `t terminate | f force kill | Esc
  cancel`.
- While it is open every key goes to it (Ctrl-C still quits): `t` / `f` without a modifier send
  the signal and close it, any other key closes it. A click or the wheel, `FocusLost` and the list
  going hidden close it too.
- Errors replace the keys line, and any key closes the popup: pid <= 0, 1 and macmon's own pid
  are refused before any system call (`Won't kill …`); `kill(pid, 0)` ESRCH → `exited`, EPERM →
  `Not permitted`; a row without a sampled start time (`ps` rows) → `Not permitted`; a start time
  other than the sampled one → `exited`. At `t` / `f` the start time is read again right before
  `kill()`: changed or unreadable → `exited`; `kill()` errors as above, others → the strerror text.

## Decisions

- Identity is the start time from `proc_bsdinfo` (`ProcInfo::started`, `procs::bsd_info`), not
  name and path; a zombie counts as exited. `ps` rows are never killed: the popup could name one
  process and signal another.
- The re-read right before `kill()` leaves only the gap between the two calls: macOS has no
  identity-bound kill (no pidfd; a task port needs root or entitlements) and hands out pids in
  sequence, so a reuse inside it would need the pid freed and handed out again within
  microseconds.
- System calls go through a small trait (`signal`, `identity`) with a libc implementation; `App`
  holds it, and `Kill::default()` is a fake in tests, so no test signals a process it did not
  spawn. One test kills its own `sleep` child through libc, with a `Drop` guard that reaps it.

## Tasks

- [x] `src/app/tui/kill.rs`, wiring in `mod.rs`, the help line, tests; `make check` and `make test`
      pass.
