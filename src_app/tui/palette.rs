//! Terminal palette query. At startup macmon asks the terminal for its ANSI red, green and yellow
//! (OSC 4), so the load gradient can blend the terminal's own colors. A DA1 request follows the
//! color queries: terminals answer it, and answer in order, so its reply marks the end of the color
//! replies and a terminal that ignores OSC 4 doesn't cost the full timeout.
//!
//! The query needs raw mode (no echo, no line buffering), and nothing else may read the terminal
//! meanwhile: replies left unread would reach crossterm, which reads them as key presses.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub type Rgb = (u8, u8, u8);

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// ANSI indexes of the gradient colors.
const RED: u8 = 1;
const GREEN: u8 = 2;
const YELLOW: u8 = 3;

/// OSC 4 queries for red, green and yellow, then the DA1 request.
const QUERY: &[u8] = b"\x1b]4;1;?\x07\x1b]4;2;?\x07\x1b]4;3;?\x07\x1b[c";

/// How long the terminal may take to answer.
const QUERY_TIMEOUT: Duration = Duration::from_millis(150);
/// How long an unanswered query keeps reading (and dropping) late replies, until the DA1 reply.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Terminal colors of the load gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
  pub green: Rgb,
  pub yellow: Rgb,
  pub red: Rgb,
}

impl Palette {
  /// Palette from the color replies; `None` unless all three colors were answered.
  fn from_replies(replies: &Replies) -> Option<Self> {
    let color = |index| replies.colors.iter().rev().find(|(i, _)| *i == index).map(|&(_, c)| c);
    Some(Self { green: color(GREEN)?, yellow: color(YELLOW)?, red: color(RED)? })
  }
}

/// Terminal replies found in the input.
#[derive(Debug, Default, PartialEq, Eq)]
struct Replies {
  /// `(ANSI index, color)` of the OSC 4 color replies, in input order.
  colors: Vec<(u8, Rgb)>,
  /// The DA1 reply arrived, so no more replies follow.
  da1: bool,
}

/// Finds the OSC 4 color replies (`ESC ] 4 ; n ; rgb:R/G/B`, terminated by BEL or ST) and the DA1
/// reply (`ESC [ ? … c`) in `input`. Anything else (key presses, malformed or truncated replies) is
/// skipped.
fn parse_replies(input: &[u8]) -> Replies {
  let mut replies = Replies::default();
  let mut i = 0;
  while i < input.len() {
    if input[i] != ESC {
      i += 1;
      continue;
    }

    match input.get(i + 1) {
      Some(b']') => {
        let (body, next) = osc_string(input, i + 2);
        replies.colors.extend(body.and_then(parse_color));
        i = next;
      }
      Some(b'[') => match da1_len(&input[i..]) {
        Some(len) => {
          replies.da1 = true;
          i += len;
        }
        None => i += 1,
      },
      _ => i += 1,
    }
  }

  replies
}

/// Body of the OSC string starting at `start` (right after `ESC ]`) and where parsing resumes. A
/// string cut off by another escape sequence or by the end of the input has no body.
fn osc_string(input: &[u8], start: usize) -> (Option<&[u8]>, usize) {
  for pos in start..input.len() {
    match input[pos] {
      BEL => return (Some(&input[start..pos]), pos + 1),
      ESC if input.get(pos + 1) == Some(&b'\\') => return (Some(&input[start..pos]), pos + 2),
      ESC => return (None, pos),
      _ => {}
    }
  }

  (None, input.len())
}

/// Length of the DA1 reply (`ESC [ ? 6 2 ; 2 2 c`) at the start of `input`, if there is one.
fn da1_len(input: &[u8]) -> Option<usize> {
  let rest = input.strip_prefix(b"\x1b[?")?;
  let params = rest.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
  (rest.get(params) == Some(&b'c')).then_some(3 + params + 1)
}

/// Index and color of an OSC 4 reply body: `4;1;rgb:ffff/0000/0000`.
fn parse_color(body: &[u8]) -> Option<(u8, Rgb)> {
  let body = std::str::from_utf8(body).ok()?;
  let (index, spec) = body.strip_prefix("4;")?.split_once(';')?;
  if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }

  let mut channels = spec.strip_prefix("rgb:")?.split('/').map(channel);
  let rgb = (channels.next()??, channels.next()??, channels.next()??);
  channels.next().is_none().then_some((index.parse().ok()?, rgb))
}

