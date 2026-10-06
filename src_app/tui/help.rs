//! Help overlay (`?`): every key and mouse action, and what the less obvious values mean.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Clear;

use super::boxes::{Titles, draw_box};
use super::theme::{heading, text};

/// One line of the help.
enum Row {
  Section(&'static str),
  /// Keys (or what to do with the mouse) and what they do.
  Key(&'static str, &'static str),
  /// More of the text of the row above.
  More(&'static str),
}

use Row::{Key, More, Section};

const HELP: &[Row] = &[
  Section("Keys"),
  Key("q, Ctrl-C", "quit (q closes this help first)"),
  Key("?", "show / hide this help"),
  Key("p", "show / hide the process list"),
  Key("v", "chart view: graph / gauge"),
  Key("r", "CPU and GPU usage: scaled / active"),
  Key("- / +", "update interval"),
  Section("Process list"),
  Key("↑ ↓", "select a process"),
  Key("PgUp PgDn", "move the selection a page; scroll without one"),
  Key("Home End", "first / last process; scroll without a selection"),
  Key("← →", "sort by the column on the left / right"),
  Key("s / S", "sort by the next column / reverse the order"),
  Key("/", "filter by name or PID"),
  Key("Esc", "clear the selection and the filter"),
  Section("Typing a filter"),
  Key("Enter / Esc", "keep / clear the filter; ↑ ↓ select, other keys type"),
  Section("Mouse"),
  Key("click", "a header sorts by it (again: reverses), a process"),
  More("selects it (again: clears), a hint presses its key"),
  Key("wheel", "scroll the process list"),
  Section("Values"),
  Key("CPU%", "100% is one fully busy core, as in Activity Monitor"),
  Key("scaled", "usage weighted by frequency: share of the maximum"),
  Key("active", "share of the time the cores were busy"),
  Key("POWER -", "another user's process: macOS gives the energy of"),
  More("your own processes only (run with sudo for all)"),
  Key("MEM", "footprint of your processes, resident size of others"),
  Key("select text", "hold Option (iTerm2) or Shift (Ghostty, most other"),
  More("terminals) while dragging"),
];

/// Cells of the key column, the gap after it included.
const KEYS_WIDTH: usize = 14;

/// The help text: section names bold, keys bold, the rest plain.
fn lines() -> Vec<Line<'static>> {
  let row = |keys: &'static str, what: &'static str| {
    Line::from(vec![heading(format!("  {keys:KEYS_WIDTH$}")), text(what)])
  };
  HELP
    .iter()
    .map(|line| match *line {
      Section(name) => Line::from(heading(name)),
      Key(keys, what) => row(keys, what),
      More(what) => row("", what),
    })
    .collect()
}

/// Draws the help in a box centered over `area`, from line `scroll` on, clamped so the box is
/// never left half empty. Returns the clamped scroll.
pub(super) fn render(f: &mut Frame, area: Rect, scroll: usize) -> usize {
  let lines = lines();
  // the text with a blank column on both sides, and the borders
  let width = lines.iter().map(Line::width).max().unwrap_or(0) + 4;
  let height = lines.len() + 2;
  let width = width.min(usize::from(area.width)) as u16;
  let height = height.min(usize::from(area.height)) as u16;
  let x = area.x + (area.width - width) / 2;
  let y = area.y + (area.height - height) / 2;
  let rect = Rect::new(x, y, width, height);

  let shown = usize::from(height.saturating_sub(2));
  let scroll = scroll.min(lines.len().saturating_sub(shown));
  let close = vec![heading("Esc"), text(" close")];
  let mut titles = Titles::new(heading("help"));
  if shown < lines.len() {
    titles = titles.left(vec![heading("↑↓"), text(" scroll")]);
  }

  f.render_widget(Clear, rect);
  let (inner, _) = draw_box(f, rect, titles.right(Line::from(close)));
  let buf = f.buffer_mut();
  for (y, line) in (inner.y..).zip(lines.iter().skip(scroll).take(shown)) {
    buf.set_line(inner.x + 1, y, line, inner.width.saturating_sub(2));
  }
  scroll
}

#[cfg(test)]
mod tests {
  use super::{HELP, KEYS_WIDTH, Row, lines};

  #[test]
  fn keys_fit_their_column() {
    for row in HELP {
      if let Row::Key(keys, _) = row {
        assert!(keys.chars().count() < KEYS_WIDTH, "{keys}");
      }
    }
    // the box stays under 80 columns
    assert!(lines().iter().all(|line| line.width() + 4 <= 80));
  }
}
