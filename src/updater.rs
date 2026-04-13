use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Result of a version check against GitHub releases.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
  pub latest_version: String,
  pub download_url: String,
}

static UPDATE_STATE: OnceLock<Mutex<Option<UpdateInfo>>> = OnceLock::new();

pub fn get_update_state() -> &'static Mutex<Option<UpdateInfo>> {
  UPDATE_STATE.get_or_init(|| Mutex::new(None))
}

/// Compare two semver strings (e.g. "0.6.1" vs "0.7.0").
/// Returns true if `latest` is newer than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
  let parse = |s: &str| -> Vec<u32> {
    s.trim_start_matches('v').split('.').filter_map(|p| p.parse().ok()).collect()
  };
  let cur = parse(current);
  let lat = parse(latest);
  lat > cur
}

/// Check GitHub for the latest release. Blocking — run in a background thread.
pub fn check_for_update() -> Option<UpdateInfo> {
  let current = env!("CARGO_PKG_VERSION");
  let url = "https://api.github.com/repos/vladkens/macmon/releases/latest";

  let body = http_get(url)?;

  // Extract tag_name
  let tag = json_str_field(&body, "tag_name")?;
  if !is_newer(current, &tag) {
    return None;
  }

  // Extract download URL from first asset
  let download_url = extract_asset_url(&body)
    .unwrap_or_else(|| format!("https://github.com/vladkens/macmon/releases/tag/{}", tag));

  Some(UpdateInfo { latest_version: tag, download_url })
}

/// Perform the update: download tarball, extract, replace binary.
pub fn perform_update(info: &UpdateInfo) -> Result<(), String> {
  let current_exe = std::env::current_exe().map_err(|e| format!("Can't find current exe: {}", e))?;

  // Download the tarball
  let tarball = http_get_bytes(&info.download_url).ok_or("Failed to download update")?;

  // Extract the macmon binary from the tarball
  let binary = extract_binary_from_tarball(&tarball).ok_or("Failed to extract binary from tarball")?;

  // Write to a temp file next to the current binary, then atomic rename
  let dir = current_exe.parent().ok_or("Can't find binary directory")?;
  let tmp_path = dir.join(".macmon_update_tmp");

  std::fs::write(&tmp_path, &binary).map_err(|e| format!("Failed to write temp file: {}", e))?;

  // Make executable
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&tmp_path, perms)
      .map_err(|e| format!("Failed to set permissions: {}", e))?;
  }

  // Atomic rename
  std::fs::rename(&tmp_path, &current_exe)
    .map_err(|e| format!("Failed to replace binary: {}", e))?;

  Ok(())
}

/// Spawn a background thread to check for updates, storing result in global state.
pub fn check_in_background() {
  std::thread::spawn(|| {
    if let Some(info) = check_for_update() {
      let mut state = get_update_state().lock().unwrap();
      *state = Some(info);
    }
  });
}

// MARK: HTTP helpers (minimal, no dependencies)

fn http_get(url: &str) -> Option<String> {
  let bytes = http_get_bytes(url)?;
  String::from_utf8(bytes).ok()
}

fn http_get_bytes(url: &str) -> Option<Vec<u8>> {
  // Use /usr/bin/curl since we can't add HTTP deps
  let output = std::process::Command::new("/usr/bin/curl")
    .args(["-sSL", "-H", "User-Agent: macmon-updater", "--max-time", "10", url])
    .output()
    .ok()?;

  if !output.status.success() {
    return None;
  }

  Some(output.stdout)
}

// MARK: JSON helpers

fn json_str_field(json: &str, key: &str) -> Option<String> {
  let pattern = format!("\"{}\":\"", key);
  let start = json.find(&pattern)? + pattern.len();
  let end = json[start..].find('"')?;
  Some(json[start..start + end].to_string())
}

fn extract_asset_url(json: &str) -> Option<String> {
  // Find "browser_download_url" in the first asset
  json_str_field(json, "browser_download_url")
}

// MARK: Tarball extraction

fn extract_binary_from_tarball(tarball: &[u8]) -> Option<Vec<u8>> {
  // The tarball is gzip-compressed. Use /usr/bin/tar to extract.
  let tmp_dir = std::env::temp_dir().join("macmon_update");
  let _ = std::fs::create_dir_all(&tmp_dir);

  let tarball_path = tmp_dir.join("macmon.tar.gz");
  std::fs::write(&tarball_path, tarball).ok()?;

  // Extract
  let status = std::process::Command::new("/usr/bin/tar")
    .args(["-xzf", tarball_path.to_str()?, "-C", tmp_dir.to_str()?])
    .status()
    .ok()?;

  if !status.success() {
    let _ = std::fs::remove_dir_all(&tmp_dir);
    return None;
  }

  // Find the macmon binary in extracted contents
  let binary_path = find_binary_in_dir(&tmp_dir)?;
  let binary = std::fs::read(&binary_path).ok()?;

  // Cleanup
  let _ = std::fs::remove_dir_all(&tmp_dir);

  Some(binary)
}

fn find_binary_in_dir(dir: &PathBuf) -> Option<PathBuf> {
  // Look for a file named "macmon" (the binary)
  for entry in walkdir(dir) {
    if entry.file_name().map_or(false, |n| n == "macmon") {
      if entry.is_file() {
        return Some(entry);
      }
    }
  }
  None
}

/// Simple recursive directory walk (no extra deps).
fn walkdir(dir: &PathBuf) -> Vec<PathBuf> {
  let mut results = Vec::new();
  if let Ok(entries) = std::fs::read_dir(dir) {
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        results.extend(walkdir(&path));
      } else {
        results.push(path);
      }
    }
  }
  results
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_is_newer() {
    assert!(is_newer("0.6.1", "0.7.0"));
    assert!(is_newer("0.6.1", "v0.7.0"));
    assert!(is_newer("0.6.1", "1.0.0"));
    assert!(!is_newer("0.7.0", "0.7.0"));
    assert!(!is_newer("0.7.0", "0.6.1"));
    assert!(!is_newer("1.0.0", "0.9.9"));
  }

  #[test]
  fn test_json_str_field() {
    let json = r#"{"tag_name":"v0.7.0","name":"Release"}"#;
    assert_eq!(json_str_field(json, "tag_name"), Some("v0.7.0".to_string()));
    assert_eq!(json_str_field(json, "missing"), None);
  }
}
