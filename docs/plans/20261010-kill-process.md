# Kill the selected process (#84)

`k` in the TUI opens a confirmation popup for the selected process with the buttons Terminate
(SIGTERM), Force kill (SIGKILL) and Cancel. No signal menu, no tracking after the signal: the list
shows whether the process is gone, and one that ignores SIGTERM gets `k` then Force kill.

## Behavior

- `k` works while the process list is visible, a process is selected, no filter is typed and the
  help is closed. The popup is centered over the process list (`Clear` + `draw_box`, like the
  help), 60 columns wide (less in a narrower window): `Kill <name>?` in the title (the name cut
  before `?`), `pid · user`, the path (dim, cut from the start; no line without a path), and
  `[ Terminate ]   [ Force kill ]   [ Cancel ]` centred, Terminate selected, the selected one
  highlighted like the selected row, `T` / `F` / `C` underlined. No hints on the bottom border.
- While it is open every key goes to it (Ctrl-C still quits): `←` `→` / Tab / Shift-Tab move the
  selection (Terminate first, no wrap), Enter presses the selected button, `t` / `f` / `c` without a
  modifier press Terminate / Force kill / Cancel, Esc closes; other keys do nothing. A left click on
  a button presses it; other clicks, the wheel and `FocusLost` do nothing. The list going hidden
  closes it.
- Terminate and Force kill send the signal and close the popup; Cancel closes it. Errors show `Kill
  <name>` in the title, the reason, `pid · user` (dim) and a single centred `[ OK ]`, which Enter,
  Esc or a click closes: pid <= 0, 1 and macmon's own pid are refused before any system call (`Won't
  kill …`); `kill(pid, 0)` ESRCH → `Already exited`, EPERM → `Not permitted`; a row without a
  sampled start time (`ps` rows) → `Not permitted`; a start time other than the sampled one →
  `Already exited`. On a signal button the start time is read again right before `kill()`: changed
  or unreadable → `Already exited`; `kill()` errors as above, others → the strerror text.

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
