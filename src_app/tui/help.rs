//! Help overlay (`?`): the keys, the mouse, and what the less obvious values mean.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Clear;

use super::boxes::{Hint, Titles, cells, draw_box};
use super::theme::{heading, text};
use Row::{Key, Section};

/// One line of the help.
enum Row {
  Section(&'static str),
  /// Keys (or what to do with the mouse) and what they do.
  Key(&'static str, &'static str),
}

const HELP: &[Row] = &[
  Section("Keys"),
  Key("q", "quit"),
  Key("p", "show / hide the processes"),
  Key("v", "graph / gauge"),
  Key("r", "CPU and GPU usage: scaled / active"),
  Key("- / +", "update interval"),
  Key("/", "filter the processes"),
  Key("s / S", "sort by the next column / reverse"),
  Key("↑ ↓", "select a process"),
  Key("Esc", "clear the selection and the filter"),
  Key("?", "show / hide this help"),
  Section("Mouse"),
  Key("click", "a column header to sort, a row to select, a hint to press it"),
  Section("Notes"),
  Key("CPU%", "100% is one fully busy core"),
  Key("scaled", "usage weighted by frequency; active: share of time busy"),
  Key("POWER -", "not available for other users' processes"),
  Key("select text", "hold Option (iTerm2) or Shift (most terminals)"),
];

/// Cells of the key column, the gap after it included.
const KEYS_WIDTH: usize = 14;

/// The help text: section names bold, keys bold, the rest plain.
fn lines() -> Vec<Line<'static>> {
  HELP
    .iter()
    .map(|line| match *line {
      Section(name) => Line::from(heading(name)),
      Key(keys, what) => Line::from(vec![heading(format!("  {keys:KEYS_WIDTH$}")), text(what)]),
    })
    .collect()
}

/// Draws the help in a box centered over `area`, from line `scroll` on (in a window too short for
/// all of it, ↑ / ↓ and the wheel scroll it), clamped so the box is never left half empty.
/// Returns the clamped scroll.
pub(super) fn render(f: &mut Frame, area: Rect, scroll: usize) -> usize {
  let lines = lines();
  // the text with a blank column on both sides, and the borders
  let width = lines.iter().map(Line::width).max().unwrap_or(0) + 4;
  let height = lines.len() + 2;
  let width = cells(width).min(area.width);
  let height = cells(height).min(area.height);
  let x = area.x + (area.width - width) / 2;
  let y = area.y + (area.height - height) / 2;
  let rect = Rect::new(x, y, width, height);

  let shown = usize::from(height.saturating_sub(2));
  let scroll = scroll.min(lines.len().saturating_sub(shown));
  // any click closes the help, as Esc does
  let close = Hint::new(("Esc", KeyCode::Esc), "close");
  let titles = Titles::new(heading("help")).right(Line::from(close.spans()));

  f.render_widget(Clear, rect);
  let (inner, _) = draw_box(f, rect, titles);
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
    // the box stays under 80 columns, and fits an 80x24 window
    assert!(lines().iter().all(|line| line.width() + 4 <= 80));
    assert!(lines().len() + 2 <= 24);
  }
}
