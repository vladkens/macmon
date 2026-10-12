//! Screen layout: the metric boxes of the original macmon in the top part of the screen and the
//! process list in the rest.
//!
//! The metrics box holds two rows of boxes: one per CPU cluster, then GPU and RAM on top, CPU /
//! GPU / ANE / total power below. The boxes of a row split its width evenly, the rows split the
//! height (the top row gets the odd one). One structure at every size, only the scale changes.
//! Without power sources (a VM) the power row is left out and the top row takes the height.

use ratatui::layout::{Margin, Rect};

/// Share of the screen height for the metrics box over the process list, in percent.
const METRICS_HEIGHT_PCT: u32 = 40;
/// Lowest metrics box over the process list: two rows of boxes with one graph row each, plus the
/// borders.
const METRICS_MIN_HEIGHT: u16 = 8;
/// Fewest process rows worth showing; with less room the process list is auto-hidden.
const PROC_MIN_ROWS: u16 = 3;
/// Rows of the process box besides the process rows: borders and the table header.
const PROC_CHROME: u16 = 3;

/// One box of the metrics box: a title over a history graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Metric {
  /// CPU cluster, by index into the cluster list.
  Cluster(usize),
  Gpu,
  Ram,
  CpuPower,
  GpuPower,
  AnePower,
  /// System power, or CPU + GPU + ANE without the system power sensor.
  TotalPower,
}

/// Rectangles for one frame. Hidden parts are `None` / left out; every rect is non-empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct LayoutPlan {
  /// Metrics box, borders included.
  pub(super) top: Option<Rect>,
  /// Metric boxes inside the metrics box, borders included: the top row left to right, then the
  /// bottom row.
  pub(super) boxes: Vec<(Metric, Rect)>,
  /// Process box, borders included.
  pub(super) proc: Option<Rect>,
}

/// Boxes of the top row: CPU clusters, GPU, RAM.
fn top_row(clusters: usize) -> Vec<Metric> {
  let mut boxes: Vec<Metric> = (0..clusters).map(Metric::Cluster).collect();
  boxes.extend([Metric::Gpu, Metric::Ram]);
  boxes
}

/// Boxes of the bottom row: CPU, GPU, ANE and total power.
const BOTTOM_ROW: [Metric; 4] =
  [Metric::CpuPower, Metric::GpuPower, Metric::AnePower, Metric::TotalPower];

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

/// Whether `area` has room for the process list under the metrics box: at least `PROC_MIN_ROWS`
/// process rows. The process list setting doesn't matter.
pub(super) fn procs_fit(area: Rect) -> bool {
  !area.is_empty() && area.height - metrics_height(area.height) >= PROC_MIN_ROWS + PROC_CHROME
}

