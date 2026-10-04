//! Screen layout: which panels are shown and where.
//!
//! The CPU box spans the full width on top; below it the GPU / MEM / POWER boxes are stacked in
//! the left column and the process list takes the right side.

use std::cmp::Reverse;

use ratatui::layout::{Margin, Rect};

use crate::config::Panels;

/// Narrowest terminal that shows the process panel next to other panels.
pub const PROC_MIN_WIDTH: u16 = 100;
/// Lowest terminal that shows the process panel next to other panels.
pub const PROC_MIN_HEIGHT: u16 = 20;
/// CPU box height floor when it shares the screen: borders + 2 rows per E-CPU / P-CPU graph.
const CPU_MIN_HEIGHT: u16 = 6;
/// Preferred POWER box height: borders + CPU / GPU / ANE rows + SYS / fans footer.
const POWER_HEIGHT: u16 = 6;
/// Smallest box with one row inside its borders.
const BOX_MIN_HEIGHT: u16 = 3;
/// Width of the left column next to the process panel, in percent of the screen width.
const LEFT_WIDTH_PCT: u32 = 40;

/// Box rectangles for one frame. `None` means the panel is hidden (by the user, auto-hidden or
/// squeezed to nothing); every `Some` rect is non-empty.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LayoutPlan {
  /// CPU box, borders included.
  pub cpu: Option<Rect>,
  /// E-CPU / P-CPU graphs, inside the CPU box borders.
  pub cpu_graphs: Option<Rect>,
  /// Per-core meter grid, inside the CPU box borders right of the graphs (`d` toggles it).
  pub cores: Option<Rect>,
  pub gpu: Option<Rect>,
  pub mem: Option<Rect>,
  pub power: Option<Rect>,
  pub proc: Option<Rect>,
}

impl LayoutPlan {
  /// Top-level boxes (cpu, gpu, mem, power, proc) that are visible.
  pub fn boxes(&self) -> impl Iterator<Item = Rect> {
    [self.cpu, self.gpu, self.mem, self.power, self.proc].into_iter().flatten()
  }

  /// Box whose bottom border holds the global key hints: the lowest one, leftmost on ties.
  pub fn bottom_left(&self) -> Option<Rect> {
    self.boxes().min_by_key(|r| (Reverse(r.bottom()), r.x))
  }
}

fn non_empty(rect: Rect) -> Option<Rect> {
  if rect.is_empty() { None } else { Some(rect) }
}

/// Splits `area` into panel boxes. The process panel is auto-hidden when the screen is smaller
/// than `PROC_MIN_WIDTH` x `PROC_MIN_HEIGHT`, unless it is the only visible panel.
pub fn compute_layout(area: Rect, panels: Panels, per_core: bool) -> LayoutPlan {
  let mut plan = LayoutPlan::default();
  if area.is_empty() {
    return plan;
  }

  let left = panels.gpu || panels.mem || panels.power;
  let fits_proc = area.width >= PROC_MIN_WIDTH && area.height >= PROC_MIN_HEIGHT;
  let proc = panels.proc && (fits_proc || !(panels.cpu || left));

  let mut rest = area;
  if panels.cpu {
    let height = if left || proc {
      (area.height / 3).max(CPU_MIN_HEIGHT).min(area.height)
    } else {
      area.height
    };

    let cpu = Rect { height, ..area };
    plan.cpu = Some(cpu);
    (plan.cpu_graphs, plan.cores) = split_cpu(cpu, per_core);
    rest = Rect { y: area.y + height, height: area.height - height, ..area };
  }

  if rest.is_empty() {
    return plan;
  }

  let column = match (left, proc) {
    (true, true) => {
      let width = (u32::from(rest.width) * LEFT_WIDTH_PCT / 100) as u16;
      plan.proc = non_empty(Rect { x: rest.x + width, width: rest.width - width, ..rest });
      Rect { width, ..rest }
    }
    (true, false) => rest,
    (false, true) => {
      plan.proc = Some(rest);
      return plan;
    }
    (false, false) => return plan,
  };

  [plan.gpu, plan.mem, plan.power] = split_column(column, panels);
  plan
}

