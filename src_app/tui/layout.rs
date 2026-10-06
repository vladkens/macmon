//! Screen layout: the metric boxes of the original macmon in the top part of the screen and the
//! process list in the rest.
//!
//! The metrics box holds two rows of boxes: one per CPU cluster, then GPU and RAM on top, CPU /
//! GPU / ANE power below. The boxes of a row split its width evenly, the rows split the height
//! (the top row gets the odd one). One structure at every size, only the scale changes.

use ratatui::layout::{Margin, Rect};

/// Share of the screen height for the metrics box over the process list, in percent.
pub const METRICS_HEIGHT_PCT: u32 = 40;
/// Lowest metrics box over the process list: two rows of boxes with one graph row each, plus the
/// borders.
pub const METRICS_MIN_HEIGHT: u16 = 8;
/// Fewest process rows worth showing; with less room the process list is auto-hidden.
pub const PROC_MIN_ROWS: u16 = 3;
/// Rows of the process box besides the process rows: borders and the table header.
const PROC_CHROME: u16 = 3;

/// One box of the metrics box: a title over a history graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
  /// CPU cluster, by index into the cluster list.
  Cluster(usize),
  Gpu,
  Ram,
  CpuPower,
  GpuPower,
  AnePower,
}

/// Rectangles for one frame. Hidden parts are `None` / left out; every rect is non-empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
  /// Metrics box, borders included.
  pub top: Option<Rect>,
  /// Metric boxes inside the metrics box, borders included: the top row left to right, then the
  /// bottom row.
  pub boxes: Vec<(Metric, Rect)>,
  /// Process box, borders included.
  pub proc: Option<Rect>,
}

/// Boxes of the top row: CPU clusters, GPU, RAM.
fn top_row(clusters: usize) -> Vec<Metric> {
  let mut boxes: Vec<Metric> = (0..clusters).map(Metric::Cluster).collect();
  boxes.extend([Metric::Gpu, Metric::Ram]);
  boxes
}

/// Boxes of the bottom row: CPU, GPU and ANE power.
const BOTTOM_ROW: [Metric; 3] = [Metric::CpuPower, Metric::GpuPower, Metric::AnePower];

/// Height of the metrics box over the process list on a screen `height` rows tall:
/// `METRICS_HEIGHT_PCT` of it, rounded, but at least `METRICS_MIN_HEIGHT`.
fn metrics_height(height: u16) -> u16 {
  // a share of `height`, so it fits in u16
  let share = ((u32::from(height) * METRICS_HEIGHT_PCT + 50) / 100) as u16;
  share.max(METRICS_MIN_HEIGHT).min(height)
}

/// `count` (at least one) boxes side by side splitting the width of `row` evenly; the odd cells
/// are spread between them. Boxes without cells are left out.
fn split_row(row: Rect, count: usize) -> impl Iterator<Item = Rect> {
  let (width, count) = (u64::from(row.width), count as u64);
  // offsets stay within `row.width`, so they fit in u16
  let edge = move |i: u64| (width * i / count) as u16;
  (0..count)
    .map(move |i| Rect { x: row.x + edge(i), width: edge(i + 1) - edge(i), ..row })
    .filter(|r| !r.is_empty())
}

/// Splits `area` into the metrics box and the process box below it.
///
/// With the process list on (`procs`), the metrics box takes `METRICS_HEIGHT_PCT` of the height
/// and the process box the rest; left with fewer than `PROC_MIN_ROWS` process rows, the process
/// list is auto-hidden. Without it, the metrics box takes the whole screen.
pub fn compute_layout(area: Rect, procs: bool, clusters: usize) -> LayoutPlan {
  let mut plan = LayoutPlan::default();
  if area.is_empty() {
    return plan;
  }

  let mut top = area;
  let height = metrics_height(area.height);
  let rest = area.height - height;
  if procs && rest >= PROC_MIN_ROWS + PROC_CHROME {
    top.height = height;
    plan.proc = Some(Rect { y: top.bottom(), height: rest, ..area });
  }
  plan.top = Some(top);

  let inner = top.inner(Margin::new(1, 1));
  let upper = inner.height.div_ceil(2);
  let rows = [
    (Rect { height: upper, ..inner }, top_row(clusters)),
    (Rect { y: inner.y + upper, height: inner.height - upper, ..inner }, BOTTOM_ROW.to_vec()),
  ];
  for (row, metrics) in rows {
    if row.is_empty() {
      continue;
    }
    let count = metrics.len();
    plan.boxes.extend(metrics.into_iter().zip(split_row(row, count)));
  }

  plan
}

