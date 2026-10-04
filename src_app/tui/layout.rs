//! Screen layout: a full-width metrics box on top and the process list below it.
//!
//! The metrics box holds one strip per row on the left (CPU clusters, GPU, RAM, SWAP, then the
//! per-core bars) and the power column on the right, or under the strips on narrow screens.

use std::collections::BTreeSet;
use std::ops::Range;

use ratatui::layout::{Margin, Rect};

use crate::config::Panels;

/// Share of the screen height taken by the process list, in percent.
pub const PROC_HEIGHT_PCT: u32 = 60;
/// Fewest process rows worth showing; with less room the process list is auto-hidden.
pub const PROC_MIN_ROWS: u16 = 3;
/// Rows of the process box besides the process rows: borders and the table header.
const PROC_CHROME: u16 = 3;
/// Width of the power column next to the strips.
pub const POWER_WIDTH: u16 = 30;
/// Narrowest metrics box with the power column next to the strips; narrower boxes put the power
/// rows under the strips.
pub const POWER_SIDE_MIN_WIDTH: u16 = 70;
/// Cells between the strips and the power column: ` │ `.
const POWER_GAP: u16 = 3;
/// Blank cells inside the left and right borders of the metrics box.
const PADDING: u16 = 1;
/// Blank cells between the core bars of two clusters on one line.
pub const CORE_RUN_GAP: usize = 2;

/// One strip of the metrics box: `{label} {pct}% {detail} {graph or meter}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strip {
  /// CPU cluster, by index into the cluster list.
  Cluster(usize),
  Gpu,
  Ram,
  Swap,
}

impl Strip {
  /// History graph strips get the spare rows of the metrics box; meters stay one row high.
  fn grows(self) -> bool {
    matches!(self, Self::Cluster(_) | Self::Gpu)
  }
}

/// Cores of one CPU cluster: its label and the die of every core, in core order (grouped by die).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterCores<'a> {
  pub label: &'a str,
  pub dies: Vec<usize>,
}

/// Bars of one cluster's cores on a cores line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRun {
  /// Index into the cluster list.
  pub cluster: usize,
  /// Indexes into the cluster's cores.
  pub cores: Range<usize>,
  /// Cluster label before the bars; a wrapped cluster's later lines leave its cells blank.
  pub label: bool,
}

/// One line of core bars, optionally for one die (`D1 E ▃▅▂▁  P ▇▆█▅`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreLine {
  /// Die shown as `D{n}` at the start of the line.
  pub die: Option<usize>,
  pub runs: Vec<CoreRun>,
}

/// Die prefix of a cores line.
pub fn die_label(die: usize) -> String {
  format!("D{die}")
}

impl CoreLine {
  /// Cells taken by the line.
  pub fn width(&self, clusters: &[ClusterCores]) -> usize {
    let prefix = self.die.map_or(0, |die| die_label(die).len() + 1);
    let runs: usize = self
      .runs
      .iter()
      .map(|run| clusters[run.cluster].label.chars().count() + 1 + run.cores.len())
      .sum();
    prefix + runs + CORE_RUN_GAP * self.runs.len().saturating_sub(1)
  }
}

/// Splits the core bars into lines `width` cells wide: everything on one line when it fits, then
/// one line per die (multi-die chips), then one line per cluster (and die), and finally clusters
/// wrapped over several lines.
pub fn core_lines(width: u16, clusters: &[ClusterCores]) -> Vec<CoreLine> {
  let width = usize::from(width);
  let fits = |lines: &[CoreLine]| lines.iter().all(|line| line.width(clusters) <= width);

  // runs of every cluster, of one die or of all of them
  let runs = |die: Option<usize>| -> Vec<CoreRun> {
    let runs = clusters.iter().enumerate().filter_map(|(cluster, c)| {
      let cores = match die {
        None => 0..c.dies.len(),
        Some(die) => {
          let start = c.dies.iter().position(|&d| d == die)?;
          start..start + c.dies[start..].iter().take_while(|&&d| d == die).count()
        }
      };
      (!cores.is_empty()).then_some(CoreRun { cluster, cores, label: true })
    });
    runs.collect()
  };

  let one = vec![CoreLine { die: None, runs: runs(None) }];
  if one[0].runs.is_empty() {
    return vec![];
  }
  if fits(&one) {
    return one;
  }

  let dies: BTreeSet<usize> = clusters.iter().flat_map(|c| c.dies.iter().copied()).collect();
  let lines = if dies.len() > 1 {
    let per_die: Vec<CoreLine> =
      dies.into_iter().map(|die| CoreLine { die: Some(die), runs: runs(Some(die)) }).collect();
    if fits(&per_die) {
      return per_die;
    }
    per_die
  } else {
    one
  };

  let split: Vec<CoreLine> = lines
    .into_iter()
    .flat_map(|line| {
      let die = line.die;
      line.runs.into_iter().map(move |run| CoreLine { die, runs: vec![run] })
    })
    .collect();
  if fits(&split) {
    return split;
  }

  split.into_iter().flat_map(|line| wrap_core_line(line, width, clusters)).collect()
}