/// Splits the inner area of the CPU box into graphs (left) and the per-core grid (right half).
fn split_cpu(cpu: Rect, per_core: bool) -> (Option<Rect>, Option<Rect>) {
  let inner = cpu.inner(Margin::new(1, 1));
  if inner.is_empty() || !per_core {
    return (non_empty(inner), None);
  }

  let cores_width = inner.width / 2;
  let graphs = Rect { width: inner.width - cores_width, ..inner };
  let cores = Rect { x: graphs.right(), width: cores_width, ..inner };
  (Some(graphs), non_empty(cores))
}

/// Stacks the GPU, MEM and POWER boxes in `area`. POWER keeps its preferred height as long as the
/// other boxes still get a row inside their borders; GPU and MEM share the rest equally.
fn split_column(area: Rect, panels: Panels) -> [Option<Rect>; 3] {
  let count = [panels.gpu, panels.mem, panels.power].iter().filter(|shown| **shown).count() as u16;
  if count == 0 || area.is_empty() {
    return [None; 3];
  }

  let total = area.height;
  let power_h = match (panels.power, count) {
    (false, _) => 0,
    (true, 1) => total,
    (true, _) => {
      // in a tiny column every box gets an equal share instead
      let others_min = (count - 1) * BOX_MIN_HEIGHT;
      POWER_HEIGHT.min(total.saturating_sub(others_min).max(total / count))
    }
  };

  let rest = total - power_h;
  let gpu_h = match (panels.gpu, panels.mem) {
    (false, _) => 0,
    (true, true) => rest / 2,
    (true, false) => rest,
  };
  let mem_h = if panels.mem { rest - gpu_h } else { 0 };

  let mut y = area.y;
  [gpu_h, mem_h, power_h].map(|height| {
    let rect = Rect { y, height, ..area };
    y += height;
    non_empty(rect)
  })
}

#[cfg(test)]
mod tests {
  use ratatui::layout::Rect;

  use super::{LayoutPlan, PROC_MIN_HEIGHT, PROC_MIN_WIDTH, compute_layout};
  use crate::config::Panels;

  const ALL: Panels = Panels { cpu: true, gpu: true, mem: true, power: true, proc: true };
  const NONE: Panels = Panels { cpu: false, gpu: false, mem: false, power: false, proc: false };

  fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
  }

  /// Every combination of the 5 panel flags.
  fn all_panel_sets() -> impl Iterator<Item = Panels> {
    (0..32u8).map(|bits| Panels {
      cpu: bits & 1 != 0,
      gpu: bits & 2 != 0,
      mem: bits & 4 != 0,
      power: bits & 8 != 0,
      proc: bits & 16 != 0,
    })
  }

  fn inside(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
      && inner.y >= outer.y
      && inner.right() <= outer.right()
      && inner.bottom() <= outer.bottom()
  }

  #[test]
  fn large_screen_shows_all_panels() {
    let plan = compute_layout(rect(0, 0, 200, 50), ALL, true);

    assert_eq!(plan.cpu, Some(rect(0, 0, 200, 16)));
    assert_eq!(plan.cpu_graphs, Some(rect(1, 1, 99, 14)));
    assert_eq!(plan.cores, Some(rect(100, 1, 99, 14)));

    // left column is 40% wide, GPU and MEM share what POWER leaves
    assert_eq!(plan.gpu, Some(rect(0, 16, 80, 14)));
    assert_eq!(plan.mem, Some(rect(0, 30, 80, 14)));
    assert_eq!(plan.power, Some(rect(0, 44, 80, 6)));
    assert_eq!(plan.proc, Some(rect(80, 16, 120, 34)));
    assert_eq!(plan.bottom_left(), plan.power);
  }

  #[test]
  fn small_screen_auto_hides_proc() {
    let plan = compute_layout(rect(0, 0, 80, 24), ALL, true);

    assert_eq!(plan.proc, None);
    assert_eq!(plan.cpu, Some(rect(0, 0, 80, 8)));
    // left column takes the full width
    assert_eq!(plan.gpu, Some(rect(0, 8, 80, 5)));
    assert_eq!(plan.mem, Some(rect(0, 13, 80, 5)));
    assert_eq!(plan.power, Some(rect(0, 18, 80, 6)));
  }

  #[test]
  fn proc_auto_hide_thresholds() {
    let fits = rect(0, 0, PROC_MIN_WIDTH, PROC_MIN_HEIGHT);
    assert!(compute_layout(fits, ALL, false).proc.is_some());

    let narrow = rect(0, 0, PROC_MIN_WIDTH - 1, 50);
    assert!(compute_layout(narrow, ALL, false).proc.is_none());

    let low = rect(0, 0, 200, PROC_MIN_HEIGHT - 1);
    assert!(compute_layout(low, ALL, false).proc.is_none());
  }

  #[test]
  fn only_proc_takes_full_screen() {
    let panels = Panels { proc: true, ..NONE };

    // never auto-hidden when it is the only panel
    for area in [rect(0, 0, 200, 50), rect(0, 0, 80, 24), rect(0, 0, 30, 5)] {
      let plan = compute_layout(area, panels, true);
      assert_eq!(plan, LayoutPlan { proc: Some(area), ..LayoutPlan::default() });
      assert_eq!(plan.bottom_left(), Some(area));
    }
  }

  #[test]
  fn only_cpu_takes_full_screen() {
    let panels = Panels { cpu: true, ..NONE };
    let area = rect(0, 0, 120, 40);

    let plan = compute_layout(area, panels, true);
    assert_eq!(plan.cpu, Some(area));
    assert_eq!(plan.cpu_graphs, Some(rect(1, 1, 59, 38)));
    assert_eq!(plan.cores, Some(rect(60, 1, 59, 38)));
    assert_eq!(plan.boxes().count(), 1);
    assert_eq!(plan.bottom_left(), Some(area));

    // also when the process panel is on but auto-hidden
    let plan = compute_layout(rect(0, 0, 80, 24), Panels { proc: true, ..panels }, false);
    assert_eq!(plan.cpu, Some(rect(0, 0, 80, 24)));
    assert_eq!(plan.proc, None);
  }

  #[test]
  fn cpu_and_proc_without_left_column() {
    let panels = Panels { cpu: true, proc: true, ..NONE };
    let plan = compute_layout(rect(0, 0, 120, 40), panels, false);

    assert_eq!(plan.cpu, Some(rect(0, 0, 120, 13)));
    assert_eq!(plan.proc, Some(rect(0, 13, 120, 27)));
    assert_eq!(plan.bottom_left(), plan.proc);
  }

  #[test]
  fn hidden_cpu_gives_column_full_height() {
    let panels = Panels { cpu: false, ..ALL };
    let plan = compute_layout(rect(0, 0, 200, 50), panels, true);

    assert_eq!((plan.cpu, plan.cpu_graphs, plan.cores), (None, None, None));
    assert_eq!(plan.gpu, Some(rect(0, 0, 80, 22)));
    assert_eq!(plan.mem, Some(rect(0, 22, 80, 22)));
    assert_eq!(plan.power, Some(rect(0, 44, 80, 6)));
    assert_eq!(plan.proc, Some(rect(80, 0, 120, 50)));
  }

  #[test]
  fn all_hidden_is_empty() {
    for area in [rect(0, 0, 200, 50), rect(0, 0, 80, 24)] {
      let plan = compute_layout(area, NONE, true);
      assert_eq!(plan, LayoutPlan::default());
      assert_eq!(plan.bottom_left(), None);
    }
  }

  #[test]
  fn per_core_off_gives_graphs_full_width() {
    let plan = compute_layout(rect(0, 0, 200, 50), ALL, false);
    assert_eq!(plan.cpu_graphs, Some(rect(1, 1, 198, 14)));
    assert_eq!(plan.cores, None);
  }

  #[test]
  fn power_shrinks_before_other_boxes() {
    let panels = Panels { cpu: false, proc: false, ..ALL };

    let plan = compute_layout(rect(0, 0, 60, 10), panels, false);
    let heights = [plan.gpu, plan.mem, plan.power].map(|r| r.map(|r| r.height));
    assert_eq!(heights, [Some(3), Some(3), Some(4)]);

    // POWER alone fills the column, GPU without MEM gets everything POWER leaves
    let plan = compute_layout(rect(0, 0, 60, 30), Panels { power: true, ..NONE }, false);
    assert_eq!(plan.power, Some(rect(0, 0, 60, 30)));

    let plan = compute_layout(rect(0, 0, 60, 30), Panels { gpu: true, power: true, ..NONE }, false);
    assert_eq!((plan.gpu, plan.power), (Some(rect(0, 0, 60, 24)), Some(rect(0, 24, 60, 6))));
  }

  #[test]
  fn bottom_left_skips_hidden_boxes() {
    let area = rect(0, 0, 200, 50);
    let plan = compute_layout(area, Panels { power: false, ..ALL }, false);
    assert_eq!(plan.bottom_left(), plan.mem);

    let plan = compute_layout(area, Panels { power: false, mem: false, ..ALL }, false);
    assert_eq!(plan.bottom_left(), plan.gpu);
  }

  #[test]
  fn rects_stay_inside_and_never_overlap() {
    let widths = [1, 2, 10, 40, 79, 80, 99, 100, 101, 160, 200, 400];
    let heights = [1, 2, 3, 5, 6, 8, 15, 19, 20, 24, 40, 50, 120];

    for (width, height) in widths.into_iter().flat_map(|w| heights.map(|h| (w, h))) {
      for offset in [(0, 0), (3, 2)] {
        let area = rect(offset.0, offset.1, width, height);
        for panels in all_panel_sets() {
          for per_core in [false, true] {
            let plan = compute_layout(area, panels, per_core);
            let ctx = format!("{area:?} {panels:?} per_core={per_core}");

            let boxes: Vec<Rect> = plan.boxes().collect();
            for (i, a) in boxes.iter().enumerate() {
              assert!(!a.is_empty(), "empty box {a:?} in {ctx}");
              assert!(inside(area, *a), "{a:?} outside {ctx}");
              for b in &boxes[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?} in {ctx}");
              }
            }

            // something is shown unless every panel is off, and visible boxes tile the screen
            assert_eq!(boxes.is_empty(), panels == NONE, "{ctx}");
            if !boxes.is_empty() {
              let covered: u32 = boxes.iter().map(|r| r.area()).sum();
              assert_eq!(covered, area.area(), "gaps in {ctx}");
            }

            // CPU box parts sit inside its borders and side by side
            for part in [plan.cpu_graphs, plan.cores].into_iter().flatten() {
              let cpu = plan.cpu.expect("cpu parts without cpu box");
              assert!(!part.is_empty() && inside(cpu, part), "{part:?} outside cpu in {ctx}");
            }
            if let (Some(graphs), Some(cores)) = (plan.cpu_graphs, plan.cores) {
              assert!(!graphs.intersects(cores), "graphs overlap cores in {ctx}");
            }
            if !per_core {
              assert_eq!(plan.cores, None);
            }

            // hidden panels stay hidden
            assert!(panels.cpu || plan.cpu.is_none());
            assert!(panels.gpu || plan.gpu.is_none());
            assert!(panels.mem || plan.mem.is_none());
            assert!(panels.power || plan.power.is_none());
            assert!(panels.proc || plan.proc.is_none());
          }
        }
      }
    }
  }
}
