Done: 2026-10-16, PR #1

# Pause process ordering (roadmap item 10, first PR)

Hold process rows in place for keyboard browsing while their metrics remain live. Filter
off/on is a separate PR and remains unfinished in roadmap item 10. Aim at no more than
200 changed production lines; report and simplify if the feature exceeds that scope.

## Behavior

- Space toggles one session-only pause state for the process list and info overlay. While
  typing a filter, Space remains text. Help and confirmation popups keep their key handling.
- Pausing preserves the current process order and scroll position. Arrow keys, scrolling,
  selection and copying continue to work. All present processes receive fresh metrics and
  system metrics continue updating at the normal interval.
- A process missing from a subsequent sample stays in its slot, prefixed with `(exited)`.
  Show unavailable live metrics as `-` instead of presenting its last rates as current.
  Preserve its identity and executable path for selection and copying. It cannot be killed
  or opened as a live process; existing process identity checks remain in force.
- Newly observed processes append at the bottom, prefixed with `(new)`. The marker lasts
  until resume; if such a process exits, `(exited)` replaces `(new)` in its existing slot.
- Match identities by PID and available start time. A reused PID with a different start
  time becomes a separate new row. The sampler currently lacks start times for `ps` rows:
  preserve existing restrictions on actions and document that identity limitation rather
  than claiming PID reuse detection for those rows.
- Space again resumes normal ordering, removes retained exited rows and clears `(new)`
  markers. Keep selection on the same process identity when it still exists.
- Both titles show `paused`; explain in help that process values remain live. Preserve the
  indicator at narrow widths and use the existing terminal theme.

## Interaction details

- Existing filter editing remains live while paused and may deliberately change which rows
  are visible. Status prefixes are display text, not additions to the underlying name or
  filter query. Explicit sorting changes the selected sort for resume without rearranging
  paused rows. Group totals exclude retained exited rows.
- An already-open info overlay holds its file/socket snapshot and counts while paused.
  Opening info on a live row while paused reads once, then holds that snapshot. Do not add
  process metric fields to the overlay as part of this work.
- Hiding the process panel, including automatic hiding on resize, resets pause and discards
  the snapshot as existing visibility handling does. Opening help does not reset pause.

## Rust approach

- Keep pause and row lifecycle bookkeeping in the TUI, leaving sampler types, config,
  JSON, Prometheus and the library API unchanged. Continue the existing process sampler
  so rate baselines remain current.
- Update stored process values in place by identity while paused and append new identities;
  do not run automatic sorting on that path. Keep lifecycle state separate from raw names.
- Gate info refresh independently of metric updates. Reuse existing filter, navigation,
  rendering, theme and fake-system test support; add no dependencies.

## Tasks

- [x] Agree the proposed interaction details and finalize this plan with the person.
- [x] Establish the person's GitHub fork, fetch current upstream main, and create an isolated
      feature worktree. Push only to the person's fork; target its main branch with the PR.
- [x] Implement order pause, live values, lifecycle markers, identity-aware selection and
      safe actions for retained rows, with focused tests.
- [x] Implement overlay pause and title indicators; update help and README in the same change.
- [x] Test resume, PID reuse where start time exists, new-then-exited rows, totals, filter
      typing, sort changes, hiding, narrow layouts and continuing system metric updates.
- [x] Run `make check`, `make test` and production line measurement; manually exercise arrow
      browsing and pause/resume with temporary settings isolated from the person's config.
- [x] Review the diff, commit passing steps and open the PR against the person's fork.
      Complete the repository's review workflow and record the final PR number and result
      before moving this plan to `docs/plans/completed/` and fixing its roadmap link. Keep
      item 10 unchecked because filter off/on remains a separate PR.

## Result

+102 production lines, entirely in the TUI. PR #1 targets `ratniel/macmon`, not upstream.
Order pause, live metrics, retained exits, appended new rows and overlay pause are implemented;
filter off/on remains for a separate PR. `make check` and `make test` passed independently
(42 library, 148 binary, 3 doc tests). A native M4 terminal smoke check at 250 ms exercised
arrow browsing, live values, lifecycle markers, resume and shared overlay pause with settings
disabled. Independent review found no actionable issues; the required Codex CLI review was
attempted but unavailable because the configured model was rejected for the account.
