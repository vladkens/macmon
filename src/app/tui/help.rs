//! Help overlay (`?`): the keys, the mouse, and what the less obvious values mean.

use std::num::NonZeroU16;

use Row::{Key, Section, Term};
use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;

use super::boxes::{Titles, cells, draw_box, draw_scrollbar, hint};
use super::theme::{self, faint, heading, link, text};

/// One line of the help.
enum Row {
  Section(&'static str),
  /// Keys and what they do; several keys are separated by ` / `.
  Key(&'static str, &'static str),
  /// What to do with the mouse, or a value, and what it means.
  Term(&'static str, &'static str),
}

const HELP: &[Row] = &[
  Section("Keys"),
  Key("q", "quit"),
  Key("p", "show / hide the processes"),
  Key("v", "graph / gauge"),
  Key("r", "CPU and GPU usage: freq / raw"),
  Key("- / +", "update interval"),
  Key("/", "filter the processes"),
  Key("Space", "pause row ordering (values stay live)"),
  Key("f", "turn the saved process filter off / on"),
  Key("s / S", "sort by the next column / reverse"),
  Key("↑ ↓", "select a process"),
  Key("i", "open files and sockets of the selected process"),
  Key("e", "show the selected process in Finder"),
  Key("c", "copy the path of the selected process"),
  Key("k", "kill the selected process"),
  Key("Esc", "clear the selection and the filter"),
  Key("?", "show / hide this help"),
  Section("Info overlay"),
  Key("Space", "pause / resume ordering and file updates"),
  Key("/", "filter the files"),
  Key("f", "turn the saved file filter off / on"),
  Key("e", "show the selected file in Finder"),
  Key("c", "copy the selected path"),
  Key("q / Esc", "back to the processes"),
  Section("Mouse"),
  Term("click", "a column header to sort, a row to select"),
  Section("Notes"),
  Term("CPU%", "100% is one fully busy core"),
  Term("TIME", "CPU time used since the process started"),
  Term("freq", "usage weighted by frequency; raw: share of time busy"),
  Term("POWER -", "not available for other users' processes"),
];

/// Cells of the key column, the gap after it included.
const KEYS_WIDTH: usize = 14;

/// Last line of the help, centered after a blank one: `SUPPORT_TEXT` links to `SUPPORT_URL`.
const SUPPORT_PREFIX: &str = "Like macmon? Support it: ";
const SUPPORT_TEXT: &str = "buymeacoffee.com/vladkens";
const SUPPORT_URL: &str = "https://buymeacoffee.com/vladkens";

/// `keys` as in hints (`theme::key`), the ` / ` between them faint, indented and padded to
/// `KEYS_WIDTH`.
fn key_column(keys: &'static str) -> Vec<Span<'static>> {
  let mut spans = vec![text("  ")];
  for (i, key) in keys.split(" / ").enumerate() {
    if i > 0 {
      spans.push(faint(" / "));
    }
    spans.push(theme::key(key));
  }
  let width = spans.iter().map(Span::width).sum::<usize>() - 2;
  spans.push(text(" ".repeat(KEYS_WIDTH.saturating_sub(width))));
  spans
}

/// The help text: section names bold, keys as in hints, terms bold, the rest plain, then the
/// support line.
fn lines() -> Vec<Line<'static>> {
  let mut lines: Vec<Line<'static>> = HELP
    .iter()
    .map(|line| match *line {
      Section(name) => Line::from(heading(name)),
      Key(keys, what) => Line::from([key_column(keys), vec![text(what)]].concat()),
      Term(term, what) => Line::from(vec![heading(format!("  {term:KEYS_WIDTH$}")), text(what)]),
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
/// all of it, ↑ / ↓ scroll it, with a scrollbar on the right border), clamped so the box is never
/// left half empty. Returns the clamped scroll.
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
  draw_scrollbar(f, rect, inner, lines.len(), scroll);
  scroll
}

#[cfg(test)]
mod tests {
  use ratatui::Terminal;
  use ratatui::backend::TestBackend;
  use ratatui::style::Modifier;

  use super::render;
  use crate::tui::theme;

  #[test]
  fn keys_are_cyan_and_terms_bold() {
    let mut term = Terminal::new(TestBackend::new(80, 40)).unwrap();
    term.draw(|f| _ = render(f, f.area(), 0)).unwrap();
    let buf = term.backend().buffer();
    let rows: Vec<String> =
      (0..40).map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect()).collect();
    // `(fg, modifier)` of each cell of `text` on the row that says `what`
    let style = |what: &str, text: &str| {
      let y = rows.iter().position(|row| row.contains(what)).unwrap();
      let x = rows[y][..rows[y].find(text).unwrap()].chars().count() as u16;
      let cells = x..x + text.chars().count() as u16;
      cells.map(|x| (buf[(x, y as u16)].fg, buf[(x, y as u16)].modifier)).collect::<Vec<_>>()
    };
    let (key, faint) = ((theme::KEY, Modifier::empty()), (theme::TEXT, Modifier::DIM));
    let (bold, plain) = ((theme::TEXT, Modifier::BOLD), (theme::TEXT, Modifier::empty()));

    // keys cyan, the ` / ` between them faint, what they do plain
    assert_eq!(style("quit", "q"), [key]);
    assert_eq!(style("update interval", "- / +"), [key, faint, faint, faint, key]);
    assert_eq!(style("select a process", "↑ ↓"), [key; 3]);
    assert_eq!(style("clear the selection", "Esc"), [key; 3]);
    assert_eq!(style("update interval", "update"), [plain; 6]);
    // the mouse and the notes bold, not keys
    assert_eq!(style("a row to select", "click"), [bold; 5]);
    assert_eq!(style("fully busy core", "CPU%"), [bold; 4]);
    assert_eq!(style("other users'", "POWER -"), [bold; 7]);
  }

  #[test]
  fn a_scrollbar_shows_when_the_lines_dont_fit() {
    // the right border of the box, corner to corner, in an 80 cells window `height` rows high
    let border = |height: u16, scroll: usize| {
      let mut term = Terminal::new(TestBackend::new(80, height)).unwrap();
      term.draw(|f| _ = render(f, f.area(), scroll)).unwrap();
      let buf = term.backend().buffer();
      let at = |corner| buf.content.iter().position(|c| c.symbol() == corner).unwrap() as u16;
      let (top, bottom) = (at("╮"), at("╯"));
      assert_eq!(top % 80, bottom % 80);
      (top / 80..=bottom / 80).map(|y| buf[(top % 80, y)].symbol()).collect::<String>()
    };

    // the thumb at the top, then (scrolled to the end) at the bottom; the corners stay
    let (top, end) = (border(12, 0), border(12, usize::MAX));
    assert!(top.starts_with("╮↑┃") && top.ends_with("│↓╯"), "{top}");
    assert!(end.starts_with("╮↑│") && end.ends_with("┃↓╯"), "{end}");
    // all of it on screen: the plain border
    let tall = border(40, 0);
    assert_eq!(tall, format!("╮{}╯", "│".repeat(tall.chars().count() - 2)));
  }
}
