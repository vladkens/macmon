#!/bin/sh
# Usage: scripts/loc.sh [REF] - production lines of Rust sources (tests excluded), compared with git REF if given.
set -eu
cd "$(dirname "$0")/.."
ref=${1:-}

# Lines of one file on stdin: stops at `#[cfg(test)] mod tests`, skips other `#[cfg(test)]` items by brace depth.
prod() {
  awk '
    /^[ \t]*#\[cfg\(test\)\]/ { skip = 1; d = 0; next }
    skip && /^mod tests/ { exit }
    skip { d += gsub(/[{]/, "&") - gsub(/[}]/, "&"); if (d <= 0 && /[;}][ \t]*$/) skip = 0; next }
    { n++ }
    END { print n + 0 }'
}

count() { # REF DIR: production lines of the .rs files directly in DIR, in the working tree or at REF
  if [ -n "$1" ]; then files=$(git ls-tree --name-only "$1" "$2/" | grep '\.rs$' || true); else files=$(ls "$2"/*.rs); fi
  for f in $files; do
    if [ -n "$1" ]; then git show "$1:$f"; else cat "$f"; fi | prod
  done | awk '{ s += $1 } END { print s + 0 }'
}

row() { # NAME NOW BASE
  if [ -n "$ref" ]; then printf '%-8s %6s %12s %6s\n' "$1" "$2" "$3" "$4"; else printf '%-8s %6s\n' "$1" "$2"; fi
}

row part now "$ref" diff
t=0 tb=0
for p in library:src app:src/app tui:src/app/tui; do
  n=$(count "" "${p#*:}") b=0
  [ -z "$ref" ] || b=$(count "$ref" "${p#*:}")
  row "${p%%:*}" "$n" "$b" "$(printf '%+d' $((n - b)))"
  t=$((t + n)) tb=$((tb + b))
done
row total "$t" "$tb" "$(printf '%+d' $((t - tb)))"
