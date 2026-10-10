# Kill the selected process (#84)

`k` in the TUI sends SIGTERM to the selected process after a y/n confirmation and, once the
process has had 2 s to exit, offers SIGKILL. One key, no signal menu, nothing is signalled without
a `y`, and only the process that was confirmed is signalled (up to the race in Decisions).

## Behavior

Messages go on the left of the process box's bottom border, in place of the selected process path
(`ProcView::border_text`), while they are shown. Prompts and one-off messages show whatever is
selected; the tracking line (`SIGTERM sent…`, `still running…`) only while the tracked process is
selected or nothing is, otherwise the selected process path shows as before.

- `k` is handled in the global key match of `App::handle_key` (after `update_proc_view`), only
  while the process list is visible and a process is selected. `k` typed into the filter stays
  text; `k` without a selection does nothing.
- Checks at `k`, in this order:
  - pid `<= 0` → `Won't kill pid <pid>`; pid 1 → `Won't kill launchd (pid 1)`; macmon's own pid →
    `Won't kill macmon itself`. These checks live in the state machine, so every path that sends a
    signal goes through them.
  - `kill(pid, 0)` fails: `ESRCH` → `<pid> <name> exited`; `EPERM` → `Not permitted to kill <pid>
    <name>`.
  - The row has no sampled start time (`ProcInfo::started`, from libproc; `None` for `ps` rows)
    → `Not permitted to kill <pid> <name>`: rows without a sampled start time, i.e. `ps` rows,
    are never killed, since whichever process has the pid now can't be told from the sampled one
    (setuid processes you launched can pass `kill(pid, 0)` and still be unreadable).
  - Read the identity (start time, see Decisions). Unreadable or different from the sampled one
    → `<pid> <name> exited`: the process is gone since the sample, so the prompt never names one
    process and asks about another.
  - Otherwise store the target `(pid, name, identity)` and ask `Kill <pid> <name>? y/n`. Hints
    while asking: `y kill`, `any key cancel` (`y force kill` on the force-kill prompt). Opening a
    prompt clears a one-off message.
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
- `k` on the tracked process: before 2 s → `waiting for exit…` (it ends at 2 s), no signal;
  after it → `Force kill <pid> <name>? y/n`. The prompt is a sub-state of tracking: ticks keep checking the
  process, and an exit while it is open closes it and shows `exited`. `y` re-checks the identity
  as above, then sends SIGKILL → `SIGKILL sent to <pid> <name>`, tracked the same way until it
  exits (no further escalation): `still running` without the force hint, and `k` answers
  `waiting for exit…`. A failed SIGKILL keeps the tracking and shows the error.
- `k` on another process opens its own prompt and keeps the old tracking; the old tracking is
  replaced only when the new SIGTERM is actually sent after `y`.
- While the list is hidden, tracking goes on and messages aren't drawn. One-off messages
  (`exited`, errors, refusals) disappear after 5 s; `still running` stays while the process runs.
- Narrow borders: while a kill line is shown, key hints drop from the end until it gets all its
  text but the name plus up to 16 cells of the name (a line without a name needs its full
  width); only once no hint is left is the name cut further. Only the name is ever cut, so
  `? y/n` and `· k force kill` stay on the border.

## Decisions

- Identity is the process start time (`pbi_start_tvsec`, `pbi_start_tvusec` of `proc_bsdinfo`,
  read with `proc_pidinfo(PROC_PIDTBSDINFO)`), not name and path: a reused pid can run the same
  program. Make `procs::bsd_info` `pub(crate)` and reuse it. A zombie is either unreadable or
  `pbi_status == SZOMB`; treat both as gone. Rows without a sampled start time (`ps` rows) are
  never killed: the prompt could name one process and signal another.
- The start time is re-read right before every `kill()`, which leaves only the gap between the
  two calls. macOS has no identity-bound kill (no pidfd; a task port needs `task_for_pid`, i.e.
  root or entitlements), and it hands out pids in sequence, so a reuse inside that gap would need
  the pid to be freed and handed out again within microseconds. No workaround beyond that.
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

- [x] Tracking after SIGTERM: tick checks, `exited`, `still running · k force kill` after 2 s,
      5 s expiry of one-off messages.
- [x] `k` on the tracked process: `waiting for exit…` before 2 s; after it the force-kill prompt
      as a sub-state of tracking, the identity re-check at `y`, SIGKILL and its tracking. `k` on
      another process replaces the tracking only at `y`.
- [x] Tests with the fake and explicit times: exit before and after 2 s; a zombie counts as
      exited; a pid reused before `y` (new start time) gets no SIGKILL; `k` before 2 s sends
      nothing; an exit while the force-kill prompt is open closes it and `y` sends nothing;
      cancelling a prompt for another process keeps the old tracking. One test with a real child: the test
      spawns `sleep 30`, sends SIGTERM through the libc implementation, polls the tracking with
      real time up to 5 s while the child is a zombie (its pid can't be reused) until `exited`,
      then reaps it with `wait()`, and kills and reaps the child in a `Drop` guard so a failure
      never leaves it running.
- [x] `make check` and `make test` pass; commit `feat: force kill a process that ignores SIGTERM`.

### 3. Review fixes

- [x] Narrow borders as in Behavior: `Note::wanted_width` replaces the kept-end flag.
- [x] `ProcInfo::started` from `proc_bsdinfo` (`None` for `ps` rows), passed to `Kill::ask`;
      a reused pid since the sample → `exited`, no prompt.
- [x] The tracking line only while the tracked process or nothing is selected; `y force kill`
      on the force-kill prompt; `waiting for exit…` as its own message kind, not matched by text.
- [x] Tests: 80-cell lines for `SIGTERM sent`, `still running` and `Not permitted`; a pid
      restarted before `k`; the tracking line and the selection; `k` with the list hidden or the
      help open; the wheel closes a prompt; a failed SIGKILL keeps the tracking; ticks while the
      list is hidden; no test App with the libc implementation.
- [x] `make check` and `make test` pass; commit `fix: keep kill messages readable and bind the
      prompt to the sampled process`.

## Manual check (outside the tasks)

Run with `HOME` set to a temporary directory. In another terminal start `sleep 1000` and
`sh -c 'trap "" TERM; while :; do sleep 1; done'`; kill the first (exits on SIGTERM) and the second
(still running → force kill). Try a root process (not permitted) and pid 1.

