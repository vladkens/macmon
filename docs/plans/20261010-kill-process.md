# Kill the selected process (#84)

`k` in the TUI sends SIGTERM to the selected process after a y/n confirmation and, once the
process has had 2 s to exit, offers SIGKILL. One key, no signal menu, nothing is signalled without
a `y`, and only the process that was confirmed is signalled.

## Behavior

Messages go on the left of the process box's bottom border, in place of the selected process path
(`ProcView::border_text`), while they are shown.

- `k` is handled in the global key match of `App::handle_key` (after `update_proc_view`), only
  while the process list is visible and a process is selected. `k` typed into the filter stays
  text; `k` without a selection does nothing.
- Checks at `k`, in this order:
  - pid `<= 0` → `Won't kill pid <pid>`; pid 1 → `Won't kill launchd (pid 1)`; macmon's own pid →
    `Won't kill macmon itself`. These checks live in the state machine, so every path that sends a
    signal goes through them.
  - `kill(pid, 0)` fails: `ESRCH` → `<pid> <name> exited`; `EPERM` → `Not permitted to kill <pid>
    <name>`.
  - Read the identity (start time, see Decisions). Unreadable → `Not permitted to kill <pid>
    <name>` (setuid processes you launched can pass `kill(pid, 0)` and still be unreadable).
  - Otherwise store the target `(pid, name, identity)` and ask `Kill <pid> <name>? y/n`. Hints
    while asking: `y kill`, `any key cancel`.
- While asking, every key goes to the prompt (Ctrl-C still quits). Only `KeyCode::Char('y')`
  without Ctrl / Alt / Cmd confirms; any other key cancels and does nothing else. A click or wheel
  event, `FocusLost`, and the process list going hidden (`App::set_procs_visible(false)`, e.g. the
  window shrinking) cancel too.
- At `y`: re-read the identity and compare it with the stored target right before sending. A
  mismatch, an unreadable identity or `ESRCH` → `<pid> <name> exited`, no signal. Otherwise send
  SIGTERM to the stored target (never to the current selection) → `SIGTERM sent to <pid> <name>`.
  Another failure → `Failed to kill <pid> <name>: <strerror>`.
- After SIGTERM the target is tracked with the time it was sent. On every `Event::Tick` (250 ms;
  resizes also arrive as ticks, so use elapsed time, never a tick count) it is gone when
  `kill(pid, 0)` gives `ESRCH`, or its identity is unreadable or differs. Gone →
  `<pid> <name> exited`, tracking ends.
- Still alive 2 s after SIGTERM → `<pid> <name> still running · k force kill`, shown until it
  exits.
- `k` on the tracked process: before 2 s → `waiting for exit…`, no signal; after it →
  `Force kill <pid> <name>? y/n`. The prompt is a sub-state of tracking: ticks keep checking the
  process, and an exit while it is open closes it and shows `exited`. `y` re-checks the identity
  as above, then sends SIGKILL → `SIGKILL sent to <pid> <name>`, tracked the same way until it
  exits (no further escalation).
- `k` on another process opens its own prompt and keeps the old tracking; the old tracking is
  replaced only when that prompt is confirmed with `y`.
- While the list is hidden, tracking goes on and messages aren't drawn. One-off messages
  (`exited`, errors, refusals) disappear after 5 s; `still running` stays while the process runs.
- Long names: cut only the name, so `? y/n` and `· k force kill` always stay on the border.

## Decisions

- Identity is the process start time (`pbi_start_tvsec`, `pbi_start_tvusec` of `proc_bsdinfo`,
  read with `proc_pidinfo(PROC_PIDTBSDINFO)`), not name and path: a reused pid can run the same
  program. Make `procs::bsd_info` `pub(crate)` and reuse it. A zombie is either unreadable or
  `pbi_status == SZOMB`; treat both as gone.
- Liveness uses `kill(pid, 0)` plus the identity on each tick, not the process sample, whose
  interval can be up to 10 s.
- Code: a new `src/app/tui/kill.rs` with the state machine. System calls go through a small trait
  (`signal(pid, sig) -> Result<(), errno>`, `identity(pid) -> Option<Identity>`), and the state
  machine takes `Instant`s as arguments. `App` owns the state with the syscalls as
  `Box<dyn KillSys>`; `App`'s `Default` uses the libc implementation (write `Debug` by hand), and
  every test replaces it with a fake. No test except the one real-child test may use the libc
  implementation: the fixture pids (1, 631, 2301, …) are real pids on the dev machine.
- `help.rs` gets one line: `Key("k", "kill the selected process, again to force")`. No README
  change.

## Tasks

### 1. Confirm and send SIGTERM

- [x] `src/app/tui/kill.rs`: the syscall trait with a libc implementation, the refusals, the
      `ESRCH` / `EPERM` / unreadable messages, the prompt with its stored target, the identity
      re-check at `y`, SIGTERM and its messages. Only what this task uses: no tracking, `Instant`s
      or SZOMB yet (clippy runs with `-D warnings`). One-off messages stay until the next `k` for
      now.
- [x] Wire it into `src/app/tui/mod.rs` (`k` in the global match, prompt keys first after help,
      cancel on mouse, `FocusLost` and `set_procs_visible(false)`) and show the prompt and
      messages on the bottom border (`proc_view.rs` / `boxes.rs`), with the prompt hints and the
      name cut so the prompt's end stays.
- [x] Help line in `src/app/tui/help.rs`.
- [x] Tests with the fake: pid 0, a negative pid, pid 1 and the own pid are never signalled;
      `EPERM`, `ESRCH` and an unreadable identity give their messages without a prompt; `y`
      sends SIGTERM once to the stored target; other keys cancel without a signal; `k`, a click
      on another row, `y` → no signal; an identity change between `k` and `y` → `exited`, no
      signal; `FocusLost` cancels; `k` typed into the filter stays text. One render test of the
      prompt and hints, also at a narrow width with a long name.
- [x] `make check` and `make test` pass; commit `feat: kill the selected process with k`.

### 2. Track the exit and force kill

- [ ] Tracking after SIGTERM: tick checks, `exited`, `still running · k force kill` after 2 s,
      5 s expiry of one-off messages.
- [ ] `k` on the tracked process: `waiting for exit…` before 2 s; after it the force-kill prompt
      as a sub-state of tracking, the identity re-check at `y`, SIGKILL and its tracking. `k` on
      another process replaces the tracking only at `y`.
- [ ] Tests with the fake and explicit times: exit before and after 2 s; a zombie counts as
      exited; a reused pid (new start time) is never sent SIGKILL; `k` before 2 s sends nothing;
      an exit while the force-kill prompt is open closes it and `y` sends nothing; cancelling a
      prompt for another process keeps the old tracking. One test with a real child: the test
      spawns `sleep 30`, sends SIGTERM through the libc implementation, reaps it with `wait()`,
      polls the tracking with real time up to 5 s until `exited`, and kills and reaps the child in
      a `Drop` guard so a failure never leaves it running.
- [ ] `make check` and `make test` pass; commit `feat: force kill a process that ignores SIGTERM`.

## Manual check (outside the tasks)

Run with `HOME` set to a temporary directory. In another terminal start `sleep 1000` and
`sh -c 'trap "" TERM; while :; do sleep 1; done'`; kill the first (exits on SIGTERM) and the second
(still running → force kill). Try a root process (not permitted) and pid 1.

