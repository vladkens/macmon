Done: 2026-10-16, PR #2

# Toggle saved filters (roadmap item 10, filter PR)

Temporarily see the unfiltered list without losing the query. This PR is independent of the
pause-order PR and starts from the fork's main branch; do not include pause implementation.

## Behavior

- `/` enables the current view's filter and edits its saved text. Filtering remains live
  while typing; Enter ends editing. Existing Esc and navigation behavior stays intact.
- Outside filter input, lowercase `f` disables or enables a nonempty filter without changing
  its text or entering input mode. With no filter text, `f` does nothing. While editing,
  `f` is ordinary text. Process and info-overlay filters toggle independently.
- Disabled filter text remains visible in its existing title/divider location, faint using
  the terminal theme. Do not add another permanent shortcut hint to the main UI; document
  `f` in help and the README. Preserve existing truncation on narrow windows.
- Disabling shows all otherwise eligible rows and normal counts, removes process filter
  group totals, and preserves selection where possible using existing selection handling.
  Re-enabling restores matching and totals; a selected row outside the filter follows the
  existing view's selection rules. Exclusion and comma-separated terms keep their syntax.
- New views start with filtering enabled. Enabled state is transient, with no config, JSON,
  Prometheus or library API changes. Existing text persistence across panel hiding stays.

## Implementation

- Keep one enabled flag beside each existing filter String; do not replace the filter model
  or introduce an abstraction. Reuse existing parsing, title rendering, theme faint style,
  refresh/follow methods and fake-system test support.
- Apply an empty query when disabled without modifying the saved String. Keep matching,
  totals, counts and empty-state messages consistent with the effective query.
- Keep this under 200 production lines; stop and report if the scope exceeds that.

## Tasks

- [x] Record the previously agreed behavior and create an isolated feature worktree from main.
- [x] Implement independent process and info filter toggles with shared faint title rendering.
- [x] Add focused tests for off/on text preservation, selection, counts/totals, typing `f`,
      `/` re-enabling, exclusion terms and narrow rendering; update help and README.
- [x] Run `make check`, `make test`, `make loc REF=origin/main`, and inspect the complete diff.
- [x] Exercise the native terminal with settings isolated; independently review the change.
- [x] Commit passing work and open a separate PR against `ratniel/macmon` main; handle review
      feedback, record the PR number and result, move this plan to completed and fix its link.
      Keep item 10 unchecked until both pause and filter changes are present on main.

## Result

PR #2 targets `ratniel/macmon` main independently of pause PR #1. +47 production lines
reuse the existing parser, selection handling, title rendering and theme. `make check` and
all 193 tests passed independently. Native M4 terminal testing at 250 ms verified off/on,
query preservation, `/` re-enabling, typing `f`, and separate process/overlay filters.
Independent review found no actionable issues. The required CLI review was attempted but
unavailable for the configured model/account; no settings changed. Both PRs overlap in
title rendering, help and documentation; the second merged PR requires a rebase.