/// 8-bit value of a color channel written with 1–4 hex digits (`f`, `ff`, `fff`, `ffff`).
fn channel(hex: &str) -> Option<u8> {
  if !(1..=4).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
    return None;
  }

  let value = u32::from_str_radix(hex, 16).ok()?;
  let max = (1u32 << (4 * hex.len())) - 1;
  Some(((value * 255 + max / 2) / max) as u8)
}

/// Input that can wait for data.
trait TimedRead {
  /// Reads the bytes available, waiting up to `timeout` for some to arrive; `Ok(0)` when none did.
  fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

/// Reads into `input` until it holds the DA1 reply or `timeout` passes. Returns whether the DA1
/// reply arrived.
fn read_until_da1(
  term: &mut impl TimedRead,
  input: &mut Vec<u8>,
  timeout: Duration,
) -> io::Result<bool> {
  let deadline = Instant::now() + timeout;
  let mut chunk = [0u8; 256];
  loop {
    if parse_replies(input).da1 {
      return Ok(true);
    }

    let left = deadline.saturating_duration_since(Instant::now());
    let read = if left.is_zero() { 0 } else { term.read_timeout(&mut chunk, left)? };
    if read == 0 {
      return Ok(false);
    }
    input.extend_from_slice(&chunk[..read]);
  }
}

/// Sends the palette query and reads the replies. Colors count only when they arrive within
/// `QUERY_TIMEOUT`; without the DA1 reply by then, late replies are read and dropped until it
/// arrives (at most `DRAIN_TIMEOUT`), so they don't turn into key presses later.
fn query(term: &mut (impl Write + TimedRead)) -> io::Result<Option<Palette>> {
  term.write_all(QUERY)?;
  term.flush()?;

  let mut input = vec![];
  let answered = read_until_da1(term, &mut input, QUERY_TIMEOUT)?;
  let palette = Palette::from_replies(&parse_replies(&input));
  if !answered {
    read_until_da1(term, &mut input, DRAIN_TIMEOUT)?;
  }

  Ok(palette)
}

/// Asks the terminal for its palette. Needs raw mode and must run before anything else reads the
/// terminal. `None` when the terminal doesn't answer in time or can't be opened.
pub fn query_terminal() -> Option<Palette> {
  let mut tty = Tty::open().ok()?;
  query(&mut tty).ok().flatten()
}

/// The controlling terminal.
struct Tty(File);

impl Tty {
  fn open() -> io::Result<Self> {
    OpenOptions::new().read(true).write(true).open("/dev/tty").map(Self)
  }
}

impl Write for Tty {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.write(buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.0.flush()
  }
}

impl TimedRead for Tty {
  fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
    let fd = self.0.as_raw_fd();
    if !(0..libc::FD_SETSIZE as i32).contains(&fd) {
      return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }

    let deadline = Instant::now() + timeout;
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      let mut wait = libc::timeval {
        tv_sec: left.as_secs() as libc::time_t,
        tv_usec: left.subsec_micros() as libc::suseconds_t,
      };

      // select(2), not poll(2): poll doesn't support devices on macOS
      let null = std::ptr::null_mut();
      let ready = unsafe {
        let mut fds: libc::fd_set = std::mem::zeroed();
        libc::FD_SET(fd, &mut fds);
        libc::select(fd + 1, &mut fds, null, null, &mut wait)
      };

