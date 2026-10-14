#!/bin/sh
# The release archive for vladkens/actions/rust-build: target/distrib/macmon-dev.tar.gz,
# published as macmon-vX.Y.Z.tar.gz on a release (the name the Homebrew formula points to).

set -eu

cargo build --release --locked

# The version cargo built, stamped by rust-build outside release commits (e.g. 0.9.0-12-g3672a1b)
version=$(cargo pkgid | sed 's/.*[#@]//')
actual=$(target/release/macmon -V)
if [ "$actual" != "macmon $version" ]; then
  echo "expected 'macmon $version' from macmon -V, got '$actual'" >&2
  exit 1
fi

mkdir -p target/distrib
tar czf target/distrib/macmon-dev.tar.gz readme.md LICENSE -C target/release macmon
ls -lh target/distrib