#[cfg(test)]
mod tests {
  use ratatui::layout::{Margin, Rect};

  use super::{
    LayoutPlan, METRICS_MIN_HEIGHT, Metric, PROC_MIN_ROWS, compute_layout, metrics_height,
  };

  fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
  }

  /// Two CPU clusters, as on M1–M5.
  fn layout(area: Rect, procs: bool) -> LayoutPlan {
    compute_layout(area, procs, 2)
  }

  fn kinds(plan: &LayoutPlan) -> Vec<Metric> {
    plan.boxes.iter().map(|(metric, _)| *metric).collect()
  }

  fn inside(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
      && inner.y >= outer.y
      && inner.right() <= outer.right()
      && inner.bottom() <= outer.bottom()
  }

  #[test]
  fn metrics_take_40_percent_and_procs_the_rest() {
    // (width, height, metrics box height): 40 % of the height, rounded
    for (width, height, top) in [(200, 50, 20), (110, 32, 13), (80, 24, 10), (120, 40, 16)] {
      let plan = layout(rect(0, 0, width, height), true);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(rect(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(rect(0, top, width, height - top)), "{ctx}");
    }

    // short screens: at least two rows of boxes with a graph row each
    assert_eq!(metrics_height(15), METRICS_MIN_HEIGHT);
    let plan = layout(rect(0, 0, 100, 15), true);
    assert_eq!(plan.top, Some(rect(0, 0, 100, 8)));
    assert_eq!(plan.proc, Some(rect(0, 8, 100, 7)));
  }

  #[test]
  fn two_rows_of_boxes_split_evenly() {
    // 200x50: a 20 rows metrics box, 18 rows inside: 9 for each row of boxes
    let plan = layout(rect(0, 0, 200, 50), true);
    use Metric::*;
    assert_eq!(kinds(&plan), [Cluster(0), Cluster(1), Gpu, Ram, CpuPower, GpuPower, AnePower]);

    // 198 cells inside the borders: 4 boxes of 49 / 50 cells, then 3 boxes of 66 cells
    let rects: Vec<Rect> = plan.boxes.iter().map(|(_, r)| *r).collect();
    assert_eq!(
      rects,
      [
        rect(1, 1, 49, 9),
        rect(50, 1, 50, 9),
        rect(100, 1, 49, 9),
        rect(149, 1, 50, 9),
        rect(1, 10, 66, 9),
        rect(67, 10, 66, 9),
        rect(133, 10, 66, 9),
      ]
    );

    // an odd number of rows: the top row gets the extra one (110x32: 13 rows, 11 inside)
    let plan = layout(rect(0, 0, 110, 32), true);
    let heights: Vec<u16> = plan.boxes.iter().map(|(_, r)| r.height).collect();
    assert_eq!(heights, [6, 6, 6, 6, 5, 5, 5]);
    assert_eq!(plan.boxes[4].1.y, 7);
  }

  #[test]
  fn box_count_follows_clusters() {
    use Metric::*;
    let area = rect(0, 0, 200, 50);

    // three tiers, like M6 (6E + 4P + 2S): five boxes on top
    let plan = compute_layout(area, true, 3);
    let top: Vec<Metric> = kinds(&plan).into_iter().take(5).collect();
    assert_eq!(top, [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram]);
    assert_eq!(plan.boxes.len(), 8);
    let widths: Vec<u16> = plan.boxes[..5].iter().map(|(_, r)| r.width).collect();
    assert_eq!(widths, [39, 40, 39, 40, 40]);

    // no clusters: GPU and RAM only
    let plan = compute_layout(area, true, 0);
    assert_eq!(kinds(&plan), [Gpu, Ram, CpuPower, GpuPower, AnePower]);
  }

  #[test]
  fn hidden_procs_give_metrics_the_full_height() {
    let area = rect(0, 0, 120, 40);
    let plan = layout(area, false);
    assert_eq!(plan.top, Some(area));
    assert_eq!(plan.proc, None);
    // 38 rows inside: 19 for each row of boxes
    assert!(plan.boxes.iter().all(|(_, r)| r.height == 19), "{plan:?}");
  }

  #[test]
  fn small_screen_auto_hides_procs_by_height() {
    // 100x13: an 8 rows metrics box would leave 5 rows, one short of the smallest process box
    let min = PROC_MIN_ROWS + 3;
    let plan = layout(rect(0, 0, 100, METRICS_MIN_HEIGHT + min - 1), true);
    assert_eq!(plan.proc, None);
    assert_eq!(plan.top, Some(rect(0, 0, 100, 13)), "the metrics take the whole screen");
    let plan = layout(rect(0, 0, 100, METRICS_MIN_HEIGHT + min), true);
    assert_eq!(plan.proc, Some(rect(0, 8, 100, min)));

    // width doesn't matter, only the rows left for the processes
    assert!(layout(rect(0, 0, 20, 40), true).proc.is_some());
  }

  #[test]
  fn rects_stay_inside_and_never_overlap() {
    let widths = [1, 2, 3, 4, 5, 7, 10, 40, 69, 80, 100, 110, 160, 200, 400];
    let heights = [1, 2, 3, 4, 5, 6, 8, 12, 13, 14, 15, 20, 24, 32, 50, 120];

    for (width, height) in widths.into_iter().flat_map(|w| heights.map(|h| (w, h))) {
      for offset in [(0, 0), (3, 2)] {
        let area = rect(offset.0, offset.1, width, height);
        for (clusters, procs) in [0, 2, 3].into_iter().flat_map(|c| [(c, false), (c, true)]) {
          let plan = compute_layout(area, procs, clusters);
          let ctx = format!("{area:?} clusters={clusters} procs={procs}");

          // the metrics box is always on top, the process box right under it to the bottom
          let top = plan.top.expect("metrics box");
          assert_eq!((top.x, top.y, top.width), (area.x, area.y, area.width), "{ctx}");
          assert!(inside(area, top), "{ctx}");
          match plan.proc {
            Some(proc) => {
              assert!(procs, "{ctx}");
              assert_eq!(
                proc,
                Rect { y: top.bottom(), height: area.bottom() - top.bottom(), ..area }
              );
              assert!(proc.height >= PROC_MIN_ROWS + 3, "{ctx}");
            }
            None => assert_eq!(top, area, "{ctx}: metrics take the full height"),
          }

          // boxes sit inside the borders of the metrics box and never overlap
          let inner = top.inner(Margin::new(1, 1));
          for (i, (_, a)) in plan.boxes.iter().enumerate() {
            assert!(!a.is_empty() && inside(inner, *a), "{a:?} outside {inner:?} in {ctx}");
            for (_, b) in &plan.boxes[i + 1..] {
              assert!(!a.intersects(*b), "{a:?} overlaps {b:?} in {ctx}");
            }
          }

          // the boxes of each row tile its width: the top row, then the bottom one (when the
          // metrics box has a row for it)
          let upper = inner.height.div_ceil(2);
          let rows = [(inner.y, upper > 0), (inner.y + upper, inner.height > upper)];
          for (y, shown) in rows {
            let row: Vec<&Rect> =
              plan.boxes.iter().filter(|(_, r)| r.y == y).map(|(_, r)| r).collect();
            let width: u32 = row.iter().map(|r| u32::from(r.width)).sum();
            let ctx = format!("{ctx}: row at {y}");
            assert_eq!(width, if shown { u32::from(inner.width) } else { 0 }, "{ctx}");
            assert!(row.windows(2).all(|w| w[0].right() == w[1].x), "{ctx}: gaps");
          }
        }
      }
    }
  }
}
