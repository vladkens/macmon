//! The process filter: words and `key:value` conditions matched against `ProcInfo`.

use std::cmp::Ordering;

use crate::fds::cwd;
use crate::procs::ProcInfo;

/// Units of a number in a filter word and their worth: no unit, bytes (binary, as the MEM column
/// shows them) and seconds.
const NUM: &[(&str, f64)] = &[("", 1.0)];
const MEM: &[(&str, f64)] = &[("", 1.0), ("k", 1024.0), ("m", 1048576.0), ("g", 1073741824.0)];
const TIME: &[(&str, f64)] = &[("", 1.0), ("s", 1.0), ("m", 60.0), ("h", 3600.0), ("d", 86400.0)];

/// What one word of a process filter looks for.
enum Cond {
  /// Lowercase text in the name or pid.
  Text(String),
  /// Lowercase text in the value of a key.
  Has(fn(&ProcInfo) -> &str, String),
  /// Lowercase text in the working directory, read only for this word.
  Dir(String),
  /// The value of a key, if any, above (`Greater`) or below (`Less`) a limit.
  Num(fn(&ProcInfo) -> Option<f64>, Ordering, f64),
}

impl Cond {
  /// `None` for a known key with a value that isn't complete yet, and for an empty word; a word
  /// without a known key is text.
  fn parse(word: &str) -> Option<Self> {
    let (key, value) = word.split_once(':').unwrap_or(("", word));
    match key {
      _ if word.is_empty() => None,
      "user" | "path" | "dir" if value.is_empty() => None,
      "user" => Some(Self::Has(|p| &p.user, value.to_string())),
      "path" => Some(Self::Has(|p| &p.path, value.to_string())),
      "dir" => Some(Self::Dir(value.to_string())),
      "cpu" => Self::num(|p| Some(f64::from(p.cpu_pct)), NUM, value),
      "gpu" => Self::num(|p| Some(f64::from(p.gpu_pct)), NUM, value),
      "power" => Self::num(|p| p.power_w.map(f64::from), NUM, value),
      "mem" => Self::num(|p| Some(p.mem_bytes as f64), MEM, value.trim_end_matches('b')),
      "time" => Self::num(|p| Some(p.cpu_time_ns as f64 / 1e9), TIME, value),
      _ => Some(Self::Text(word.to_string())),
    }
  }

  /// A limit such as `>1g`, `<1g` or `1g` (the same as `>1g`), in the units of `read`.
  fn num(read: fn(&ProcInfo) -> Option<f64>, units: &[(&str, f64)], value: &str) -> Option<Self> {
    let side = if value.starts_with('<') { Ordering::Less } else { Ordering::Greater };
    let value = value.strip_prefix(['>', '<']).unwrap_or(value);
    let number = value.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let (_, worth) = units.iter().find(|(unit, _)| *unit == &value[number.len()..])?;
    Some(Self::Num(read, side, number.parse::<f64>().ok()? * worth))
  }

  fn hit(&self, proc: &ProcInfo) -> bool {
    match self {
      Self::Text(text) => {
        proc.name.to_lowercase().contains(text) || proc.pid.to_string().contains(text)
      }
      Self::Has(read, text) => read(proc).to_lowercase().contains(text),
      Self::Dir(text) => {
        cwd(proc.pid).ok().flatten().is_some_and(|dir| dir.to_lowercase().contains(text))
      }
      Self::Num(read, side, limit) => read(proc).and_then(|x| x.partial_cmp(limit)) == Some(*side),
    }
  }
}

/// Whether a word of the filter text has a known key and a value that isn't valid.
pub fn is_bad(word: &str) -> bool {
  let word = word.to_lowercase();
  let word = word.strip_prefix('!').unwrap_or(&word);
  !word.is_empty() && Cond::parse(word).is_none()
}

/// One comma-separated term of a process filter: its text and its words, `true` for a negated one.
pub struct Term {
  pub text: String,
  words: Vec<(bool, Cond)>,
}

impl Term {
  /// `None` without a complete word: such a term is ignored, as an empty one is.
  fn parse(text: &str) -> Option<Self> {
    let text = text.trim().to_lowercase();
    let mut words = vec![];
    let mut bang = false;
    for word in text.split_whitespace() {
      let bare = word.strip_prefix('!');
      let (not, word) = (bang || bare.is_some(), bare.unwrap_or(word));
      // `!` alone goes with the next word: `! helper`
      bang = word.is_empty();
      words.extend(Cond::parse(word).map(|cond| (not, cond)));
    }

    (!words.is_empty()).then_some(Self { text, words })
  }

  pub fn hit(&self, proc: &ProcInfo) -> bool {
    self.words.iter().all(|(not, cond)| cond.hit(proc) != *not)
  }
}

/// `Filter` of the process list, with words in its terms: in `node !user:root, cargo mem:>1g,
/// !helper` a row passes if it matches any term and every term of only negated words, which has no
/// total row.
pub struct Query {
  pub any: Vec<Term>,
  none: Vec<Term>,
}

impl Query {
  pub fn parse(text: &str) -> Self {
    let terms = text.split(',').filter_map(Term::parse);
    let (none, any) = terms.partition(|t| t.words.iter().all(|(not, _)| *not));
    Self { any, none }
  }

  pub fn matches(&self, proc: &ProcInfo) -> bool {
    let any = self.any.is_empty() || self.any.iter().any(|t| t.hit(proc));
    any && self.none.iter().all(|t| t.hit(proc))
  }
}

#[cfg(test)]
mod tests {
  use super::{Query, is_bad};
  use crate::procs::ProcInfo;