      match ready {
        0 => return Ok(0),
        1.. => return self.0.read(buf),
        _ => {
          let err = io::Error::last_os_error();
          if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
          }
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::VecDeque;
  use std::fs::File;
  use std::io::{self, Read, Write};
  use std::os::fd::FromRawFd;
  use std::thread;
  use std::time::{Duration, Instant};

  use super::{
    DRAIN_TIMEOUT, Palette, QUERY, QUERY_TIMEOUT, Replies, TimedRead, Tty, channel, parse_replies,
    query,
  };

  const RED: &[u8] = b"\x1b]4;1;rgb:dcdc/3232/2f2f\x07";
  const GREEN: &[u8] = b"\x1b]4;2;rgb:8585/9999/0000\x07";
  const YELLOW: &[u8] = b"\x1b]4;3;rgb:b5b5/8989/0000\x07";
  const DA1: &[u8] = b"\x1b[?62;22c";

  const PALETTE: Palette =
    Palette { green: (0x85, 0x99, 0x00), yellow: (0xb5, 0x89, 0x00), red: (0xdc, 0x32, 0x2f) };

  fn answer() -> Vec<u8> {
    [RED, GREEN, YELLOW, DA1].concat()
  }

  fn colors(input: &[u8]) -> Vec<(u8, (u8, u8, u8))> {
    parse_replies(input).colors
  }

  #[test]
  fn parses_bel_and_st_terminators() {
    assert_eq!(colors(b"\x1b]4;1;rgb:ffff/0000/0000\x07"), [(1, (255, 0, 0))]);
    assert_eq!(colors(b"\x1b]4;2;rgb:0000/ffff/0000\x1b\\"), [(2, (0, 255, 0))]);
  }

  #[test]
  fn parses_1_to_4_hex_digits_per_channel() {
    assert_eq!(colors(b"\x1b]4;3;rgb:f/8/0\x07"), [(3, (255, 136, 0))]);
    assert_eq!(colors(b"\x1b]4;3;rgb:ff/80/00\x07"), [(3, (255, 128, 0))]);
    assert_eq!(colors(b"\x1b]4;3;rgb:fff/800/000\x07"), [(3, (255, 128, 0))]);
    assert_eq!(colors(b"\x1b]4;3;rgb:ffff/8080/0000\x07"), [(3, (255, 128, 0))]);
    // channels may differ in length, hex digits in either case
    assert_eq!(colors(b"\x1b]4;3;rgb:AB/c/00Ff\x07"), [(3, (0xab, 0xcc, 1))]);

    assert_eq!(channel("7f"), Some(0x7f));
    assert_eq!(channel("7fff"), Some(0x7f));
    assert_eq!(channel("8000"), Some(0x80));
    for bad in ["", "fffff", "g", "-1", "+f", " f"] {
      assert_eq!(channel(bad), None, "{bad:?}");
    }
  }

  #[test]
  fn parses_several_replies_in_one_buffer() {
    let replies = parse_replies(&answer());
    let expected = [(1, (0xdc, 0x32, 0x2f)), (2, (0x85, 0x99, 0x00)), (3, (0xb5, 0x89, 0x00))];
    assert_eq!(replies, Replies { colors: expected.to_vec(), da1: true });
    assert_eq!(Palette::from_replies(&replies), Some(PALETTE));

    // any order, other colors and repeated indexes (the last one wins)
    let input = [YELLOW, b"\x1b]4;12;rgb:00/00/ff\x07", GREEN, b"\x1b]4;1;rgb:00/00/00\x07", RED];
    let replies = parse_replies(&input.concat());
    assert_eq!(replies.colors.len(), 5);
    assert!(!replies.da1);
    assert_eq!(Palette::from_replies(&replies), Some(PALETTE));
  }

  #[test]
  fn skips_garbage() {
    let garbage: [&[u8]; 10] = [
      b"q1 4;1;rgb:ff/00/00",                           // key presses, no escape
      b"\x1b]4;1;rgb:ff/00\x07",                        // two channels
      b"\x1b]4;1;rgb:ff/00/00/00\x07",                  // four channels
      b"\x1b]4;1;rgb:ff/zz/00\x07",                     // not hex
      b"\x1b]4;1;rgb:12345/00/00\x07",                  // 5 digits
      b"\x1b]4;x;rgb:ff/00/00\x07",                     // bad index
      b"\x1b]4;256;rgb:ff/00/00\x07",                   // index out of range
      b"\x1b]4;1;#ff0000\x07",                          // other color format
      b"\x1b]10;rgb:ff/ff/ff\x07\x1b]11;rgb:0/0/0\x07", // fg / bg replies
      b"\x1b[?1;2R\x1b[c\x1b[?6$y\x1bO\xff\xfe",        // other sequences, no DA1
    ];
    for input in garbage {
      assert_eq!(parse_replies(input), Replies::default(), "{input:?}");
    }

    // valid replies around garbage still count
    let input = [b"\x00zz\x1b\x1b]".as_slice(), GREEN, b"\x1b]4;1;rgb:\xff\x07", RED, b"\x1b"];
    let replies = parse_replies(&input.concat());
    assert_eq!(replies.colors, [(2, (0x85, 0x99, 0x00)), (1, (0xdc, 0x32, 0x2f))]);
  }

  #[test]
  fn skips_truncated_replies() {
    let full = answer();
    // every cut of the full answer: only complete replies count
    for cut in 0..full.len() {
      let replies = parse_replies(&full[..cut]);
      let complete = [RED, GREEN, YELLOW].iter().scan(0, |end, r| {
        *end += r.len();
        Some(*end)
      });
      assert_eq!(replies.colors.len(), complete.filter(|end| *end <= cut).count(), "cut {cut}");
      assert!(!replies.da1, "cut {cut}");
    }

    // a reply cut off by the next one doesn't swallow it
    let input = [b"\x1b]4;1;rgb:ff/00".as_slice(), GREEN, b"\x1b]4;3;rgb:ff/ff/00\x1b", DA1];
    let replies = parse_replies(&input.concat());
    assert_eq!(replies, Replies { colors: vec![(2, (0x85, 0x99, 0x00))], da1: true });
  }

  #[test]
  fn detects_da1_sentinel() {
    for input in [b"\x1b[?62;22c".as_slice(), b"\x1b[?1;2c", b"\x1b[?6c", b"\x1b[?c"] {
      assert!(parse_replies(input).da1, "{input:?}");
    }
    for input in [b"\x1b[?62;22".as_slice(), b"\x1b[62c", b"\x1b?62c", b"\x1b[?62;x22c", b"[?6c"] {
      assert!(!parse_replies(input).da1, "{input:?}");
    }
    // DA1 alone: no palette
    assert_eq!(Palette::from_replies(&parse_replies(DA1)), None);
  }

  #[test]
  fn palette_needs_all_three_colors() {
    for missing in 0..3 {
      let mut replies = vec![RED, GREEN, YELLOW];
      replies.remove(missing);
      replies.push(DA1);
      assert_eq!(Palette::from_replies(&parse_replies(&replies.concat())), None, "{missing}");
    }
  }

  enum Step {
    Data(Vec<u8>),
    Timeout,
    Fail,
  }

  /// Scripted terminal: each read returns the next step.
  #[derive(Default)]
  struct FakeTerm {
    written: Vec<u8>,
    steps: VecDeque<Step>,
    waits: Vec<Duration>,
  }

  impl FakeTerm {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
      Self { steps: steps.into_iter().collect(), ..Default::default() }
    }
  }

