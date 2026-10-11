# `macmon` – Mac Monitor

<div align="center">

[<img src="https://badges.ws/github/assets-dl/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/release/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/license/vladkens/macmon" />](https://github.com/vladkens/macmon/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`macmon` is a sudoless performance monitor for Apple Silicon Macs. It reads real-time CPU / GPU / ANE power usage, temperatures, and memory stats through a private macOS API — the same data `powermetrics` exposes — without requiring root access.

<div align="center">
  <img src="https://github.com/vladkens/macmon/blob/assets/macmon-v0.9.png?raw=true" alt="preview" />
</div>

## 🌟 Features

- 🚫 Runs without sudo
- ⚡ Real-time CPU / GPU / ANE power usage
- 📊 CPU frequency-scaled and active ratios per cluster
- 💾 RAM / Swap usage
- 📈 Historical charts with average and max values
- 🌡️ Average CPU / GPU temperature
- 📋 Process list with per-process CPU, memory, power and GPU usage
- 🎨 Follows your terminal's color scheme
- 🪟 Can be displayed in a small window
- 🦀 Written in Rust

## 📥 Installation

Install [`macmon`](https://formulae.brew.sh/formula/macmon) using [brew](https://brew.sh/):

```sh
brew install macmon
```

<details>
<summary>Other installation methods</summary>

Install using [MacPorts](https://ports.macports.org/port/macmon/):

```sh
sudo port install macmon
```

Install using [Cargo](https://crates.io/crates/macmon):

```sh
cargo install macmon
```

Install using [Nix](https://search.nixos.org/packages?show=macmon&from=0&size=50&type=packages&query=macmon):

```sh
nix-env -i macmon
```

</details>

## 🚀 Usage

```sh
Usage: macmon [OPTIONS] [COMMAND]

Commands:
  pipe    Output metrics in JSON format
  serve   Serve metrics over HTTP
  debug   Print debug information
  stress  Generate load for testing metrics
  help    Print this message or the help of the given subcommand(s)

Options:
  -i, --interval <INTERVAL>  Update interval in milliseconds [default: 1000]
  -h, --help                 Print help
  -V, --version              Print version
```

### Interactive mode

Run `macmon` without a subcommand to open the terminal UI: the metric boxes on top and the process list below. Press `?` in the app to see the keys.

Press `/` to filter the process list. A filter is a comma-separated list of terms, each matched case-insensitively against the process name or pid: `safari, cargo` shows processes matching any term, and a term starting with `!` hides its matches (`chrome, !helper`). `Enter` keeps the filter, `Esc` clears it.

### JSON output

You can use the `pipe` subcommand to output metrics in JSON format, which makes it suitable for piping into other tools or scripts. For example:

```sh
macmon pipe | jq
```

This command runs `macmon` in "pipe" mode and sends the output to `jq` for pretty-printing.

You can also specify the number of samples to collect using the `-s` or `--samples` parameter (default: `0`, which runs indefinitely), and set the update interval in milliseconds using the `-i` or `--interval` parameter (default: `1000` ms). For example:

```sh
macmon pipe -s 10 -i 500 | jq
```

This will collect 10 samples with an update interval of 500 milliseconds.

### HTTP server

You can use the `serve` subcommand to expose metrics over HTTP. This is useful for integrating with monitoring systems like [Prometheus](https://prometheus.io/) and [Grafana](https://grafana.com/).

```sh
macmon serve                   # default port 9090, interval 1000ms
macmon serve --host 127.0.0.1  # listen on localhost only
macmon serve -p 8080           # custom port
macmon serve -i 500            # sampling interval 500ms
macmon serve &                 # run in background
```

Two endpoints are available:

| Endpoint       | Format     | Description                                                                                       |
| -------------- | ---------- | ------------------------------------------------------------------------------------------------- |
| `GET /json`    | JSON       | Current metrics snapshot (same format as `pipe --soc-info`)                                       |
| `GET /metrics` | Prometheus | Metrics in [Prometheus text format](https://prometheus.io/docs/instrumenting/exposition_formats/) |

#### launchd service

To start `macmon serve` automatically on login and keep it running:

```sh
macmon serve --install                   # install and start (default port 9090)
macmon serve --port 8080 --install       # with custom port
macmon serve --host 127.0.0.1 --install  # listen on localhost only
macmon serve --uninstall                 # stop and remove
```

This creates a launchd agent at `~/Library/LaunchAgents/com.macmon.plist` that auto-starts on login and restarts on crash.

#### Prometheus and Grafana

The `/metrics` endpoint exposes metrics in Prometheus format. See [`examples/grafana`](examples/grafana) for a local demo stack with Prometheus and Grafana.

<details>
<summary>Prometheus output example</summary>

```
# HELP macmon_cpu_temp_celsius Average CPU temperature in Celsius
# TYPE macmon_cpu_temp_celsius gauge
macmon_cpu_temp_celsius{chip="Apple M3 Pro"} 47.3

# HELP macmon_cpu_power_watts CPU power consumption in Watts
# TYPE macmon_cpu_power_watts gauge
macmon_cpu_power_watts{chip="Apple M3 Pro"} 8.42

# HELP macmon_fan_speed_rpm Fan speed in revolutions per minute
# TYPE macmon_fan_speed_rpm gauge
macmon_fan_speed_rpm{chip="Apple M3 Pro",fan="fan0"} 1234

# HELP macmon_cpu_scaled_ratio Combined frequency-scaled CPU ratio (0–1), weighted by core count
# TYPE macmon_cpu_scaled_ratio gauge
macmon_cpu_scaled_ratio{chip="Apple M3 Pro"} 0.037

# HELP macmon_cpu_active_ratio Combined CPU active residency ratio (not frequency-scaled, 0–1), weighted by core count
# TYPE macmon_cpu_active_ratio gauge
macmon_cpu_active_ratio{chip="Apple M3 Pro"} 0.092

# HELP macmon_cpu_tier_freq_mhz CPU tier frequency in MHz
# TYPE macmon_cpu_tier_freq_mhz gauge
macmon_cpu_tier_freq_mhz{chip="Apple M3 Pro",tier="E"} 1100
macmon_cpu_tier_freq_mhz{chip="Apple M3 Pro",tier="P"} 1800

# HELP macmon_cpu_tier_scaled_ratio CPU tier frequency-scaled ratio (0–1)
# TYPE macmon_cpu_tier_scaled_ratio gauge
macmon_cpu_tier_scaled_ratio{chip="Apple M3 Pro",tier="E"} 0.083
macmon_cpu_tier_scaled_ratio{chip="Apple M3 Pro",tier="P"} 0.015

# HELP macmon_cpu_tier_active_ratio CPU tier active residency ratio (not frequency-scaled, 0–1)
# TYPE macmon_cpu_tier_active_ratio gauge
macmon_cpu_tier_active_ratio{chip="Apple M3 Pro",tier="E"} 0.18
macmon_cpu_tier_active_ratio{chip="Apple M3 Pro",tier="P"} 0.04
```

`macmon_ecpu_*` and `macmon_pcpu_*` are still exported for the lowest and the highest tier.

</details>

### Stress testing

Use `macmon stress` to generate load while checking metric behavior:

```sh
macmon stress
macmon stress pulse --duration 30
macmon stress cpu --duration 30
macmon stress cpu --workers 8 --duration 30
macmon stress gpu --duration 30
macmon stress all --duration 30
```

The default `pulse` mode generates a predictable cyclic CPU load with a fixed 50% duty cycle on half of the logical CPUs. The `cpu` and `gpu` modes continuously load only the selected processor, while `all` loads both. The `cpu` and `all` modes use all logical CPUs unless `--workers` is specified; `--workers` has no effect in `gpu` mode.

## 📊 Metrics

### Power sources and macOS 27

macOS 27 broke the CPU/ANE power source that macmon previously used without root. The IOReport `Energy Model` counters can remain visible but stop updating: refreshing them now requires the private `com.apple.private.pmgr.nrg.reporting` entitlement. Apple's signed `powermetrics` has this entitlement and can keep using the named channels. Running macmon with `sudo` does not grant the same access.

The working rootless workaround we found is to read hidden energy reports from the AppleCLPC driver through IOReport. These reports use opaque IDs instead of discoverable CPU/GPU/ANE channel names, so macmon has to identify the counters and verify their units. This leaves macmon maintaining undocumented mappings for data that Apple's own tool can still read directly.

Both the CLI and the Rust library use known CLPC counters automatically, and `Sampler::new()` retains legacy fallbacks for older systems. New chips or driver versions may need additional mappings. On an unsupported configuration, zero CPU/ANE readings can mean unavailable counters rather than zero consumption. See [docs/clpc-discovery.md](docs/clpc-discovery.md) for details.

### Output format

The `pipe` command and the HTTP `/json` endpoint return the same metrics:

<details>
<summary>JSON example</summary>

```jsonc
{
  "timestamp": "2025-02-24T20:38:15.427569+00:00",
  "temp": {
    "cpu_temp_avg": 43.73614, // Celsius, null when no sensor has a valid reading
    "gpu_temp_avg": 36.95167, // Celsius, null when no sensor has a valid reading
  },
  "memory": {
    "ram_total": 25769803776, // Bytes
    "ram_usage": 20985479168, // Bytes
    "swap_total": 4294967296, // Bytes
    "swap_usage": 2602434560, // Bytes
  },
  "fans": [
    { "name": "fan0", "rpm": 999, "max_rpm": 4900 },
    { "name": "fan1", "rpm": 1200, "max_rpm": 5200 },
  ],
  "cpu_scaled_ratio": 0.036854, // Combined frequency-scaled CPU ratio (weighted by core count, 0–1)
  "cpu_active_ratio": 0.092, // Combined active residency ratio (not frequency-scaled, weighted by core count, 0–1)
  "cpu_tiers": [ // CPU core types from the lowest to the highest: E, P on M1–M4; E, S on M5; P, S on M5 Pro/Max; E, P, S on M6
    {
      "label": "E",
      "freq_mhz": 1100, // Tier frequency
      "scaled_ratio": 0.082656614, // Frequency-scaled ratio (0–1)
      "active_ratio": 0.18, // Active residency (not frequency-scaled, 0–1)
      "cores": [
        { "die_id": 0, "core_id": 0, "freq_mhz": 1600, "scaled_ratio": 0.14, "active_ratio": 0.24 },
        { "die_id": 0, "core_id": 1, "freq_mhz": 1700, "scaled_ratio": 0.12, "active_ratio": 0.2 },
      ],
    },
    {
      "label": "P",
      "freq_mhz": 1800,
      "scaled_ratio": 0.015181795,
      "active_ratio": 0.04,
      "cores": [
        { "die_id": 0, "core_id": 0, "freq_mhz": 2100, "scaled_ratio": 0.05, "active_ratio": 0.08 },
        { "die_id": 0, "core_id": 1, "freq_mhz": 2200, "scaled_ratio": 0.07, "active_ratio": 0.06 },
      ],
    },
  ],
  // Deprecated, use cpu_tiers: ecpu_* is the lowest tier, pcpu_* the highest
  "ecpu_freq_mhz": 1100,
  "ecpu_scaled_ratio": 0.082656614,
  "ecpu_active_ratio": 0.18,
  "pcpu_freq_mhz": 1800,
  "pcpu_scaled_ratio": 0.015181795,
  "pcpu_active_ratio": 0.04,
  "ecpu_cores": [/* cores of the first tier */],
  "pcpu_cores": [/* cores of the last tier */],
  "gpu_freq_mhz": 461, // GPU frequency
  "gpu_scaled_ratio": 0.021497859, // Frequency-scaled ratio (0–1)
  "gpu_active_ratio": 0.09, // GPU active residency ratio (not frequency-scaled, 0–1)
  "cpu_power": 0.20486385, // Watts
  "gpu_power": 0.017451683, // Watts
  "ane_power": 0.0, // Watts
  "all_power": 0.22231553, // Watts
  "sys_power": 5.876533, // Watts
  "ram_power": 0.11635789, // Watts
  "gpu_ram_power": 0.0009615385, // GPU SRAM power, Watts
}
```

</details>

### Active and scaled ratios

`active_ratio` is the share of the sampling interval during which the processor was doing any work. `scaled_ratio` is the same measure adjusted for operating frequency, showing the share of the processor's maximum possible capacity that was used.

For interval $T$, $t_i$ is the time spent doing work at frequency $f_i$, and $f_\text{max}$ is the hardware maximum frequency for the CPU cluster or GPU. The product $T f_\text{max}$ is the theoretical maximum frequency-time the processor could deliver at full load without thermal or power throttling:

```math
R_\text{active} = \frac{\sum_i t_i}{T}
\qquad
R_\text{scaled} = \frac{\sum_i t_i f_i}{T f_\text{max}}
```

CPU ratios are calculated per core and averaged across the cluster. A core that did no work contributes zero; combined CPU ratios average all cores from all clusters:

```math
R_\text{cluster} = \frac{1}{N_\text{cores}}\sum_j R_j
```

Examples:

- Full interval at half the maximum frequency → `active = 1.0`, `scaled = 0.5`.
- Half the interval at the maximum frequency → `active = 0.5`, `scaled = 0.5`.
- One of ten cores working at maximum frequency → cluster `active = scaled = 0.1`.

## 📚 Library usage

`macmon` can be used as a Rust library to collect Apple Silicon metrics in your own applications.

Add it to your project:

```sh
cargo add macmon --no-default-features
```

The default `app` feature enables the `macmon` executable and its terminal UI dependencies. Disable default features when using `macmon` only as a library.

Run the [demo example](examples/demo.rs) from this repository:

```sh
cargo run --example demo
```

Then use the `Sampler` to collect metrics:

```rust
use macmon::Sampler;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sampler = Sampler::new()?;

    // collect metrics over a 1000ms window
    let metrics = sampler.get_metrics(1000)?;

    println!("CPU power:  {:.2} W", metrics.cpu_power);
    println!("GPU power:  {:.2} W", metrics.gpu_power);
    match metrics.temp.cpu_temp_avg {
        Some(value) => println!("CPU temp:   {value:.1} °C"),
        None => println!("CPU temp:   N/A"),
    }
    println!("RAM usage:  {} / {} bytes", metrics.memory.ram_usage, metrics.memory.ram_total);
    for tier in &metrics.cpu_tiers {
        println!("{}-CPU:      {} MHz  {:.1}%", tier.label, tier.freq_mhz, tier.scaled_ratio * 100.0);
    }

    Ok(())
}
```

`get_metrics(duration_ms)` blocks the calling thread while collecting one IOReport delta over the complete interval. For a UI, server, or async application, create the sampler inside a dedicated worker thread and send the completed metrics back through a channel:

```rust
use std::{sync::mpsc, thread};

use macmon::Sampler;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut sampler = Sampler::new().expect("failed to create sampler");

        while let Ok(metrics) = sampler.get_metrics(1000) {
            if tx.send(metrics).is_err() {
                break;
            }
        }
    });

    // Use recv() in a consumer thread or try_recv() in a non-blocking event loop.
    let metrics = rx.recv()?;
    println!("CPU power: {:.2} W", metrics.cpu_power);

    Ok(())
}
```

Creating `Sampler` inside the worker keeps its low-level macOS handles on that thread. In an async runtime, use its blocking-thread facility rather than calling `get_metrics` directly from an executor worker.

### C library

`libmacmon` exposes the same rootless metrics to Swift, Python, Go or C apps as JSON: power, temperatures, CPU tiers with per-core frequency and ratios, GPU, memory, swap, fans and SoC info. Processes are not exposed. Each release (and the `dev` pre-release, with `-dev` names) ships, for arm64 and macOS 11+:

- `libmacmon-vX.Y.Z.tar.gz`: `include/macmon.h` and `lib/libmacmon.dylib`, for C, Python and Go.
- `CMacmon.xcframework-vX.Y.Z.zip`: a static library with the `CMacmon` module for Swift, and its SwiftPM checksum in `CMacmon.xcframework-vX.Y.Z.zip.sha256`.

The dylib is ad-hoc signed, not notarized: download it with `curl` (no quarantine attribute), or run `xattr -dr com.apple.quarantine` on what a browser downloaded. `make capi` and `make xcframework` build both from source into `out/`.

| Function                    | Description                                                             |
| --------------------------- | ----------------------------------------------------------------------- |
| `macmon_abi_version`        | `MACMON_ABI_VERSION` the library was built with                         |
| `macmon_version`            | macmon's version, e.g. `"0.10.0"`                                       |
| `macmon_sampler_new`        | Creates a sampler and primes its baseline                               |
| `macmon_sampler_next_json`  | One sample, `interval_ms` after the previous one (`0`: at once); blocks |
| `macmon_sampler_free`       | Frees a sampler (`NULL`: no-op)                                         |
| `macmon_get_soc_info_json`  | SoC info                                                                |
| `macmon_string_free`        | Frees a returned JSON string (`NULL`: no-op)                            |
| `macmon_last_error_message` | This thread's last error, `NULL` after a success                        |

- **Threading:** a sampler is not thread-safe: calls on it must not overlap; the caller orders them (a mutex or a serial queue). It may move between threads. Different samplers are independent. As in Rust, sample on a worker thread.
- **Errors:** functions that return a status (`0` OK, `1` invalid argument, `2` init failed, `3` sample failed, `4` internal error: if it came from a sampler call, the sampler can only be freed) set `*out` to `NULL` first and, on an error, the message read with `macmon_last_error_message()`. A panic's message is also printed to the host app's stderr by Rust's panic hook.
- **Versioning:** every JSON document carries `"schema"`, equal to `macmon_abi_version()`. Symbols and keys are only added; removing, renaming or retyping one bumps the version. Check it once at start.
- **JSON:** the [output format](#output-format) of `pipe`, except `timestamp_ms` (Unix milliseconds) instead of the RFC 3339 `timestamp`, no v0.7 aliases (`cpu_usage_pct`, `ecpu_usage`, `pcpu_usage`, `gpu_usage`) and no deprecated `ecpu_*`/`pcpu_*` keys.

Python with `ctypes` (`out_json` as `POINTER(c_char)`, so it can be passed back to `macmon_string_free`):

```python
import json
from ctypes import CDLL, POINTER, byref, c_char, c_char_p, c_uint32, c_void_p, string_at

lib = CDLL("lib/libmacmon.dylib")
lib.macmon_abi_version.restype = c_uint32
lib.macmon_last_error_message.restype = c_char_p
lib.macmon_sampler_new.argtypes = [POINTER(c_void_p)]
lib.macmon_sampler_next_json.argtypes = [c_void_p, c_uint32, POINTER(POINTER(c_char))]
lib.macmon_string_free.argtypes = [POINTER(c_char)]
lib.macmon_sampler_free.argtypes = [c_void_p]

def check(status):
    if status != 0:
        raise RuntimeError(lib.macmon_last_error_message().decode())

assert lib.macmon_abi_version() == 1, "unsupported libmacmon"
sampler = c_void_p()
check(lib.macmon_sampler_new(byref(sampler)))
try:
    out = POINTER(c_char)()
    check(lib.macmon_sampler_next_json(sampler, 1000, byref(out)))  # blocks for 1000 ms
    metrics = json.loads(string_at(out))
    lib.macmon_string_free(out)
finally:
    lib.macmon_sampler_free(sampler)

print(f"CPU {metrics['cpu_power']:.2f} W, GPU {metrics['gpu_power']:.2f} W")
```

Swift with SwiftPM: add `.binaryTarget(name: "CMacmon", url: "https://github.com/vladkens/macmon/releases/download/vX.Y.Z/CMacmon.xcframework-vX.Y.Z.zip", checksum: "<the .sha256 file>")` and depend on `CMacmon`; no linker flags are needed.

```swift
import CMacmon
import Foundation

struct Metrics: Decodable { let timestampMs: Int64; let cpuPower: Double; let gpuPower: Double }

func check(_ status: macmon_status_t) throws {
  guard status == 0 else {
    throw NSError(domain: "macmon", code: Int(status), userInfo: [
      NSLocalizedDescriptionKey: macmon_last_error_message().map { String(cString: $0) } ?? "",
    ])
  }
}

precondition(macmon_abi_version() == 1, "unsupported libmacmon")
var sampler: OpaquePointer?
try check(macmon_sampler_new(&sampler))
defer { macmon_sampler_free(sampler) }

var json: UnsafeMutablePointer<CChar>?
try check(macmon_sampler_next_json(sampler, 1000, &json))  // blocks for 1000 ms
let data = Data(bytes: json!, count: strlen(json!))
macmon_string_free(json)

let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase
let metrics = try decoder.decode(Metrics.self, from: data)
print("CPU \(metrics.cpuPower) W, GPU \(metrics.gpuPower) W")
```

## 🤝 Contributing

All contributions are welcome! Feel free to open an issue or submit a pull request.

## 📝 License

Distributed under the [MIT License](LICENSE).

## 🔍 See also

- [tlkh/asitop](https://github.com/tlkh/asitop) – The original tool. Written in Python, requires sudo.
- [dehydratedpotato/socpowerbud](https://github.com/dehydratedpotato/socpowerbud) – Written in Objective-C, sudoless, no TUI.
- [op06072/NeoAsitop](https://github.com/op06072/NeoAsitop) – Written in Swift, sudoless.
- [graelo/pumas](https://github.com/graelo/pumas) – Written in Rust, requires sudo.
- [context-labs/mactop](https://github.com/context-labs/mactop) – Written in Go, requires sudo.
