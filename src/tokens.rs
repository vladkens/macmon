use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

/// Token usage snapshot for a single Claude Code session.
#[derive(Debug, Clone, Default)]
pub struct SessionTokens {
  pub input_tokens: u64,
  pub output_tokens: u64,
  pub cache_read_tokens: u64,
  pub cache_creation_tokens: u64,
}


/// Aggregated token usage across all active Claude Code sessions.
#[derive(Debug, Clone, Default)]
pub struct TokenMetrics {
  pub input_tokens: u64,
  pub output_tokens: u64,
  pub cache_read_tokens: u64,
  pub cache_creation_tokens: u64,
  pub session_count: u32,
}

impl TokenMetrics {
  pub fn total(&self) -> u64 {
    self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_creation_tokens
  }
}

/// Tracks file read positions so we only parse new lines on each poll.
struct FileState {
  offset: u64,
  tokens: SessionTokens,
}

/// Reads Claude Code session data from ~/.claude to track token usage.
pub struct TokenReader {
  claude_dir: PathBuf,
  /// session_id -> file tracking state
  files: HashMap<String, FileState>,
  /// cached mapping: session_id -> jsonl file path
  paths: HashMap<String, PathBuf>,
}

impl TokenReader {
  pub fn new() -> Self {
    let home = std::env::var("HOME").unwrap_or_default();
    let claude_dir = PathBuf::from(home).join(".claude");
    Self { claude_dir, files: HashMap::new(), paths: HashMap::new() }
  }

  /// Discover active sessions by checking which session PIDs are still running.
  fn discover_active_sessions(&self) -> Vec<(String, String, String)> {
    // Returns: Vec<(session_id, cwd, pid)>
    let sessions_dir = self.claude_dir.join("sessions");
    let mut active = Vec::new();

    let entries = match std::fs::read_dir(&sessions_dir) {
      Ok(e) => e,
      Err(_) => return active,
    };

    for entry in entries.flatten() {
      let path = entry.path();
      if path.extension().map_or(true, |e| e != "json") {
        continue;
      }

      let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => continue,
      };

      // Parse minimally — avoid pulling in full serde for this
      let session_id = json_str_field(&content, "sessionId");
      let cwd = json_str_field(&content, "cwd");
      let pid_str = json_num_field(&content, "pid");

      if session_id.is_empty() || cwd.is_empty() {
        continue;
      }

      // Check if process is still alive
      if let Ok(pid) = pid_str.parse::<i32>() {
        if unsafe { libc::kill(pid, 0) } == 0 {
          active.push((session_id, cwd, pid_str));
        }
      }
    }

    active
  }

  /// Convert a cwd to the Claude project directory name.
  fn cwd_to_project_dir(cwd: &str) -> String {
    cwd.replace('/', "-")
  }

  /// Find the JSONL file for a session.
  fn find_jsonl(&self, session_id: &str, cwd: &str) -> Option<PathBuf> {
    let project_dir = Self::cwd_to_project_dir(cwd);
    let path = self.claude_dir.join("projects").join(&project_dir).join(format!("{}.jsonl", session_id));
    if path.exists() { Some(path) } else { None }
  }

  /// Read new token usage data from a JSONL file starting at the given offset.
  fn read_incremental(path: &PathBuf, state: &mut FileState) {
    let file = match std::fs::File::open(path) {
      Ok(f) => f,
      Err(_) => return,
    };

    let metadata = match file.metadata() {
      Ok(m) => m,
      Err(_) => return,
    };

    // If file is smaller than our offset, it was truncated — reset
    if metadata.len() < state.offset {
      state.offset = 0;
      state.tokens = SessionTokens::default();
    }

    // If no new data, skip
    if metadata.len() == state.offset {
      return;
    }

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(state.offset)).is_err() {
      return;
    }

    let mut line = String::new();
    loop {
      line.clear();
      match reader.read_line(&mut line) {
        Ok(0) => break,
        Ok(_) => {
          // Only parse assistant messages with usage data
          if line.contains("\"type\":\"assistant\"") && line.contains("\"usage\"") {
            parse_usage_line(&line, &mut state.tokens);
          }
        }
        Err(_) => break,
      }
    }

    state.offset = reader.stream_position().unwrap_or(state.offset);
  }

  /// Poll all active sessions and return aggregated token metrics.
  pub fn get_metrics(&mut self) -> TokenMetrics {
    let sessions = self.discover_active_sessions();

    // Update path cache for new sessions
    for (sid, cwd, _) in &sessions {
      if !self.paths.contains_key(sid) {
        if let Some(path) = self.find_jsonl(sid, cwd) {
          self.paths.insert(sid.clone(), path);
        }
      }
    }

    // Read incremental updates
    let active_ids: Vec<String> = sessions.iter().map(|(sid, _, _)| sid.clone()).collect();
    for sid in &active_ids {
      if let Some(path) = self.paths.get(sid) {
        let state = self.files.entry(sid.clone()).or_insert(FileState {
          offset: 0,
          tokens: SessionTokens::default(),
        });
        Self::read_incremental(path, state);
      }
    }

    // Clean up stale sessions
    self.files.retain(|k, _| active_ids.contains(k));
    self.paths.retain(|k, _| active_ids.contains(k));

    // Aggregate
    let mut metrics = TokenMetrics::default();
    metrics.session_count = active_ids.len() as u32;
    for state in self.files.values() {
      metrics.input_tokens += state.tokens.input_tokens;
      metrics.output_tokens += state.tokens.output_tokens;
      metrics.cache_read_tokens += state.tokens.cache_read_tokens;
      metrics.cache_creation_tokens += state.tokens.cache_creation_tokens;
    }

    metrics
  }
}

/// Extract a string field from a JSON line without full parsing.
fn json_str_field(json: &str, key: &str) -> String {
  let pattern = format!("\"{}\":\"", key);
  if let Some(start) = json.find(&pattern) {
    let start = start + pattern.len();
    if let Some(end) = json[start..].find('"') {
      return json[start..start + end].to_string();
    }
  }
  String::new()
}

/// Extract a numeric field from a JSON line without full parsing.
fn json_num_field(json: &str, key: &str) -> String {
  let pattern = format!("\"{}\":", key);
  if let Some(start) = json.find(&pattern) {
    let start = start + pattern.len();
    let rest = json[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    return rest[..end].to_string();
  }
  String::new()
}

/// Parse token usage from a JSONL assistant message line.
fn parse_usage_line(line: &str, tokens: &mut SessionTokens) {
  // Find the "usage" object and extract token counts
  // We look for the top-level usage fields (not the nested iterations ones)
  if let Some(usage_start) = line.find("\"usage\":{") {
    let usage_str = &line[usage_start..];

    // Extract each token count
    if let Some(v) = extract_token_value(usage_str, "\"input_tokens\":") {
      tokens.input_tokens += v;
    }
    if let Some(v) = extract_token_value(usage_str, "\"output_tokens\":") {
      tokens.output_tokens += v;
    }
    if let Some(v) = extract_token_value(usage_str, "\"cache_read_input_tokens\":") {
      tokens.cache_read_tokens += v;
    }
    if let Some(v) = extract_token_value(usage_str, "\"cache_creation_input_tokens\":") {
      tokens.cache_creation_tokens += v;
    }
  }
}

fn extract_token_value(json: &str, key: &str) -> Option<u64> {
  // Find the first occurrence of the key (top-level usage, not iterations)
  let start = json.find(key)? + key.len();
  let rest = json[start..].trim_start();
  let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
  rest[..end].parse().ok()
}
