use std::env;
use std::path::{Path, PathBuf};

fn workspace_root(manifest_dir: &Path) -> PathBuf {
  manifest_dir
    .parent()
    .and_then(Path::parent)
    .expect("workspace root")
    .to_path_buf()
}

fn main() {
  let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
  let profile = env::var("PROFILE").expect("profile");
  let target_dir = env::var("CARGO_TARGET_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|_| workspace_root(&manifest_dir).join("target"));

  let lib_dir = target_dir.join(&profile);

  println!("cargo:rustc-link-search=native={}", lib_dir.display());
  println!("cargo:rustc-link-lib=dylib=macmon");
  println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
}
