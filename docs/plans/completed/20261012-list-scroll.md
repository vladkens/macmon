Done: 2026-10-12, PR #98

# Scrolling in the lists (roadmap item 9)

A scrollbar on the right border of the process list and the info overlay, ↑ / ↓ wrapping around
at the ends, and the full text of a cut row in the info overlay scrolling on its bottom border.
One PR; aim ≤ 200 production lines (`make loc`), stop and report past that.

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
- Marquee: when the selected row of the overlay is cut, its full text (`~` form) goes on the
  bottom border. When it doesn't fit there either it scrolls: still at its start for 1.5 s, one
  cell every 67 ms (~15 cells/s), still at its end for 1.5 s, then from the start again. A new
  selection starts it over; a refresh that keeps the selection doesn't.
- The run loop waits for the next marquee step with `recv_timeout` only while the overlay is open
  and its marquee scrolls; otherwise it blocks on `recv()` as before.

## Decisions

- One helper in `boxes.rs` draws the scrollbar for both boxes with ratatui's `Scrollbar`
  (`VerticalRight`, track symbol `None`). Its state counts scroll positions, not rows:
  `ScrollbarState::new(len - page + 1).position(offset).viewport_content_length(page)`, so the
  thumb reaches the bottom at the last offset (with `new(len)` it stops short of it).
- `step` (shared by both lists) takes the row count and returns the clamped row, wrapping only
  ↑ / ↓. The overlay's wheel moves the selection itself instead of going through ↑ / ↓.
- The scrollbar ends a row above the bottom border and the marquee keeps a border cell before
  the corner, so they never overlap.

## Tasks

- [x] Scrollbar on the process list and the info overlay, with tests: only when the rows
      overflow, the thumb at the top, in the middle and at the bottom, in both boxes; every
      window size renders. Commit `feat: scrollbar on the process list and the info overlay`.
- [x] Wrap-around of ↑ / ↓ in both lists, with tests at both ends and none for the wheel, PgDn
      and End. Commit `feat: up and down wrap around in the lists`.
- [x] The marquee and the run loop timer, with tests of its timing (holds, steps, start over,
      restart on a new selection, not on a refresh) and no timer without it; roadmap item 9
      linked and ticked. Commit `feat: the selected path scrolls on the bottom border`.

## Result

+94 production lines. Codex review: the marquee measured the path in characters, so paths with
wide characters didn't scroll far enough; it counts cells now. By hand, macmon's own CPU over
20 s with the overlay open was 0.51 s with an uncut row selected and 0.98 s while the path
scrolls.
