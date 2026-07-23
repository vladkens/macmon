#!/usr/bin/env python3

import json
import plistlib
import re
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path


TIME_RE = re.compile(r"^\s*(?P<time>\d+(?:\.\d+)?) ms\s+")
EVENT_RE = re.compile(r"^\s*\d+(?:\.\d+)? ms\s+(?P<event>IOReport\w+)\(")
SAMPLE_RE = re.compile(
    r"IOReportCreateSamples\(subscription=(?P<subscription>0x[0-9a-f]+), "
    r"channels=(?P<channels>0x[0-9a-f]+), options=(?P<options>0x[0-9a-f]+)\) "
    r"-> sample=(?P<sample>0x[0-9a-f]+)"
)
DELTA_RE = re.compile(
    r"IOReportCreateSamplesDelta\(a=(?P<a>0x[0-9a-f]+), "
    r"b=(?P<b>0x[0-9a-f]+), options=(?P<options>0x[0-9a-f]+)\) "
    r"-> delta=(?P<delta>0x[0-9a-f]+)"
)
SUBSCRIPTION_RE = re.compile(
    r"IOReportCreateSubscription\(requested=(?P<requested>0x[0-9a-f]+), "
    r"subscribed=(?P<subscribed>0x[0-9a-f]+), flags=(?P<flags>0x[0-9a-f]+), "
    r"options=(?P<options>0x[0-9a-f]+)\) "
    r"-> subscription=(?P<subscription>0x[0-9a-f]+)"
)
CHANNELS_RE = re.compile(
    r"IOReportSubscriptionChannels\(subscription=(?P<subscription>0x[0-9a-f]+), "
    r"requested=(?P<requested>.*), subscribed=(?P<subscribed>.*)\)"
)
INTERVAL_RE = re.compile(r"--samplers cpu_power -i (?P<interval>\d+)")


def parse_trace(path: Path):
    text = path.read_text(errors="replace")
    events = Counter()
    samples = []
    deltas = []
    subscriptions = []
    channel_sets = []

    for sequence, line in enumerate(text.splitlines()):
        time_match = TIME_RE.match(line)
        timestamp = float(time_match.group("time")) if time_match else 0.0

        event_match = EVENT_RE.match(line)
        if event_match:
            events[event_match.group("event")] += 1

        match = SAMPLE_RE.search(line)
        if match:
            samples.append(
                {"time": timestamp, "sequence": sequence, **match.groupdict()}
            )

        match = DELTA_RE.search(line)
        if match:
            deltas.append(
                {"time": timestamp, "sequence": sequence, **match.groupdict()}
            )

        match = SUBSCRIPTION_RE.search(line)
        if match:
            subscriptions.append(match.groupdict())

        match = CHANNELS_RE.search(line)
        if match:
            channel_sets.append(match.groupdict())

    interval_match = INTERVAL_RE.search(text)
    interval = int(interval_match.group("interval")) if interval_match else None
    return events, samples, deltas, subscriptions, channel_sets, interval


def parse_elapsed(path: Path):
    elapsed = []
    for chunk in path.read_bytes().split(b"\0"):
        if not chunk.strip():
            continue
        try:
            document = plistlib.loads(chunk)
        except Exception:
            continue
        if "elapsed_ns" in document:
            elapsed.append(document["elapsed_ns"] / 1_000_000)
    return elapsed


def format_stats(values):
    return (
        f"median={statistics.median(values):.3f}ms "
        f"min={min(values):.3f}ms max={max(values):.3f}ms"
    )


def main():
    if len(sys.argv) not in {2, 3}:
        print("usage: summarize.py <trace.log> [powermetrics.pliststream]", file=sys.stderr)
        return 2

    trace_path = Path(sys.argv[1])
    plist_path = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    events, samples, deltas, subscriptions, channel_sets, interval = parse_trace(
        trace_path
    )

    print(f"trace: {trace_path.name}")
    if interval is not None:
        print(f"requested interval: {interval}ms")
    print("events:")
    for event, count in events.most_common():
        print(f"  {event}: {count}")

    if not samples:
        print("no IOReport samples captured")
        return 1

    by_subscription = defaultdict(list)
    for sample in samples:
        by_subscription[sample["subscription"]].append(sample)

    print("sample streams:")
    for subscription, stream in by_subscription.items():
        gaps = [
            current["time"] - previous["time"]
            for previous, current in zip(stream, stream[1:])
        ]
        threshold = interval * 0.5 if interval is not None else 50
        periodic_gaps = [gap for gap in gaps if gap >= threshold]
        period = format_stats(periodic_gaps) if periodic_gaps else "period=n/a"
        print(f"  {subscription}: samples={len(stream)} {period}")

    sampled_subscriptions = set(by_subscription)
    print("created subscriptions:")
    for subscription in subscriptions:
        state = (
            "sampled"
            if subscription["subscription"] in sampled_subscriptions
            else "not sampled"
        )
        print(
            f"  {subscription['subscription']}: "
            f"requested={subscription['requested']} "
            f"subscribed={subscription['subscribed']} {state}"
        )

    print("channel sets:")
    for channel_set in channel_sets:
        requested = (
            json.loads(channel_set["requested"])
            if channel_set["requested"] != "null"
            else ""
        )
        groups = Counter(
            re.findall(r'IOReportGroupName = "([^"]+)"', requested)
        )
        subgroups = Counter(
            re.findall(r'IOReportSubGroupName = "([^"]+)"', requested)
        )
        group_text = ", ".join(
            f"{name} ({count})" for name, count in groups.items()
        )
        subgroup_text = ", ".join(
            f"{name} ({count})" for name, count in subgroups.items()
        )
        print(
            f"  {channel_set['subscription']}: "
            f"channels={requested.count('DriverID =')} "
            f"groups={group_text or '-'} subgroups={subgroup_text or '-'}"
        )

    sample_to_subscription = {
        sample["sample"]: sample["subscription"] for sample in samples
    }
    linked_a = sum(delta["a"] in sample_to_subscription for delta in deltas)
    linked_b = sum(delta["b"] in sample_to_subscription for delta in deltas)
    same_stream = sum(
        sample_to_subscription.get(delta["a"])
        == sample_to_subscription.get(delta["b"])
        and delta["a"] in sample_to_subscription
        for delta in deltas
    )
    print("delta linkage:")
    print(f"  previous argument captured: {linked_a}/{len(deltas)}")
    print(f"  current argument captured: {linked_b}/{len(deltas)}")
    print(f"  previous/current from same stream: {same_stream}/{len(deltas)}")

    if plist_path is not None:
        elapsed = parse_elapsed(plist_path)
        print(f"plist reports: {len(elapsed)}")
        if elapsed:
            print(f"actual elapsed: {format_stats(elapsed)}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
