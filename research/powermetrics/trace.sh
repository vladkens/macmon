#!/bin/sh

set -eu

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root: sudo $0 [interval_ms] [reports]" >&2
  exit 1
fi

interval_ms="${1:-1000}"
reports="${2:-30}"

case "$interval_ms" in
  ''|*[!0-9]*)
    echo "interval_ms must be a positive integer" >&2
    exit 1
    ;;
esac

case "$reports" in
  ''|*[!0-9]*)
    echo "reports must be a positive integer" >&2
    exit 1
    ;;
esac

if [ "$interval_ms" -eq 0 ] || [ "$reports" -eq 0 ]; then
  echo "interval_ms and reports must be greater than zero" >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
frida_trace=$(command -v frida-trace)
run_id=$(date +%Y%m%d-%H%M%S)
run_dir="$script_dir/out/$run_id-${interval_ms}ms"
output_owner="${SUDO_UID:-$(stat -f %u "$script_dir")}:${SUDO_GID:-$(stat -f %g "$script_dir")}"
frida_dir="$run_dir/frida"
trace_log="$run_dir/trace.log"
plist_log="$run_dir/powermetrics.pliststream"
summary_log="$run_dir/summary.txt"
environment_log="$run_dir/environment.txt"
powermetrics_copy="$run_dir/powermetrics"

mkdir -p "$frida_dir/__handlers__"
chown "$output_owner" "$script_dir/out"
trap 'chown -R "$output_owner" "$run_dir" 2>/dev/null || true' EXIT
cp -R "$script_dir/handlers/." "$frida_dir/__handlers__"
cp -f /usr/bin/powermetrics "$powermetrics_copy"
chmod +x "$powermetrics_copy"
codesign --remove-signature "$powermetrics_copy"
codesign --force --sign - "$powermetrics_copy"

{
  date
  sw_vers
  uname -a
  csrutil status || true
  nvram boot-args || true
  "$frida_trace" --version
  codesign -dvv "$powermetrics_copy"
} >"$environment_log" 2>&1

echo "run: $run_dir"
echo "spawning powermetrics: interval=${interval_ms}ms reports=$reports"

if ! (
  cd "$frida_dir"
  "$frida_trace" \
    -i "IOReportCreateSubscription" \
    -i "IOReportCreateSamples" \
    -i "IOReportCreateSamplesDelta" \
    -f "$powermetrics_copy" -- \
    --samplers cpu_power \
    -i "$interval_ms" \
    -n "$reports" \
    --format plist \
    -o "$plist_log"
) >"$trace_log" 2>&1; then
  tail -40 "$trace_log" >&2
  exit 1
fi

python3 "$script_dir/summarize.py" "$trace_log" "$plist_log" | tee "$summary_log"
