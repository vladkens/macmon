# powermetrics IOReport sampling research

This directory contains a reproducible Frida trace for the private IOReport
calls used by `powermetrics --samplers cpu_power`.

The trace answers two questions:

1. Are the quick `IOReportCreateSamples` calls repeated measurements of one
   counter set, or independent channel subscriptions?
2. Does the `powermetrics` interval change only the output cadence, or also the
   IOReport delta window?

## Requirements

- Apple Silicon Mac.
- SIP disabled from macOS Recovery:

  ```sh
  csrutil disable
  ```

- Frida installed with `pip install frida-tools` and `frida-trace` available
  in `PATH`. The recorded results used Frida 17.16.4.
- These boot arguments, followed by a reboot:

  ```sh
  sudo nvram boot-args="-arm64e_preview_abi thid_should_crash=0 tss_should_crash=0"
  sudo reboot
  ```

Verify the environment after reboot:

```sh
csrutil status
nvram boot-args
frida-trace --version
```

Disabling SIP materially reduces macOS security. Use a development machine and
restore the previous security configuration after tracing. If `boot-args` was
empty before the experiment, remove it with:

```sh
sudo nvram -d boot-args
```

Re-enable SIP from Recovery with `csrutil enable`.

## Run

Default: 30 reports at a requested 1000 ms interval.

```sh
sudo ./research/powermetrics/trace.sh
```

The two interval experiments from this research are:

```sh
sudo ./research/powermetrics/trace.sh 250 20
sudo ./research/powermetrics/trace.sh 100 30
```

Each run creates an ignored directory under `out/` containing:

```text
environment.txt
powermetrics
powermetrics.pliststream
trace.log
summary.txt
frida/__handlers__/
```

The script copies `/usr/bin/powermetrics`, removes its Apple platform
signature, and applies an ad-hoc signature before Frida spawn. On macOS 26.5.2,
spawning the original hardened binary crashed `frida-helper` with
`EXC_GUARD: THREAD_SET_STATE`; the local ad-hoc copy avoids that restriction.
No Apple binary is stored in Git.

## Result

The experiment was recorded on:

```text
Hardware: Mac14,7, Apple M2
macOS: 26.5.2 (25F84)
Frida: 17.16.4
Sampler: powermetrics --samplers cpu_power
Date: 2026-07-23
```

### Subscription layout

`powermetrics` creates three successful IOReport subscriptions:

```text
CPU Complex Performance States:   4 channels
CPU Core Performance States:      8 channels
Energy Model:                   136 channels
```

Two additional empty channel queries return a null subscription and are never
sampled.

The complex subscription contains ECPU/ECPM/PCPU/PCPM state channels. The core
subscription contains one performance-state channel for each of the four
efficiency and four performance cores. The energy subscription contains CPU,
GPU, ANE, DRAM, display, media, SRAM, and other SoC energy channels.

### Sampling sequence

Initialization takes two baseline samples from the complex-state subscription
and one from each of the core-state and energy subscriptions.

Every reporting cycle after initialization is:

```text
complex_current = CreateSamples(complex_subscription)
complex_delta = CreateSamplesDelta(complex_previous, complex_current)

core_current = CreateSamples(core_subscription)
core_delta = CreateSamplesDelta(core_previous, core_current)

energy_current = CreateSamples(energy_subscription)
energy_delta = CreateSamplesDelta(energy_previous, energy_current)

wait for the requested interval
```

The quick burst is therefore not repeated temporal sampling. It is one sample
for each of three independent channel sets.

The older macmon issue #10 trace showed eight quick sample/delta pairs. The new
argument and subscription trace demonstrates that counting calls alone cannot
be interpreted as eight measurements that `powermetrics` averages. They were
most likely separate subscriptions or channel sets on that macOS/hardware
combination.

### Interval behavior

Recorded plist `elapsed_ns`:

| Requested interval | Reports | Median actual window |     Minimum |     Maximum |
| -----------------: | ------: | -------------------: | ----------: | ----------: |
|            1000 ms |      30 |          1009.305 ms | 1003.461 ms | 1011.380 ms |
|             250 ms |      20 |           258.146 ms |  252.884 ms |  259.619 ms |
|             100 ms |      30 |           107.943 ms |  102.892 ms |  108.919 ms |