/// Splits `area` into the metrics box and the process box below it.
///
/// With the process list on (`procs`), the metrics box takes `METRICS_HEIGHT_PCT` of the height
/// and the process box the rest; left with fewer than `PROC_MIN_ROWS` process rows, the process
/// list is auto-hidden. Without it, the metrics box takes the whole screen. Without `power`, the
/// top row of boxes takes the whole metrics box.
pub(super) fn compute_layout(area: Rect, procs: bool, clusters: usize, power: bool) -> LayoutPlan {
  let mut plan = LayoutPlan::default();
  if area.is_empty() {
    return plan;
  }

  let mut top = area;
  if procs && procs_fit(area) {
    top.height = metrics_height(area.height);
    plan.proc = Some(Rect { y: top.bottom(), height: area.height - top.height, ..area });
  }
  plan.top = Some(top);

  let inner = top.inner(Margin::new(1, 1));
  let upper = if power { inner.height.div_ceil(2) } else { inner.height };
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

  use super::{LayoutPlan, PROC_CHROME, PROC_MIN_ROWS, compute_layout, procs_fit};

  #[test]
  fn metrics_take_40_percent_and_procs_the_rest() {
    // (width, height, metrics box height): 40 % of the height, rounded, but at least two rows of
    // boxes with a graph row each
    for (width, height, top) in [(200, 50, 20), (110, 32, 13), (80, 24, 10), (100, 15, 8)] {
      let plan = compute_layout(Rect::new(0, 0, width, height), true, 2, true);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(Rect::new(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(Rect::new(0, top, width, height - top)), "{ctx}");
    }
  }

  #[test]
  fn hidden_or_auto_hidden_list_gives_metrics_the_full_height() {
    let area = Rect::new(0, 0, 120, 40);
    let plan = compute_layout(area, false, 2, true);
    assert_eq!((plan.top, plan.proc), (Some(area), None));
    // 38 rows inside: 19 for each row of boxes
    assert!(plan.boxes.iter().all(|(_, r)| r.height == 19), "{plan:?}");

    // 100x13: an 8 rows metrics box would leave 5 rows, one short of the smallest process box
    let small = Rect::new(0, 0, 100, 13);
    let plan = compute_layout(small, true, 2, true);
    assert_eq!((plan.top, plan.proc), (Some(small), None));
    assert!(!procs_fit(small) && procs_fit(Rect { height: 14, ..small }));
  }

  #[test]
  fn without_power_the_top_row_takes_the_power_row_height() {
    use super::Metric::*;
    // 200x50 with the process list: a 20 rows metrics box, 18 rows inside
    let area = Rect::new(0, 0, 200, 50);
    let with = compute_layout(area, true, 1, true);
    let rows =
      |plan: &LayoutPlan| plan.boxes.iter().map(|(m, r)| (*m, r.y, r.height)).collect::<Vec<_>>();
    assert_eq!(
      rows(&with),
      [
        (Cluster(0), 1, 9),
        (Gpu, 1, 9),
        (Ram, 1, 9),
        (CpuPower, 10, 9),
        (GpuPower, 10, 9),
        (AnePower, 10, 9),
        (TotalPower, 10, 9)
      ]
    );

    // the same metrics and process boxes, one row of boxes
    let without = compute_layout(area, true, 1, false);
    assert_eq!((without.top, without.proc), (with.top, with.proc));
    assert_eq!(rows(&without), [(Cluster(0), 1, 18), (Gpu, 1, 18), (Ram, 1, 18)]);
    let widths = |plan: &LayoutPlan| {
      plan.boxes.iter().map(|(_, r)| (r.x, r.width)).take(3).collect::<Vec<_>>()
    };
    assert_eq!(widths(&without), widths(&with));
  }

  #[test]
  fn boxes_stay_inside_and_never_overlap() {
    let widths = [1, 2, 3, 5, 10, 69, 80, 110, 200, 400];
    let heights = [1, 2, 3, 5, 8, 13, 14, 24, 50, 120];
    for (width, height) in widths.into_iter().flat_map(|w| heights.map(|h| (w, h))) {
      let area = Rect::new(3, 2, width, height);
      let cases = [(0, true, true), (2, false, true), (2, true, true), (3, true, true)];
      for (clusters, procs, power) in cases.into_iter().chain([(1, true, false), (2, false, false)])
      {
        let plan = compute_layout(area, procs, clusters, power);
        let ctx = format!("{area:?} clusters={clusters} procs={procs} power={power}");

        // the metrics box on top, the process box right under it to the bottom
        let top = plan.top.expect("metrics box");
        assert_eq!(plan.proc.is_some(), procs && procs_fit(area), "{ctx}");
        match plan.proc {
          Some(proc) => {
            let rest = Rect { y: top.bottom(), height: area.bottom() - top.bottom(), ..area };
            assert_eq!(proc, rest, "{ctx}");
            assert!(proc.height >= PROC_MIN_ROWS + PROC_CHROME, "{ctx}");
          }
          None => assert_eq!(top, area, "{ctx}: metrics take the full height"),
        }

        // boxes sit inside the borders of the metrics box and never overlap
        let inner = top.inner(Margin::new(1, 1));
        for (i, (_, a)) in plan.boxes.iter().enumerate() {
          assert!(!a.is_empty() && inner.intersection(*a) == *a, "{a:?} out of {inner:?}: {ctx}");
          for (_, b) in &plan.boxes[i + 1..] {
            assert!(!a.intersects(*b), "{a:?} overlaps {b:?} in {ctx}");
          }
        }
      }
    }
  }
}