/// Wraps a one-cluster line over as few lines as fit in `width`, with balanced bar counts.
fn wrap_core_line(line: CoreLine, width: usize, clusters: &[ClusterCores]) -> Vec<CoreLine> {
  let lead = line.width(clusters) - line.runs[0].cores.len();
  let CoreLine { die, runs } = line;
  let run = &runs[0];

  let room = width.saturating_sub(lead).max(1);
  let count = run.cores.len().div_ceil(room);
  let chunk = run.cores.len().div_ceil(count);
  let end = run.cores.end;

  let starts = run.cores.clone().step_by(chunk).enumerate();
  let chunks = starts.map(|(i, start)| {
    let cores = start..(start + chunk).min(end);
    CoreLine { die, runs: vec![CoreRun { cluster: run.cluster, cores, label: i == 0 }] }
  });
  chunks.collect()
}

/// What the metrics box has to show, besides the panel flags.
#[derive(Debug, Default)]
pub struct Content<'a> {
  /// CPU clusters, lowest tier first: one strip each and their cores on the cores lines.
  pub clusters: &'a [ClusterCores<'a>],
  /// SWAP strip under RAM (only when swap is configured).
  pub swap: bool,
  /// Rows of the power column.
  pub power_rows: u16,
  /// Cells of a cores line before the bars (the strip label column).
  pub cores_indent: u16,
}

/// Rectangles for one frame. Hidden parts are `None` / empty; every rect is non-empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
  /// Metrics box, borders included.
  pub top: Option<Rect>,
  /// Strips inside the metrics box, top to bottom.
  pub strips: Vec<(Strip, Rect)>,
  /// Lines of per-core bars under the strips (`d`), each the width of the strips.
  pub cores: Vec<(CoreLine, Rect)>,
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

fn rows(count: usize) -> u16 {
  u16::try_from(count).unwrap_or(u16::MAX)
}

/// Strips of the visible panels, top to bottom.
fn strips(panels: Panels, content: &Content) -> Vec<Strip> {
  let mut strips = vec![];
  if panels.cpu {
    strips.extend((0..content.clusters.len()).map(Strip::Cluster));
  }
  if panels.gpu {
    strips.push(Strip::Gpu);
  }
  if panels.mem {
    strips.push(Strip::Ram);
    if content.swap {
      strips.push(Strip::Swap);
    }
  }
  strips
}

