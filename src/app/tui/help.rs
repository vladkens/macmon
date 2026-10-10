//! Help overlay (`?`): the keys, the mouse, and what the less obvious values mean.

use std::num::NonZeroU16;

use Row::{Key, Section};
use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Clear;

use super::boxes::{Titles, cells, draw_box, hint};
use super::theme::{heading, link, text};

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
  Key("k", "kill the selected process"),
  Key("Esc", "clear the selection and the filter"),
  Key("?", "show / hide this help"),
  Section("Mouse"),
  Key("click", "a column header to sort, a row to select"),
  Section("Notes"),
  Key("CPU%", "100% is one fully busy core"),
  Key("scaled", "usage weighted by frequency; active: share of time busy"),
  Key("POWER -", "not available for other users' processes"),
];

/// Cells of the key column, the gap after it included.
const KEYS_WIDTH: usize = 14;

/// Last line of the help, centered after a blank one: `SUPPORT_TEXT` links to `SUPPORT_URL`.
const SUPPORT_PREFIX: &str = "Like macmon? Support it: ";
const SUPPORT_TEXT: &str = "buymeacoffee.com/vladkens";
const SUPPORT_URL: &str = "https://buymeacoffee.com/vladkens";

/// The help text: section names bold, keys bold, the rest plain, then the support line.
fn lines() -> Vec<Line<'static>> {
  let mut lines: Vec<Line<'static>> = HELP
    .iter()
    .map(|line| match *line {
      Section(name) => Line::from(heading(name)),
      Key(keys, what) => Line::from(vec![heading(format!("  {keys:KEYS_WIDTH$}")), text(what)]),
    })
    .collect();
  lines.push(Line::default());
  lines.push(Line::from(vec![text(SUPPORT_PREFIX), link(SUPPORT_TEXT)]));
  lines
}

/// Turns the `width` cells from `(x, y)` into one OSC 8 hyperlink to `url`, which terminals that
/// support it open on Cmd-click (others show the plain text). The first cell writes the whole text
/// inside the link and takes all `width` cells; the others keep their text, so the buffer still
/// matches the screen.
fn hyperlink(buf: &mut Buffer, x: u16, y: u16, width: u16, url: &str) {
  let Some(forced) = NonZeroU16::new(width) else {
    return;
  };
  let text: String = (x..x + width).map(|x| buf[(x, y)].symbol()).collect();
  buf[(x, y)]
    .set_symbol(&format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\"))
    .set_diff_option(CellDiffOption::ForcedWidth(forced));
}

/// Draws the help in a box centered over `area`, from line `scroll` on (in a window too short for
/// all of it, ↑ / ↓ scroll it), clamped so the box is never left half empty. Returns the clamped
/// scroll.
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
  if rect.is_empty() {
    return scroll; // nothing to draw on (a window resized to zero rows or columns)
  }

  let shown = usize::from(height.saturating_sub(2));
  let scroll = scroll.min(lines.len().saturating_sub(shown));
  let titles = Titles::new(heading("help")).right(hint("Esc", "close"));

  f.render_widget(Clear, rect);
  let inner = draw_box(f, rect, titles);
  let buf = f.buffer_mut();
  let text_width = inner.width.saturating_sub(2);
  // the support line is the last one, centered; the rest start at the left
  let support = lines.len() - 1;
  let support_x = inner.x + 1 + text_width.saturating_sub(cells(lines[support].width())) / 2;
  for (i, y) in (scroll..lines.len()).take(shown).zip(inner.y..) {
    let x = if i == support { support_x } else { inner.x + 1 };
    buf.set_line(x, y, &lines[i], text_width);
  }

  // linked only when on screen and not cut
  let (prefix, link_width) = (cells(SUPPORT_PREFIX.len()), cells(SUPPORT_TEXT.len()));
  if (scroll..scroll + shown).contains(&support) && prefix + link_width <= text_width {
    let y = inner.y + cells(support - scroll);
    hyperlink(buf, support_x + prefix, y, link_width, SUPPORT_URL);
  }
  scroll
}
