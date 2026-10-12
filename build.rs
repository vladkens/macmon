// Version the app shows (`MACMON_VERSION`): `git describe` for builds between releases, e.g.
// `0.9.0-12-g3672a1b`, the crate version for releases (`MACMON_RELEASE` set or HEAD at its tag)
// and outside this repository (crates.io sources, `cargo package` verification).

use std::env::var;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
  let out = Command::new("git").args(args).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn describe(root: &Path) -> Option<String> {
  let dirs = git(&["rev-parse", "--show-toplevel", "--git-dir", "--git-common-dir"])?;
  let dirs: Vec<PathBuf> = dirs.lines().map(|x| root.join(x)).collect();
  if dirs.len() != 3 || dirs[0].canonicalize().ok()? != root.canonicalize().ok()? {
    return None;
  }

  // in a worktree HEAD is in its own git dir, refs and packed-refs in the common one
  let watch = [dirs[1].join("HEAD"), dirs[2].join("refs"), dirs[2].join("packed-refs")];
  for path in watch.iter().chain([&root.join("src"), &root.join("Cargo.toml")]) {
    if path.exists() {
      println!("cargo:rerun-if-changed={}", path.display());
    }
  }

  let desc = git(&["describe", "--tags", "--dirty", "--match", "v[0-9]*"])?;
  Some(desc.strip_prefix('v').unwrap_or(&desc).to_string())
}

fn main() {
  println!("cargo:rerun-if-env-changed=MACMON_RELEASE");
  if var("CARGO_FEATURE_APP").is_err() {
    return;
  }

  let pkg = var("CARGO_PKG_VERSION").unwrap();
  let root = PathBuf::from(var("CARGO_MANIFEST_DIR").unwrap());
  let dev = if var("MACMON_RELEASE").is_ok() { None } else { describe(&root) };
  println!("cargo:rustc-env=MACMON_VERSION={}", dev.unwrap_or(pkg));
}