/// Splits `area` into the metrics box on top and the process box below it.
///
/// The process box takes `PROC_HEIGHT_PCT` of the height, the metrics box the rest but never less
/// than its content needs; a process box left with fewer than `PROC_MIN_ROWS` process rows is
/// auto-hidden. Spare rows of the metrics box make the graph strips taller; without graph strips
/// the metrics box keeps only the rows it needs.
pub fn compute_layout(area: Rect, panels: Panels, per_core: bool, content: &Content) -> LayoutPlan {
  let mut plan = LayoutPlan::default();
  if area.is_empty() {
    return plan;
  }

  if !(panels.cpu || panels.gpu || panels.mem || panels.power) {
    plan.proc = panels.proc.then_some(area);
    return plan;
  }

  let strips = strips(panels, content);
  let power_rows = if panels.power { content.power_rows } else { 0 };
  let inner_width = area.width.saturating_sub(2 + 2 * PADDING);
  let side = panels.power && !strips.is_empty() && area.width >= POWER_SIDE_MIN_WIDTH;
  let left_width =
    if side { inner_width.saturating_sub(POWER_WIDTH + POWER_GAP) } else { inner_width };

  let core_lines = if panels.cpu && per_core {
    core_lines(left_width.saturating_sub(content.cores_indent), content.clusters)
  } else {
    vec![]
  };

  let left_rows = rows(strips.len() + core_lines.len());
  let content_rows =
    if side { left_rows.max(power_rows) } else { left_rows.saturating_add(power_rows) };
  let top_min = content_rows.max(1).saturating_add(2);
  let growing = rows(strips.iter().filter(|s| s.grows()).count());

  let mut top = area;
  if panels.proc {
    // without graphs to grow, the metrics box keeps only the rows it needs
    let proc_height = (u32::from(area.height) * PROC_HEIGHT_PCT / 100) as u16;
    let share = if growing > 0 { area.height - proc_height } else { 0 };
    let top_height = share.max(top_min).min(area.height);
    let proc_height = area.height - top_height;
    if proc_height >= PROC_MIN_ROWS + PROC_CHROME {
      top.height = top_height;
      plan.proc = Some(Rect { y: area.y + top_height, height: proc_height, ..area });
    }
  }
  plan.top = Some(top);

  let inner = top.inner(Margin::new(1 + PADDING, 1));
  if inner.is_empty() {
    return plan;
  }

  let left = if side {
    let left = Rect { width: left_width, ..inner };
    plan.separator = non_empty(Rect { x: left.right() + 1, width: 1, ..inner });
    plan.power = non_empty(Rect { x: inner.right() - POWER_WIDTH, width: POWER_WIDTH, ..inner });
    left
  } else {
    // the strips keep their rows first, power rows go under them; spare rows go to the graphs,
    // or stay below the power rows when there are none
    let fixed = left_rows.min(inner.height);
    let height =
      if growing > 0 { inner.height.saturating_sub(power_rows).max(fixed) } else { fixed };
    if panels.power {
      plan.power = non_empty(Rect { y: inner.y + height, height: inner.height - height, ..inner });
    }
    Rect { height, ..inner }
  };

  let spare = left.height.saturating_sub(left_rows);
  let mut y = left.y;
  let mut grown = 0;
  for strip in strips {
    let mut height = 1;
    if strip.grows() {
      height += spare / growing + u16::from(grown < spare % growing);
      grown += 1;
    }

    let height = height.min(left.bottom().saturating_sub(y));
    if height == 0 {
      return plan;
    }
    plan.strips.push((strip, Rect { y, height, ..left }));
    y += height;
  }

  for line in core_lines {
    if y >= left.bottom() {
      break;
    }
    plan.cores.push((line, Rect { y, height: 1, ..left }));
    y += 1;
  }

  plan
}

#[cfg(test)]
mod tests {
  use std::ops::Range;

  use ratatui::layout::{Margin, Rect};

  use super::{
    ClusterCores, Content, CoreLine, CoreRun, LayoutPlan, POWER_SIDE_MIN_WIDTH, POWER_WIDTH,
    PROC_MIN_ROWS, Strip, compute_layout, core_lines,
  };
  use crate::config::Panels;

  const ALL: Panels = Panels { cpu: true, gpu: true, mem: true, power: true, proc: true };
  const NONE: Panels = Panels { cpu: false, gpu: false, mem: false, power: false, proc: false };

  fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
  }

  /// Cluster `label` with `per_die` cores on each of `dies` dies.
  fn cluster(label: &str, per_die: usize, dies: usize) -> ClusterCores<'_> {
    ClusterCores { label, dies: (0..dies).flat_map(|die| vec![die; per_die]).collect() }
  }

  /// M3 Pro: 6E + 6P on one die.
  fn m3_pro() -> Vec<ClusterCores<'static>> {
    vec![cluster("E", 6, 1), cluster("P", 6, 1)]
  }

  fn content<'a>(clusters: &'a [ClusterCores<'a>]) -> Content<'a> {
    Content { clusters, swap: true, power_rows: 5, cores_indent: 7 }
  }

  fn layout(area: Rect, panels: Panels, per_core: bool) -> LayoutPlan {
    compute_layout(area, panels, per_core, &content(&m3_pro()))
  }

  fn strip_kinds(plan: &LayoutPlan) -> Vec<Strip> {
    plan.strips.iter().map(|(strip, _)| *strip).collect()
  }

  fn heights(plan: &LayoutPlan) -> Vec<u16> {
    plan.strips.iter().map(|(_, r)| r.height).collect()
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
  fn large_screen_splits_metrics_and_procs() {
    let plan = layout(rect(0, 0, 200, 50), ALL, false);

    // the process list takes 60% of the height, full width
    assert_eq!(plan.top, Some(rect(0, 0, 200, 20)));
    assert_eq!(plan.proc, Some(rect(0, 20, 200, 30)));
    assert_eq!(plan.bottom(), plan.proc);

    // strips left with one padding cell, power column right, separator between them
    use Strip::*;
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Gpu, Ram, Swap]);
    assert_eq!(plan.strips[0].1, rect(2, 1, 163, 6));
    assert_eq!(plan.separator, Some(rect(166, 1, 1, 18)));
    assert_eq!(plan.power, Some(rect(168, 1, POWER_WIDTH, 18)));

    // 13 spare rows go to the three graph strips, meters stay one row high
    assert_eq!(heights(&plan), [6, 5, 5, 1, 1]);
    assert_eq!(plan.strips.last().unwrap().1.bottom(), 19);
    assert!(plan.cores.is_empty());
  }

  #[test]
  fn common_sizes_give_procs_60_pct() {
    // (width, height, top box height)
    for (width, height, top) in [(200, 50, 20), (120, 40, 16), (100, 30, 12), (80, 24, 10)] {
      let plan = layout(rect(0, 0, width, height), ALL, true);
      let ctx = format!("{width}x{height}");
      assert_eq!(plan.top, Some(rect(0, 0, width, top)), "{ctx}");
      assert_eq!(plan.proc, Some(rect(0, top, width, height - top)), "{ctx}");
      assert_eq!(strip_kinds(&plan).len(), 5, "{ctx}");
      assert_eq!(plan.cores.len(), 1, "{ctx}");
      assert!(plan.separator.is_some(), "{ctx}: power column on the side");
    }

    // 72x24: still room for the power column next to the strips
    let plan = layout(rect(0, 0, 72, 24), ALL, true);
    assert_eq!(plan.proc, Some(rect(0, 10, 72, 14)));
    assert_eq!(plan.power, Some(rect(40, 1, POWER_WIDTH, 8)));
    assert_eq!(plan.strips[0].1.width, 35);
  }

  #[test]
  fn narrow_screen_moves_power_under_strips() {
    let width = POWER_SIDE_MIN_WIDTH - 1;

    // 5 strips + 5 power rows need 12 rows with the borders, more than 40% of 25
    let plan = layout(rect(0, 0, width, 25), ALL, false);
    assert_eq!(plan.separator, None);
    assert_eq!(plan.top, Some(rect(0, 0, width, 12)));
    assert_eq!(plan.proc, Some(rect(0, 12, width, 13)));
    assert_eq!(heights(&plan), [1, 1, 1, 1, 1]);
    assert_eq!(plan.power, Some(rect(2, 6, width - 4, 5)));

    // spare rows grow the graphs, the power rows stay right under the strips
    let plan = layout(rect(0, 0, width, 40), ALL, false);
    assert_eq!(plan.top, Some(rect(0, 0, width, 16)));
    assert_eq!(heights(&plan), [3, 2, 2, 1, 1]);
    assert_eq!(plan.power, Some(rect(2, 10, width - 4, 5)));
  }

  #[test]
  fn small_screen_auto_hides_procs_by_height() {
    // 60x15: the metrics need 12 rows, the process box would get 3
    let plan = layout(rect(0, 0, 60, 15), ALL, false);
    assert_eq!(plan.proc, None);
    assert_eq!(plan.top, Some(rect(0, 0, 60, 15)));
    assert_eq!(plan.bottom(), plan.top);

    // width doesn't matter, only the rows left for the processes
    let plan = layout(rect(0, 0, 40, 40), ALL, false);
    assert!(plan.proc.is_some());

    // threshold: borders + header + PROC_MIN_ROWS process rows under a 7 rows metrics box (5
    // strips next to 5 power rows)
    let (top, min) = (7, PROC_MIN_ROWS + 3);
    let plan = layout(rect(0, 0, 100, top + min), ALL, false);
    assert_eq!(plan.top.map(|r| r.height), Some(top));
    assert_eq!(plan.proc.map(|r| r.height), Some(min));
    let plan = layout(rect(0, 0, 100, top + min - 1), ALL, false);
    assert_eq!(plan.proc, None);
  }

  #[test]
  fn hidden_procs_give_metrics_full_height() {
    let area = rect(0, 0, 120, 40);
    let plan = layout(area, Panels { proc: false, ..ALL }, false);
    assert_eq!(plan.top, Some(area));
    assert_eq!(plan.proc, None);
    // spare rows: 38 inside, 2 meters, 36 for the 3 graphs
    assert_eq!(heights(&plan), [12, 12, 12, 1, 1]);
  }

  #[test]
  fn hidden_metrics_give_procs_full_height() {
    let panels = Panels { proc: true, ..NONE };
    // never auto-hidden when it is the only panel
    for area in [rect(0, 0, 200, 50), rect(0, 0, 80, 24), rect(0, 0, 30, 5)] {
      let plan = layout(area, panels, true);
      assert_eq!(plan, LayoutPlan { proc: Some(area), ..LayoutPlan::default() });
      assert_eq!(plan.bottom(), Some(area));
    }
  }

  #[test]
  fn all_hidden_is_empty() {
    for area in [rect(0, 0, 200, 50), rect(0, 0, 80, 24)] {
      let plan = layout(area, NONE, true);
      assert_eq!(plan, LayoutPlan::default());
      assert_eq!(plan.bottom(), None);
    }
  }

  #[test]
  fn panel_flags_pick_rows() {
    use Strip::*;
    let area = rect(0, 0, 120, 40);
    let cases = [
      (Panels { cpu: false, ..ALL }, vec![Gpu, Ram, Swap]),
      (Panels { gpu: false, ..ALL }, vec![Cluster(0), Cluster(1), Ram, Swap]),
      (Panels { mem: false, ..ALL }, vec![Cluster(0), Cluster(1), Gpu]),
      (Panels { power: false, ..ALL }, vec![Cluster(0), Cluster(1), Gpu, Ram, Swap]),
    ];
    for (panels, strips) in cases {
      let plan = layout(area, panels, true);
      assert_eq!(strip_kinds(&plan), strips, "{panels:?}");
      assert_eq!(plan.cores.is_empty(), !panels.cpu, "{panels:?}: cores follow the cpu panel");
      assert_eq!(plan.power.is_some(), panels.power, "{panels:?}");
    }

    // no power column: strips take the full width
    let plan = layout(area, Panels { power: false, ..ALL }, false);
    assert_eq!(plan.strips[0].1, rect(2, 1, 116, 4));
    assert_eq!(plan.separator, None);

    // only the power column: full width, no separator; nothing to grow, so the box keeps only
    // the rows it needs
    let plan = layout(area, Panels { power: true, proc: true, ..NONE }, false);
    assert_eq!(plan.power, Some(rect(2, 1, 116, 5)));
    assert_eq!(plan.proc, Some(rect(0, 7, 120, 33)));
    assert!(plan.strips.is_empty() && plan.separator.is_none());

    // same with meters only
    let plan = layout(area, Panels { mem: true, power: true, proc: true, ..NONE }, false);
    assert_eq!(heights(&plan), [1, 1]);
    assert_eq!(plan.top, Some(rect(0, 0, 120, 7)));

    // no swap configured: no SWAP strip
    let clusters = m3_pro();
    let plan = compute_layout(area, ALL, false, &Content { swap: false, ..content(&clusters) });
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Gpu, Ram]);
  }

  #[test]
  fn power_rows_grow_short_strips() {
    // GPU alone next to 5 power rows: the GPU graph gets all 5 rows
    let panels = Panels { gpu: true, power: true, ..NONE };
    let plan = layout(rect(0, 0, 100, 7), panels, false);
    assert_eq!(plan.strips, [(Strip::Gpu, rect(2, 1, 63, 5))]);

    // meters don't grow: power rows sit right under them on narrow screens
    let panels = Panels { mem: true, power: true, ..NONE };
    let plan = layout(rect(0, 0, 60, 20), panels, false);
    assert_eq!(heights(&plan), [1, 1]);
    assert_eq!(plan.power, Some(rect(2, 3, 56, 16)));
  }

  #[test]
  fn three_clusters_get_a_strip_each() {
    let clusters = vec![cluster("E", 6, 1), cluster("P", 4, 1), cluster("S", 2, 1)];
    let plan = compute_layout(rect(0, 0, 100, 30), ALL, true, &content(&clusters));

    use Strip::*;
    assert_eq!(strip_kinds(&plan), [Cluster(0), Cluster(1), Cluster(2), Gpu, Ram, Swap]);
    let runs: Vec<usize> = plan.cores[0].0.runs.iter().map(|run| run.cluster).collect();
    assert_eq!(runs, [0, 1, 2]);
  }

  #[test]
  fn core_lines_add_rows_to_metrics_box() {
    // M3 Ultra (8E + 24P on 2 dies) at 72 columns: one line per die
    let clusters = vec![cluster("E", 4, 2), cluster("P", 12, 2)];
    let plan = compute_layout(rect(0, 0, 72, 24), ALL, true, &content(&clusters));
    let dies: Vec<Option<usize>> = plan.cores.iter().map(|(line, _)| line.die).collect();
    assert_eq!(dies, [Some(0), Some(1)]);

    // the lines sit under the strips, the top box keeps its 40%
    let last_strip = plan.strips.last().unwrap().1;
    assert_eq!(plan.cores[0].1, Rect { y: last_strip.bottom(), height: 1, ..last_strip });
    assert_eq!(plan.top.map(|r| r.height), Some(10));

    // narrow screen: one line per die and cluster grows the top box past 40%
    let plan = compute_layout(rect(0, 0, 30, 30), ALL, true, &content(&clusters));
    assert_eq!(plan.cores.len(), 4);
    let needed = 5 + 4 + 5 + 2; // strips, core lines, power rows, borders
    assert_eq!(plan.top.map(|r| r.height), Some(needed));
  }

  #[test]
  fn core_lines_wrap_per_die_then_per_cluster() {
    let line = |die, runs: &[(usize, Range<usize>, bool)]| {
      let runs = runs.iter().map(|(cluster, cores, label)| CoreRun {
        cluster: *cluster,
        cores: cores.clone(),
        label: *label,
      });
      CoreLine { die, runs: runs.collect() }
    };

    // M3 Ultra: "E " + 8 + "  " + "P " + 24 = 38 cells
    let ultra = vec![cluster("E", 4, 2), cluster("P", 12, 2)];
    assert_eq!(core_lines(38, &ultra), [line(None, &[(0, 0..8, true), (1, 0..24, true)])]);

    // per die: "D0 " + "E " + 4 + "  " + "P " + 12 = 25 cells
    let per_die = [
      line(Some(0), &[(0, 0..4, true), (1, 0..12, true)]),
      line(Some(1), &[(0, 4..8, true), (1, 12..24, true)]),
    ];
    assert_eq!(core_lines(37, &ultra), per_die);
    assert_eq!(core_lines(25, &ultra), per_die);

    // per die and cluster: "D0 P " + 12 = 17 cells
    let per_cluster = [
      line(Some(0), &[(0, 0..4, true)]),
      line(Some(0), &[(1, 0..12, true)]),
      line(Some(1), &[(0, 4..8, true)]),
      line(Some(1), &[(1, 12..24, true)]),
    ];
    assert_eq!(core_lines(24, &ultra), per_cluster);
    assert_eq!(core_lines(17, &ultra), per_cluster);

    // still too long: balanced chunks, the label only on the first one
    let lines = core_lines(13, &ultra);
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[1], line(Some(0), &[(1, 0..6, true)]));
    assert_eq!(lines[2], line(Some(0), &[(1, 6..12, false)]));
    assert!(lines.iter().all(|l| l.width(&ultra) <= 13));

    // single die skips the per-die step: M4 Max "E " + 4 + "  " + "P " + 12 = 22 cells
    let max = vec![cluster("E", 4, 1), cluster("P", 12, 1)];
    assert_eq!(core_lines(22, &max).len(), 1);
    assert_eq!(
      core_lines(21, &max),
      [line(None, &[(0, 0..4, true)]), line(None, &[(1, 0..12, true)])]
    );
  }

  #[test]
  fn core_lines_show_every_core_at_any_width() {
    let configs = [
      vec![cluster("E", 4, 1), cluster("P", 4, 1)],
      vec![cluster("E", 6, 1), cluster("P", 4, 1), cluster("S", 2, 1)],
      vec![cluster("E", 4, 2), cluster("P", 12, 2)],
      vec![cluster("P", 12, 2), cluster("S", 6, 2)],
    ];
    for clusters in &configs {
      for width in 0..60 {
        let lines = core_lines(width, clusters);
        for (i, c) in clusters.iter().enumerate() {
          // every core exactly once, in order
          let cores: Vec<usize> = lines
            .iter()
            .flat_map(|line| line.runs.iter().filter(|run| run.cluster == i))
            .flat_map(|run| run.cores.clone())
            .collect();
          assert_eq!(cores, (0..c.dies.len()).collect::<Vec<_>>(), "width {width}");
        }

        // lines fit unless a single bar doesn't
        for line in &lines {
          let bars: usize = line.runs.iter().map(|run| run.cores.len()).sum();
          assert!(line.width(clusters) <= usize::from(width) || bars == 1, "width {width}");
        }
      }
    }

    assert!(core_lines(80, &[]).is_empty());
    assert!(core_lines(80, &[ClusterCores { label: "E", dies: vec![] }]).is_empty());
  }

  #[test]
  fn rects_stay_inside_and_never_overlap() {
    let widths = [1, 2, 4, 5, 10, 40, 69, 70, 72, 80, 100, 160, 200, 400];
    let heights = [1, 2, 3, 5, 6, 8, 15, 19, 20, 24, 30, 40, 50, 120];
    let clusters = vec![cluster("E", 4, 2), cluster("P", 12, 2)];

    for (width, height) in widths.into_iter().flat_map(|w| heights.map(|h| (w, h))) {
      for offset in [(0, 0), (3, 2)] {
        let area = rect(offset.0, offset.1, width, height);
        for panels in all_panel_sets() {
          for per_core in [false, true] {
            let plan = compute_layout(area, panels, per_core, &content(&clusters));
            let ctx = format!("{area:?} {panels:?} per_core={per_core}");

            let boxes: Vec<Rect> = [plan.top, plan.proc].into_iter().flatten().collect();
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

            // parts of the metrics box sit inside its borders and never overlap
            let mut parts: Vec<Rect> = plan.strips.iter().map(|(_, r)| *r).collect();
            parts.extend(plan.cores.iter().map(|(_, r)| *r));
            parts.extend(plan.power);
            parts.extend(plan.separator);
            for (i, a) in parts.iter().enumerate() {
              let top = plan.top.expect("metrics parts without the metrics box");
              let inner = top.inner(Margin::new(1, 1));
              assert!(!a.is_empty() && inside(inner, *a), "{a:?} outside {inner:?} in {ctx}");
              for b in &parts[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?} in {ctx}");
              }
            }

            // hidden panels stay hidden
            let metrics = panels.cpu || panels.gpu || panels.mem || panels.power;
            assert_eq!(plan.top.is_some(), metrics, "{ctx}");
            assert!(panels.proc || plan.proc.is_none(), "{ctx}");
            assert!(panels.power || plan.power.is_none(), "{ctx}");
            assert!((panels.cpu && per_core) || plan.cores.is_empty(), "{ctx}");
            for (strip, _) in &plan.strips {
              let shown = match strip {
                Strip::Cluster(_) => panels.cpu,
                Strip::Gpu => panels.gpu,
                Strip::Ram | Strip::Swap => panels.mem,
              };
              assert!(shown, "{strip:?} in {ctx}");
            }
          }
        }
      }
    }
  }
}
