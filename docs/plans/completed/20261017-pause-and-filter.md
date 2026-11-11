Done: 2026-10-18, PR #108

# Pause and filter off (roadmap item 10)

Pause the process and file lists for reading and selection. Keep process values and system metrics live.
Turn each filter off or on without entering its text again.

## Behavior

- `Space` pauses or resumes the process list and file updates. While paused, the list keeps the same processes and row order.
  New processes appear on the next sample after resume. Hiding the process list ends pause.
- If pause starts before the first sample, sort that sample when it arrives. Keep the list paused.
- Column clicks and `s` / `S` sort once while paused. Selection follows the same process.
  Filter group totals also sort once. Later samples update values without changing row or group order.
- Keep exited rows in place with dim text. Show unavailable values as `-` and the name without a prefix.
  A selected exited row has a dim red PID. Keep the rest of its selection highlight readable.
  Allow copying. Block actions that require a live process.
- Show `paused` in bold yellow on the bottom border of both lists. Keep it visible in narrow windows and during filter input.
- `f` turns the current filter off or on. Keep its text and show it dim while the filter is off.
  The process list and file list have separate filters. With no filter text, `f` has no effect.
- `/` turns the filter on for editing. While editing, `Space` and `f` enter text.
  Filter syntax, counts, and selection rules stay the same.

## Decisions

- Use existing `ProcInfo` rows and sort functions. Update paused rows by process identity.
  The stored list does not grow while paused.
- Identify processes by PID and available start time. The app cannot detect PID reuse when the start time is unavailable.
- Pause holds the file and socket list. Opening the file view while paused reads it once.
- Keep pause and filter state in the TUI. JSON output, Prometheus output, settings, and the library API stay compatible.

## Tasks

- [x] Pause process order and file updates. Keep process values live.
- [x] Sort the first sample and explicit sort requests while paused. Preserve selection.
- [x] Retain exited rows and ignore new processes until resume. Preserve safe actions.
- [x] Add the bottom pause label and independent filter switches. Update help and README.
- [x] Test startup, sorting, selection, process exits, PID reuse, filters, and narrow layouts.
- [x] Run `make check` and `make test`. Review the changes.

## Result

Pause keeps the same processes visible with live values. Sorting works while paused.
Exited rows stay dim, with a dim red PID when selected. Each view keeps its text when its filter is off.

`make check` and all 202 tests pass. Manual tests confirmed sorting, selection, process changes, filters, and resizing.
An automated test covers pause before the first sample. Production code: +154 lines.