  impl Write for FakeTerm {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      self.written.extend_from_slice(buf);
      Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl TimedRead for FakeTerm {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
      self.waits.push(timeout);
      match self.steps.pop_front() {
        Some(Step::Data(data)) => {
          buf[..data.len()].copy_from_slice(&data);
          Ok(data.len())
        }
        Some(Step::Fail) => Err(io::Error::other("read failed")),
        Some(Step::Timeout) | None => Ok(0),
      }
    }
  }

  fn data(bytes: &[u8]) -> Step {
    Step::Data(bytes.to_vec())
  }

  #[test]
  fn query_sends_osc4_then_da1() {
    let mut term = FakeTerm::new([data(&answer())]);
    assert_eq!(query(&mut term).unwrap(), Some(PALETTE));
    assert_eq!(term.written, b"\x1b]4;1;?\x07\x1b]4;2;?\x07\x1b]4;3;?\x07\x1b[c");
    assert_eq!(term.written, QUERY);
    // one read, nothing more after the DA1 reply
    assert_eq!(term.waits.len(), 1);
    assert!(term.waits[0] <= QUERY_TIMEOUT);
  }

  #[test]
  fn query_reads_replies_in_pieces() {
    let mut term = FakeTerm::new(answer().into_iter().map(|b| data(&[b])));
    assert_eq!(query(&mut term).unwrap(), Some(PALETTE));
    assert_eq!(term.waits.len(), answer().len());
    assert!(term.waits.windows(2).all(|w| w[1] <= w[0]), "one deadline for all reads");
  }

  #[test]
  fn query_without_osc4_support_stops_at_da1() {
    let mut term = FakeTerm::new([data(DA1), data(b"q")]);
    assert_eq!(query(&mut term).unwrap(), None);
    assert_eq!(term.waits.len(), 1, "no wait for the timeout");
    assert_eq!(term.steps.len(), 1, "input after the DA1 reply is left alone");
  }