  fn proc(pid: i32, name: &str, cpu: f32, mem_mb: u64, power: Option<f32>, gpu: f32) -> ProcInfo {
    ProcInfo {
      pid,
      name: name.to_string(),
      path: format!("/usr/bin/{name}"),
      user: "user".to_string(),
      cpu_pct: cpu,
      // an order of its own: the last two digits of the pid in seconds
      cpu_time_ns: (pid % 100) as u64 * 1_000_000_000,
      mem_bytes: mem_mb << 20,
      power_w: power,
      gpu_pct: gpu,
      started: None,
    }
  }

  fn sample() -> Vec<ProcInfo> {
    vec![
      proc(1, "launchd", 0.5, 20, None, 0.0),
      proc(631, "WindowServer", 25.0, 300, Some(1.5), 40.0),
      proc(2301, "Safari", 12.0, 900, Some(0.8), 5.0),
      proc(4410, "cargo", 80.0, 150, Some(3.2), 0.0),
      proc(77, "safaribookmarksyncagent", 0.0, 10, Some(0.0), 0.0),
    ]
  }

  #[test]
  fn queries_match_words_and_keys() {
    let mut procs = sample();
    procs[0].user = "root".to_string();
    procs[1].path = String::new();
    procs[2].path = "/Applications/Safari.app/Contents/MacOS/Safari".to_string();
    procs.push(proc(500, "Google Chrome", 1.0, 5, Some(0.1), 0.0));
    procs.push(proc(501, "Google Chrome Helper", 1.0, 5, Some(0.1), 0.0));
    procs.push(proc(502, "node:worker", 1.0, 5, Some(0.1), 0.0));
    let all = [1, 631, 2301, 4410, 77, 500, 501, 502];

    let cases: &[(&str, &[i32])] = &[
      // old filters
      ("safari, cargo", &[2301, 4410, 77]),
      ("chrome, !helper", &[500]),
      ("chrome, ! helper", &[500]),
      ("safari,", &[2301, 77]),
      ("safari, !", &[2301, 77]),
      // all words of a term
      ("google helper", &[501]),
      ("saf mem:>500m", &[2301]),
      ("saf mem:>500m, cargo", &[2301, 4410]),
      // `!` words
      ("saf !bookmark", &[2301]),
      ("!bookmark saf", &[2301]),
      ("!google !saf !node", &[1, 631, 4410]),
      ("d !user:root", &[631, 502]),
      // keys, with `>` and `<`
      ("cpu:>50", &[4410]),
      ("cpu:<0.5", &[77]),
      ("gpu:>5", &[631]),
      ("gpu:<5", &[1, 4410, 77, 500, 501, 502]),
      ("power:>1", &[631, 4410]),
      ("power:<0.5", &[77, 500, 501, 502]),
      ("mem:>500m", &[2301]),
      ("mem:<6", &[]),
      ("time:>1m", &[77]),
      ("time:<2", &[1, 2301, 500, 501]),
      ("user:ROOT", &[1]),
      ("path:/applications", &[2301]),
      // units
      ("mem:>899mb", &[2301]),
      ("mem:>900m", &[]),
      ("mem:<6144k", &[500, 501, 502]),
      ("mem:<6144kb", &[500, 501, 502]),
      ("mem:>0.8G", &[2301]),
      ("mem:>0.8gb", &[2301]),
      ("mem:<1b", &[]),
      ("time:>76s", &[77]),
      ("time:>1h", &[]),
      ("time:<1d", &all),
      // no sign: `>`
      ("cpu:50", &[4410]),
      ("mem:500m", &[2301]),
      ("time:1m", &[77]),
      // values not complete yet
      ("mem:", &all),
      ("mem:>", &all),
      ("mem:>1x", &all),
      ("power:>1g", &all),
      ("user:", &all),
      ("dir:", &all),
      ("!mem:>", &all),
      ("saf mem:>", &[2301, 77]),
      ("saf !mem:>1x", &[2301, 77]),
      ("saf, mem:>", &[2301, 77]),
      // unknown keys are text
      ("node:work", &[502]),
      ("foo:>1", &[]),
      // no value for a key
      ("power:<1000", &[631, 2301, 4410, 77, 500, 501, 502]),
      ("!power:>1", &[1, 2301, 77, 500, 501, 502]),
      ("path:/", &[1, 2301, 4410, 77, 500, 501, 502]),
    ];

    for (filter, expected) in cases {
      let query = Query::parse(filter);
      let hits: Vec<_> = procs.iter().filter(|p| query.matches(p)).map(|p| p.pid).collect();
      assert_eq!(hits, *expected, "{filter:?}");
    }

    // the working directory: of this process, none of a process that can't be read
    let own = proc(std::process::id() as i32, "own", 0.0, 0, None, 0.0);
    let gone = proc(i32::MAX, "gone", 0.0, 0, None, 0.0);
    assert!(Query::parse("dir:/").matches(&own));
    assert!(!Query::parse("dir:/no/such/dir").matches(&own));
    assert!(!Query::parse("dir:/").matches(&gone));

    // bad words: known keys with values that aren't valid
    for word in ["mem:", "mem:>", "!MEM:>1x", "user:", "dir:", "cpu:>1g"] {
      assert!(is_bad(word), "{word}");
    }

    for word in ["", "!", "node", "!node", "foo:", "mem:1g", "mem:>1G", "user:root"] {
      assert!(!is_bad(word), "{word}");
    }

    // total rows: terms with a plain word, not the ignored or negated ones
    let query = Query::parse(" Saf  mem:>1, mem:>, !bookmark");
    assert_eq!(query.any.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["saf  mem:>1"]);
  }
}
