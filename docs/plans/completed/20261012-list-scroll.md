Done: 2026-10-12, PR #98

# Scrolling in the lists (roadmap item 9)

A scrollbar on the right border of the process list and the info overlay, ↑ / ↓ wrapping around
at the ends, and the full path of the selected row within reach: `c` copies it in the list and in
the overlay. One PR; aim ≤ 200 production lines (`make loc`), stop and report past that.

## Behavior

- Scrollbar: only while the rows don't fit, drawn over the right border column beside the rows
  (the process box: below the header row and the group totals; the overlay: below the divider).
  `↑` on the first row, `↓` on the last, the thumb `┃` between them; the border `│` stays as the
  track. Arrows in `theme::BORDER`, the thumb in `theme::TEXT`. The thumb fills the track in the
  proportion of the rows on screen and sits at its top / bottom when the list is scrolled to its
  start / end. Fewer than 3 rows leave no room for it: nothing is drawn.
- Wrap-around: in both lists ↓ on the last row selects the first, ↑ on the first selects the
  last. The wheel, PgUp / PgDn and Home / End stop at the ends. Without a selection in the
  process list ↑ and ↓ both select the top row on screen, as before. ↑ / ↓ still end filter
  input.
- Copy: `c` copies the selected process's executable path in the list (nothing for an empty
  path) and the selected row's full text in the overlay (the path, or the socket text). A faint
  `copied` shows on the left of the bottom border (in place of the POWER note in the list) until
  the next key press, click or wheel step. Typed into a filter `c` is text. The process title's
  actions are `info | finder | copy | kill`, the overlay's `copy | finder`.
- Overlay borders: `? | q back` on the right of the bottom border, as on the main footer;
  nothing there while the filter is typed. `?` opens the help over the overlay (unless typed into
  its filter); closing the help returns to the overlay as it was. The help has a short "Info
  overlay" group.
- Typing hints: `Enter keep | Esc clear` only, on the process box's bottom border and on the
  overlay's divider; ↑ / ↓ end the input as Enter does.

## Decisions

- One helper in `boxes.rs` draws the scrollbar for both boxes with ratatui's `Scrollbar`
  (`VerticalRight`, track symbol `None`). Its state counts scroll positions, not rows:
  `ScrollbarState::new(len - page + 1).position(offset).viewport_content_length(page)`, so the
  thumb reaches the bottom at the last offset (with `new(len)` it stops short of it).
- `step` (shared by both lists) takes the row count and returns the clamped row, wrapping only
  ↑ / ↓. The overlay's wheel moves the selection itself instead of going through ↑ / ↓.
- The scrolling path was tried and dropped: the selected path on the overlay's bottom border,
  first scrolling when cut (hold, scroll, jump back), then as a ticker. It needed a redraw timer
  (macmon's own CPU over 20 s with the overlay open went from 0.51 s to 0.98 s) and a moving
  line; copy and finder give the full path without a timer.
- Copy runs `/usr/bin/pbcopy` with `LC_ALL=en_US.UTF-8` through `ProcSys::copy` (stdin piped,
  written and reaped on a thread, as `reveal`), so tests use the fake. `copied` has no timer:
  it goes with the next input.
- The open mode is aligned as `ls` shows it: `r-`, `-w`, `rw` (owner's review).

## Tasks

- [x] Scrollbar on the process list and the info overlay, with tests: only when the rows
      overflow, the thumb at the top, in the middle and at the bottom, in both boxes; every
      window size renders. Commit `feat: scrollbar on the process list and the info overlay`.
- [x] Wrap-around of ↑ / ↓ in both lists, with tests at both ends and none for the wheel, PgDn
      and End. Commit `feat: up and down wrap around in the lists`.
- [x] The scrolling path on the overlay's bottom border (dropped in review).
- [x] Review: typing hints without `↑↓ select`; `c` copies the selected path with `copied`
      until the next key; `?` opens the help over the overlay; `? | q back` on its bottom border
      in place of the scrolling path; tests for each.

## Result

+103 production lines. In review the scrolling path on the overlay's bottom border was replaced
by `c` (copy) and `? | q back`, and the help opens over the overlay. Codex review: pbcopy gets a
UTF-8 locale (under `LC_ALL=C` it read the UTF-8 text as MacRoman), and a click clears `copied`
in the overlay too.