  #[test]
  fn query_drains_late_replies() {
    // nothing within the timeout: the replies that come later are read up to the DA1 reply
    let mut term = FakeTerm::new([Step::Timeout, data(&answer()[..20]), data(&answer()[20..])]);
    term.steps.push_back(data(b"q"));
    assert_eq!(query(&mut term).unwrap(), None, "late colors don't count");
    assert_eq!(term.steps.len(), 1, "input after the DA1 reply is left alone");
    assert_eq!(term.waits.len(), 3);
    assert!(term.waits[0] <= QUERY_TIMEOUT);
    assert!(term.waits[1] > QUERY_TIMEOUT && term.waits[1] <= DRAIN_TIMEOUT);

    // a reply cut by the timeout: drained, ignored
    let full = answer();
    let (head, tail) = full.split_at(10);
    let mut term = FakeTerm::new([data(head), Step::Timeout, data(tail)]);
    assert_eq!(query(&mut term).unwrap(), None);
    assert!(term.steps.is_empty());
  }

  #[test]
  fn query_unanswered_gives_up() {
    let mut term = FakeTerm::new([]);
    assert_eq!(query(&mut term).unwrap(), None);
    assert_eq!(term.waits.len(), 2, "query and drain");

    // colors without the DA1 reply still count when they arrive in time
    let replies = [RED, GREEN, YELLOW].concat();
    let mut term = FakeTerm::new([data(&replies)]);
    assert_eq!(query(&mut term).unwrap(), Some(PALETTE));
  }

  #[test]
  fn query_read_error() {
    let mut term = FakeTerm::new([data(RED), Step::Fail]);
    assert!(query(&mut term).is_err());
  }

  /// Pseudo terminal in raw mode: macmon's side and the terminal (master) side.
  fn pty() -> (Tty, File) {
    use std::ptr::null_mut;
    let (mut master, mut slave) = (0, 0);
    unsafe {
      assert_eq!(libc::openpty(&mut master, &mut slave, null_mut(), null_mut(), null_mut()), 0);
      let mut attrs: libc::termios = std::mem::zeroed();
      assert_eq!(libc::tcgetattr(slave, &mut attrs), 0);
      libc::cfmakeraw(&mut attrs);
      assert_eq!(libc::tcsetattr(slave, libc::TCSANOW, &attrs), 0);
      (Tty(File::from_raw_fd(slave)), File::from_raw_fd(master))
    }
  }

  /// Plays a terminal on the master side: reads the query, waits `delay`, then writes `answer`.
  fn answer_query(mut master: File, delay: Duration, answer: Vec<u8>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
      let mut query = vec![0u8; QUERY.len()];
      master.read_exact(&mut query).unwrap();
      assert_eq!(query, QUERY);
      thread::sleep(delay);
      master.write_all(&answer).unwrap();
      // keep the master open until the test is done reading
      thread::sleep(Duration::from_millis(300));
    })
  }

  #[test]
  fn tty_read_waits_for_data() {
    let (mut tty, mut master) = pty();
    let started = Instant::now();
    let mut buf = [0u8; 16];
    assert_eq!(tty.read_timeout(&mut buf, Duration::from_millis(50)).unwrap(), 0);
    assert!(started.elapsed() >= Duration::from_millis(40));

    master.write_all(b"abc").unwrap();
    let read = tty.read_timeout(&mut buf, Duration::from_secs(2)).unwrap();
    assert_eq!(&buf[..read], b"abc");
  }

  #[test]
  fn query_over_pty() {
    let (mut tty, master) = pty();
    let terminal = answer_query(master, Duration::ZERO, answer());
    let started = Instant::now();
    assert_eq!(query(&mut tty).unwrap(), Some(PALETTE));
    assert!(started.elapsed() < QUERY_TIMEOUT, "the DA1 reply ends the wait");

    // nothing left for crossterm to read
    let mut buf = [0u8; 64];
    assert_eq!(tty.read_timeout(&mut buf, Duration::from_millis(50)).unwrap(), 0);
    terminal.join().unwrap();
  }

  #[test]
  fn late_replies_over_pty_are_drained() {
    let (mut tty, master) = pty();
    let terminal = answer_query(master, QUERY_TIMEOUT + Duration::from_millis(100), answer());
    assert_eq!(query(&mut tty).unwrap(), None);

    let mut buf = [0u8; 64];
    assert_eq!(tty.read_timeout(&mut buf, Duration::from_millis(50)).unwrap(), 0);
    terminal.join().unwrap();
  }
}