At every tested interval, each successful subscription is sampled exactly once
per reporting cycle. There is no fixed-rate hidden sampling and no subdivision
of the requested interval.

The actual delta window is slightly longer than the requested interval because
collection and processing take additional time. `powermetrics` records the real
window in plist `elapsed_ns`.

### CPU `IDLE` and `DOWN` states

Apple's public `IOStateReporter` contract describes generic state-residency
accounting, not CPU-specific power states. State identifiers are
provider-defined 64-bit values, and the public Apple Silicon CPU driver source
that assigns `IDLE` and `DOWN` is unavailable. The XNU implementation records
elapsed time against the previous state on every transition and updates the
current state's residency when a report is produced:

- [IOStateReporter documentation](https://developer.apple.com/documentation/driverkit/iostatereporter)
- [IOStateReporter state IDs](https://github.com/apple-oss-distributions/xnu/blob/main/iokit/IOKit/IOKernelReporters.h#L965-L1004)
- [IOStateReporter transition accounting](https://github.com/apple-oss-distributions/xnu/blob/main/iokit/Kernel/IOStateReporter.cpp#L613-L710)
- [IOStateReporter report update](https://github.com/apple-oss-distributions/xnu/blob/main/iokit/Kernel/IOStateReporter.cpp#L802-L865)

Inspection of the Apple Silicon `powermetrics` binary on macOS 26.5.2 found two
accepted CPU state layouts:

```text
[IDLE, frequency states...]
[DOWN, IDLE, frequency states...]
```

Its `cpu_power_arm.m` logic treats `IDLE` and `DOWN` as distinct states but
excludes both from active residency. The observed calculations are:

```text
total = DOWN + IDLE + sum(frequency states)
active = sum(frequency states)
active_ratio = active / total
idle_ratio = IDLE / total
down_ratio = DOWN / total
active_frequency =
  sum(frequency * frequency_residency) / active
```

`man powermetrics` likewise describes average CPU frequency as frequency while
the processor was executing, excluding idle time. The binary exposes separate
`idle_ratio`, `down_ratio`, `idle residency`, and `down residency` labels.

The distinction is physical but not topological: both values change over time.
Public measurements show valid M4 cores entering `DOWN`, and an M5 Max P-core
cluster spending about 99.8% of an interval in `DOWN` while another cluster is
active. The same M5 Max topology is reported as two valid performance clusters
in [macmon issue #47](https://github.com/vladkens/macmon/issues/47). Therefore,
an all-`DOWN` sample does not prove that a core or cluster is disabled:

- [M4 P-core power states](https://eclecticlight.co/2024/11/11/inside-m4-chips-p-cores/)
- [M5 Max CPU frequency measurements](https://eclecticlight.co/2026/04/09/please-help-update-cpu-frequencies-for-apple-silicon-macs/)

The binary inspection can be repeated with:

```sh
man powermetrics
strings /usr/bin/powermetrics | grep -E 'idle residency|down residency|idle_ratio|down_ratio'
otool -arch arm64e -tvV /usr/bin/powermetrics
```

This is an observed implementation detail, not a documented ABI. Re-check it
after material macOS or hardware changes.

### Consequence for macmon

Before this research, macmon divided every requested interval into four
adjacent windows, derived metrics for each window, and averaged the four
derived values. That behavior did not reproduce `powermetrics`: Apple's tool
derives one report from the delta between adjacent report-level samples for
each subscription.

The production sampler should therefore:

1. Create one IOReport delta over the complete requested interval.
2. Use the real elapsed duration for energy-to-power conversion.
3. Derive active frequency only from frequency-state residency.
4. Include `IDLE`, `DOWN`, and `OFF` in the total residency denominator.
5. Preserve every recognized core channel even when its active residency is
   zero; topology must not be inferred from a dynamic power state.

Four-way sampling was presentation smoothing introduced to make bursty graphs
resemble Activity Monitor. If smoothing is desired, it belongs after raw
interval metrics have been derived, not inside the library's collection
semantics.
