//! Screen layout: a full-width metrics box on top, as tall as its content, and the process list
//! in the rest of the screen.
//!
//! The metrics box holds one strip per row on the left (CPU clusters, GPU, RAM, SWAP) and the
//! power column on the right, or under the strips on narrow screens.

use ratatui::layout::{Margin, Rect};

/// Fewest process rows worth showing; with less room the process list is auto-hidden.
pub const PROC_MIN_ROWS: u16 = 3;
/// Rows of the process box besides the process rows: borders and the table header.
const PROC_CHROME: u16 = 3;
/// Narrowest metrics box with the power column next to the strips; narrower boxes put the power
/// rows under the strips.
pub const POWER_SIDE_MIN_WIDTH: u16 = 70;
/// Strips keep this width next to the power column: the power column gives way first, down to
/// its own minimum width.
pub const STRIPS_MIN_WIDTH: u16 = 36;
/// Cells between the strips and the power column: ` │ `.
const POWER_GAP: u16 = 3;
/// Blank cells inside the left and right borders of the metrics box.
const PADDING: u16 = 1;

/// One strip of the metrics box: `{label} {pct}% {detail} {graph or meter}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strip {
  /// CPU cluster, by index into the cluster list.
  Cluster(usize),
  Gpu,
  Ram,
  Swap,
}

/// Size of the power rows, measured from their text.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PowerSize {
  /// Rows with the fans on the Total row.
  pub rows: u16,
  /// Width that shows everything: numbers, temperatures and history graphs.
  pub width: u16,
  /// Narrowest power column next to the strips: the numbers with average and maximum.
  pub min_width: u16,
  /// Narrowest width with the fans on the Total row; narrower rows put them on a row of their
  /// own (0: nothing to move).
  pub fans_inline: u16,
}

impl PowerSize {
  /// Rows taken by power rows `width` cells wide.
  pub fn height(&self, width: u16) -> u16 {
    self.rows.saturating_add(u16::from(width < self.fans_inline))
  }
}

/// What the metrics box has to show.
#[derive(Debug, Default)]
pub struct Content {
  /// Number of CPU clusters, one strip each.
  pub clusters: usize,
  /// SWAP strip under RAM (only when swap is configured).
  pub swap: bool,
  /// Rows and widths of the power column.
  pub power: PowerSize,
}

/// Rectangles for one frame. Hidden parts are `None` / empty; every rect is non-empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
  /// Metrics box, borders included.
  pub top: Option<Rect>,
  /// Strips inside the metrics box, one row each, top to bottom.
  pub strips: Vec<(Strip, Rect)>,
  /// Power rows: a column right of the strips, or rows under them.
  pub power: Option<Rect>,
  /// Column of the line between the strips and the power column.
  pub separator: Option<Rect>,
  /// Process box, borders included.
  pub proc: Option<Rect>,
}

impl LayoutPlan {
  /// Box whose bottom border holds the key hints: the lowest one.
  pub fn bottom(&self) -> Option<Rect> {
    self.proc.or(self.top)
  }
}

fn non_empty(rect: Rect) -> Option<Rect> {
  if rect.is_empty() { None } else { Some(rect) }
}

/// Strips top to bottom: CPU clusters, GPU, RAM, SWAP.
fn strips(content: &Content) -> Vec<Strip> {
  let mut strips: Vec<Strip> = (0..content.clusters).map(Strip::Cluster).collect();
  strips.extend([Strip::Gpu, Strip::Ram]);
  if content.swap {
    strips.push(Strip::Swap);
  }
  strips
}

/// Width of the power column next to the strips in a box `inner_width` cells wide: everything
/// fits while the strips keep `STRIPS_MIN_WIDTH`, then the column shrinks down to its minimum
/// width. The strips always keep at least one cell.
fn side_power_width(inner_width: u16, power: &PowerSize) -> u16 {
  let room = inner_width.saturating_sub(POWER_GAP + STRIPS_MIN_WIDTH);
  let max = inner_width.saturating_sub(POWER_GAP + 1);
  room.min(power.width).max(power.min_width).min(max)
}

