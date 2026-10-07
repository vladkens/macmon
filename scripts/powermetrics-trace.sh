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
repo_dir=$(dirname -- "$script_dir")
out_dir="$repo_dir/out"
frida_trace=$(command -v frida-trace)
run_id=$(date +%Y%m%d-%H%M%S)
run_dir="$out_dir/powermetrics-$run_id-${interval_ms}ms"
output_owner="${SUDO_UID:-$(stat -f %u "$repo_dir")}:${SUDO_GID:-$(stat -f %g "$repo_dir")}"
frida_dir="$run_dir/frida"
handlers_dir="$frida_dir/__handlers__/libIOReport.dylib"
trace_log="$run_dir/trace.log"
plist_log="$run_dir/powermetrics.pliststream"
summary_log="$run_dir/summary.txt"
environment_log="$run_dir/environment.txt"
powermetrics_copy="$run_dir/powermetrics"

mkdir -p "$handlers_dir"
chown "$output_owner" "$out_dir"
trap 'chown -R "$output_owner" "$run_dir" 2>/dev/null || true' EXIT

# frida-trace loads one handler per traced function from __handlers__/<module>/<function>.js
cat >"$handlers_dir/IOReportCreateSubscription.js" <<'EOF'
function describeCfObject(value) {
  if (value.isNull() || !ObjC.available) {
    return null;
  }

  try {
    return new ObjC.Object(value).toString();
  } catch (error) {
    return `<description failed: ${error}>`;
  }
}

defineHandler({
  onEnter(log, args, state) {
    state.requestedChannels = args[1];
    state.subscribedChannelsOut = args[2];
    state.flags = args[3];
    state.options = args[4];
    state.requestedDescription = describeCfObject(args[1]);
  },

  onLeave(log, retval, state) {
    let subscribedChannels = ptr(0);
    if (!state.subscribedChannelsOut.isNull()) {
      try {
        subscribedChannels = state.subscribedChannelsOut.readPointer();
      } catch (error) {
        subscribedChannels = ptr(0);
      }
    }

    log(
      `IOReportCreateSubscription(requested=${state.requestedChannels}, ` +
        `subscribed=${subscribedChannels}, flags=${state.flags}, ` +
        `options=${state.options}) -> subscription=${retval}`,
    );
    log(
      `IOReportSubscriptionChannels(subscription=${retval}, ` +
        `requested=${JSON.stringify(state.requestedDescription)}, ` +
        `subscribed=${JSON.stringify(describeCfObject(subscribedChannels))})`,
    );
  },
});
EOF

cat >"$handlers_dir/IOReportCreateSamples.js" <<'EOF'
defineHandler({
  onEnter(log, args, state) {
    state.subscription = args[0];
    state.channels = args[1];
    state.options = args[2];
  },

  onLeave(log, retval, state) {
    log(
      `IOReportCreateSamples(subscription=${state.subscription}, ` +
        `channels=${state.channels}, options=${state.options}) -> sample=${retval}`,
    );
  },
});
EOF

cat >"$handlers_dir/IOReportCreateSamplesDelta.js" <<'EOF'
defineHandler({
  onEnter(log, args, state) {
    state.a = args[0];
    state.b = args[1];
    state.options = args[2];
  },

  onLeave(log, retval, state) {
    log(
      `IOReportCreateSamplesDelta(a=${state.a}, b=${state.b}, ` +
        `options=${state.options}) -> delta=${retval}`,
    );
  },
});
EOF

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

python3 "$script_dir/powermetrics-summarize.py" "$trace_log" "$plist_log" | tee "$summary_log"