/// Splits `area` into the metrics box on top and the process box below it.
///
/// The metrics box is as tall as its content: one row per strip next to (or above) one row per
/// power row. The process box (`procs`) takes the rest of the height; left with fewer than
/// `PROC_MIN_ROWS` process rows, it is auto-hidden.
pub fn compute_layout(area: Rect, procs: bool, content: &Content) -> LayoutPlan {
  let mut plan = LayoutPlan::default();
  if area.is_empty() {
    return plan;
  }

  let strips = strips(content);
  let inner_width = area.width.saturating_sub(2 + 2 * PADDING);
  let side = area.width >= POWER_SIDE_MIN_WIDTH;
  let power_width = if side { side_power_width(inner_width, &content.power) } else { inner_width };
  let power_rows = content.power.height(power_width);
  let strip_rows = u16::try_from(strips.len()).unwrap_or(u16::MAX);
  let content_rows =
    if side { strip_rows.max(power_rows) } else { strip_rows.saturating_add(power_rows) };

  let top = Rect { height: content_rows.saturating_add(2).min(area.height), ..area };
  plan.top = Some(top);

  let rest = area.height - top.height;
  if procs && rest >= PROC_MIN_ROWS + PROC_CHROME {
    plan.proc = Some(Rect { y: top.bottom(), height: rest, ..area });
  }

  let inner = top.inner(Margin::new(1 + PADDING, 1));
  if inner.is_empty() {
    return plan;
  }

  let left = if side {
    let left = Rect { width: inner_width.saturating_sub(power_width + POWER_GAP), ..inner };
    plan.separator = non_empty(Rect { x: left.right() + 1, width: 1, ..inner });
    plan.power = non_empty(Rect { x: inner.right() - power_width, width: power_width, ..inner });
    left
  } else {
    let height = strip_rows.min(inner.height);
    plan.power = non_empty(Rect { y: inner.y + height, height: inner.height - height, ..inner });
    Rect { height, ..inner }
  };

  let rows = (left.top()..left.bottom()).map(|y| Rect { y, height: 1, ..left });
  plan.strips = strips.into_iter().zip(rows).collect();
  plan
}

#[cfg(test)]
mod tests {
  use ratatui::layout::{Margin, Rect};

  use super::{
    Content, LayoutPlan, POWER_SIDE_MIN_WIDTH, PROC_MIN_ROWS, PowerSize, STRIPS_MIN_WIDTH, Strip,
    compute_layout,
  };

  fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
  }

  /// Power rows of an M3 Pro with one fan: CPU / GPU / ANE `CPU    4.50W avg  4.50 max  4.50  45°C`
  /// (38 cells) and an 8 cells graph, Power, Total with the fan (45 cells).
  const POWER: PowerSize = PowerSize { rows: 5, width: 47, min_width: 32, fans_inline: 45 };

  /// M3 Pro: two clusters, swap configured.
  fn content() -> Content {
    Content { clusters: 2, swap: true, power: POWER }
  }

  fn layout(area: Rect, procs: bool) -> LayoutPlan {
    compute_layout(area, procs, &content())
  }

  fn strip_kinds(plan: &LayoutPlan) -> Vec<Strip> {
    plan.strips.iter().map(|(strip, _)| *strip).collect()
  }

  fn inside(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
      && inner.y >= outer.y
      && inner.right() <= outer.right()
      && inner.bottom() <= outer.bottom()
  }

  #[test]
  fn large_screen_splits_metrics_and_procs() {
    let plan = layout(rect(0, 0, 200, 50), true);

    // 5 strips next to 5 power rows plus the borders, the process list takes the rest
    assert_eq!(plan.top, Some(rect(0, 0, 200, 7)));
    assert_eq!(plan.proc, Some(rect(0, 7, 200, 43)));
    assert_eq!(plan.bottom(), plan.proc);

    // strips left with one padding cell, one row each; power column right, separator between
    use Strip::*;
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Gpu, Ram, Swap]);
    let rows: Vec<Rect> = plan.strips.iter().map(|(_, r)| *r).collect();
    assert_eq!(rows, (1..6).map(|y| rect(2, y, 146, 1)).collect::<Vec<_>>());
    assert_eq!(plan.separator, Some(rect(149, 1, 1, 5)));
    assert_eq!(plan.power, Some(rect(151, 1, POWER.width, 5)));
  }

  #[test]
  fn metrics_box_is_as_tall_as_its_content() {
    // (width, height, metrics box height): 5 strips next to 5 power rows; at 80 columns the
    // power column is 37 cells, the fan moves to a row of its own
    for (width, height, top) in [(200, 50, 7), (120, 40, 7), (100, 30, 7), (80, 24, 8)] {
      let plan = layout(rect(0, 0, width, height), true);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(rect(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(rect(0, top, width, height - top)), "{ctx}");
      assert!(plan.strips.iter().all(|(_, r)| r.height == 1), "{ctx}");
      assert!(plan.separator.is_some(), "{ctx}: power column on the side");
    }

    // more strips than power rows: three clusters and swap
    let three = Content { clusters: 3, ..content() };
    let plan = compute_layout(rect(0, 0, 120, 40), true, &three);
    assert_eq!(plan.top.map(|r| r.height), Some(8));

    // 72x24: still room for the power column next to the strips, at its minimum width
    let plan = layout(rect(0, 0, 72, 24), true);
    assert_eq!(plan.top, Some(rect(0, 0, 72, 8)));
    assert_eq!(plan.power, Some(rect(38, 1, POWER.min_width, 6)));
    assert_eq!(plan.strips[0].1.width, 33);
  }

  #[test]
  fn power_column_widens_on_wide_screens() {
    // (screen width, power column width): everything while the strips keep their minimum, then
    // narrower down to the minimum power width
    let cases = [
      (400, POWER.width),
      (200, POWER.width),
      (90, POWER.width),
      (89, POWER.width - 1),
      (80, 37),
      (75, POWER.min_width),
      (72, POWER.min_width),
      (POWER_SIDE_MIN_WIDTH, POWER.min_width),
    ];
    for (width, power) in cases {
      let plan = layout(rect(0, 0, width, 40), true);
      let ctx = format!("width {width}");
      let column = plan.power.expect("power column");
      let strips = plan.strips[0].1;
      assert_eq!(column.width, power, "{ctx}");
      assert_eq!(column.right(), width - 2, "{ctx}: one padding cell before the border");
      assert_eq!(strips.width + 3 + column.width, width - 4, "{ctx}: ` │ ` between them");
      assert!(power == POWER.min_width || strips.width >= STRIPS_MIN_WIDTH, "{ctx}");
    }

    // under the strips the power rows take the full width
    let plan = layout(rect(0, 0, POWER_SIDE_MIN_WIDTH - 1, 40), true);
    assert_eq!(plan.power.map(|r| r.width), Some(POWER_SIDE_MIN_WIDTH - 5));
  }

  #[test]
  fn fans_move_to_own_row_in_narrow_power_column() {
    let top = |width, power| {
      let content = Content { power, ..content() };
      compute_layout(rect(0, 0, width, 40), true, &content).top.map(|r| r.height)
    };

    // next to the strips: the fan fits after Total from 45 cells on (88 columns)
    assert_eq!(top(88, POWER), Some(7));
    assert_eq!(top(87, POWER), Some(8));

    // under the strips: 5 strips and 5 or 6 power rows
    assert_eq!(top(POWER_SIDE_MIN_WIDTH - 1, POWER), Some(12));
    assert_eq!(top(30, POWER), Some(13));
    // no fans or no Total row: nothing to move
    assert_eq!(top(30, PowerSize { fans_inline: 0, ..POWER }), Some(12));
  }

  #[test]
  fn narrow_screen_moves_power_under_strips() {
    let width = POWER_SIDE_MIN_WIDTH - 1;

    // 5 strips + 5 power rows + borders, the rest goes to the processes
    for height in [25, 40] {
      let plan = layout(rect(0, 0, width, height), true);
      assert_eq!(plan.separator, None);
      assert_eq!(plan.top, Some(rect(0, 0, width, 12)));
      assert_eq!(plan.proc, Some(rect(0, 12, width, height - 12)));
      assert_eq!(plan.strips.len(), 5);
      assert_eq!(plan.strips[4].1, rect(2, 5, width - 4, 1));
      assert_eq!(plan.power, Some(rect(2, 6, width - 4, 5)));
    }
  }

  #[test]
  fn small_screen_auto_hides_procs_by_height() {
    // 60x15: the metrics need 12 rows, the process box would get 3
    let plan = layout(rect(0, 0, 60, 15), true);
    assert_eq!(plan.proc, None);
    assert_eq!(plan.top, Some(rect(0, 0, 60, 12)));
    assert_eq!(plan.bottom(), plan.top);

    // width doesn't matter, only the rows left for the processes
    let plan = layout(rect(0, 0, 40, 40), true);
    assert!(plan.proc.is_some());

    // threshold: borders + header + PROC_MIN_ROWS process rows under a 7 rows metrics box
    let (top, min) = (7, PROC_MIN_ROWS + 3);
    let plan = layout(rect(0, 0, 100, top + min), true);
    assert_eq!(plan.top.map(|r| r.height), Some(top));
    assert_eq!(plan.proc.map(|r| r.height), Some(min));
    let plan = layout(rect(0, 0, 100, top + min - 1), true);
    assert_eq!(plan.proc, None);

    // shorter than the metrics: the box takes the whole screen, strips that don't fit are cut
    let plan = layout(rect(0, 0, 100, 5), true);
    assert_eq!(plan.top, Some(rect(0, 0, 100, 5)));
    assert_eq!(plan.strips.len(), 3);
    assert_eq!(plan.power.map(|r| r.height), Some(3));
  }

  #[test]
  fn hidden_procs_leave_metrics_box_as_is() {
    let area = rect(0, 0, 120, 40);
    let plan = layout(area, false);
    assert_eq!(plan.top, Some(rect(0, 0, 120, 7)));
    assert_eq!(plan.proc, None);
    assert_eq!(plan.bottom(), plan.top);
    assert_eq!(strip_kinds(&plan).len(), 5);
  }

  #[test]
  fn strips_follow_clusters_and_swap() {
    use Strip::*;
    let area = rect(0, 0, 120, 40);
    let plan = compute_layout(area, true, &Content { clusters: 3, ..content() });
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram, Swap]);

    // no swap configured: no SWAP strip
    let plan = compute_layout(area, true, &Content { swap: false, ..content() });
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Gpu, Ram]);

    // no metrics yet: no clusters
    let plan = compute_layout(area, true, &Content { clusters: 0, swap: false, ..content() });
    assert_eq!(strip_kinds(&plan), [Gpu, Ram]);
  }

  #[test]
  fn rects_stay_inside_and_never_overlap() {
    let widths = [1, 2, 4, 5, 10, 40, 69, 70, 72, 80, 100, 160, 200, 400];
    let heights = [1, 2, 3, 5, 6, 8, 12, 13, 15, 20, 24, 30, 40, 50, 120];
    let contents = [
      content(),
      Content { clusters: 3, swap: false, ..content() },
      Content { clusters: 0, power: PowerSize { fans_inline: 0, ..POWER }, ..content() },
    ];

    for (width, height) in widths.into_iter().flat_map(|w| heights.map(|h| (w, h))) {
      for offset in [(0, 0), (3, 2)] {
        let area = rect(offset.0, offset.1, width, height);
        for (content, procs) in contents.iter().flat_map(|c| [(c, false), (c, true)]) {
          let plan = compute_layout(area, procs, content);
          let ctx = format!("{area:?} {content:?} procs={procs}");

          // the metrics box is always on top, the process box right under it to the bottom
          let top = plan.top.expect("metrics box");
          assert_eq!((top.x, top.y, top.width), (area.x, area.y, area.width), "{ctx}");
          assert!(inside(area, top), "{ctx}");
          if let Some(proc) = plan.proc {
            assert!(procs, "{ctx}");
            assert_eq!(
              proc,
              Rect { y: top.bottom(), height: area.bottom() - top.bottom(), ..area }
            );
          }

          // parts of the metrics box sit inside its borders and never overlap
          let mut parts: Vec<Rect> = plan.strips.iter().map(|(_, r)| *r).collect();
          parts.extend(plan.power);
          parts.extend(plan.separator);
          let inner = top.inner(Margin::new(1, 1));
          for (i, a) in parts.iter().enumerate() {
            assert!(!a.is_empty() && inside(inner, *a), "{a:?} outside {inner:?} in {ctx}");
            for b in &parts[i + 1..] {
              assert!(!a.intersects(*b), "{a:?} overlaps {b:?} in {ctx}");
            }
          }
        }
      }
    }
  }
}
