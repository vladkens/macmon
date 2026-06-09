# `macmon` – Mac Monitor

<div align="center">

A sudoless performance monitoring CLI tool for Apple Silicon processors.

[<img src="https://badges.ws/github/assets-dl/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/release/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/license/vladkens/macmon" />](https://github.com/vladkens/macmon/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

<div align="center">
  <img src="https://github.com/vladkens/macmon/blob/assets/macmon.png?raw=true" alt="preview" />
</div>

## Motivation

Apple Silicon processors don't provide an easy way to view live power consumption. I was interested in this data while testing local LLM models. `asitop` is a nice and simple TUI for quickly checking current metrics, but it reads data from `powermetrics` and requires root privileges. `macmon` uses a private macOS API to gather similar metrics (essentially the same as `powermetrics`), but runs without sudo. 🎉

## 🌟 Features

- 🚫 Runs without sudo
- ⚡ Real-time CPU / GPU / ANE power usage
- 📊 CPU effective usage per cluster
- 💾 RAM / Swap usage
- 📈 Historical charts with average and max values
- 🌡️ Average CPU / GPU temperature
- 🎨 Switchable color themes (6 variants)
- 🪟 Can be displayed in a small window
- 🦀 Written in Rust

## 📥 Installation

- Install [`macmon`](https://formulae.brew.sh/formula/macmon) using [brew](https://brew.sh/):

```sh
brew install macmon
```

- Install [`macmon`](https://ports.macports.org/port/macmon/) using [MacPorts](https://macports.org/):

```sh
sudo port install macmon
```

- Install [`macmon`](https://crates.io/crates/macmon) using [Cargo](https://crates.io/):

```sh
cargo install macmon
```

- Install [`macmon`](https://search.nixos.org/packages?show=macmon&from=0&size=50&type=packages&query=macmon) using [Nix](https://nixos.org/):

```sh
nix-env -i macmon
```

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

Controls:
  c - change color
  v - switch charts view: gauge / sparkline
  d - toggle detailed CPU/RAM view
  q - quit
```

## 🧪 Stress Test

Use `macmon stress` to generate load while checking metric behavior:

```sh
macmon stress
macmon stress --duration 30
macmon stress --full --duration 30
macmon stress --full --workers 8 --duration 30
```

The default remains the predictable cyclic CPU load with a fixed 50% duty cycle and 4 CPU workers. Use `--full` for continuous CPU and GPU load; when `--workers` is omitted, full mode uses all logical CPUs.

## 🚰 Piping

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

### Output

```jsonc
{
  "timestamp": "2025-02-24T20:38:15.427569+00:00",
  "temp": {
    "cpu_temp_avg": 43.73614,         // Celsius
    "gpu_temp_avg": 36.95167          // Celsius
  },
  "memory": {
    "ram_total": 25769803776,         // Bytes
    "ram_usage": 20985479168,         // Bytes
    "swap_total": 4294967296,         // Bytes
    "swap_usage": 2602434560          // Bytes
  },
  "fans": [
    { "name": "fan0", "rpm": 999, "max_rpm": 4900 },
    { "name": "fan1", "rpm": 1200, "max_rpm": 5200 }
  ],
  "ecpu_usage": [1181, 0.082656614],  // (Frequency MHz, effective usage ratio) - cluster aggregate
  "pcpu_usage": [1974, 0.015181795],  // (Frequency MHz, effective usage ratio) - cluster aggregate
  "ecpu_core_usages": [[1600, 0.14], [1700, 0.12]], // Per-core (Frequency MHz, effective usage ratio), experimental
  "pcpu_core_usages": [[2100, 0.05], [2200, 0.07]], // Per-core (Frequency MHz, effective usage ratio), experimental
  "cpu_usage_pct": 0.036854,          // Combined effective CPU usage (frequency-scaled, weighted by core count, 0–1)
  "cpu_active_ratio": 0.092,          // Combined active residency ratio (not frequency-scaled, weighted by core count, 0–1)
  "ecpu_active_ratio": 0.18,          // Efficiency CPU active residency ratio (not frequency-scaled, 0–1)
  "pcpu_active_ratio": 0.04,          // Performance CPU active residency ratio (not frequency-scaled, 0–1)
  "ecpu_core_active_ratios": [0.24, 0.20, 0.18, 0.10],
  "pcpu_core_active_ratios": [0.08, 0.06, 0.03, 0.02],
  "gpu_usage": [461, 0.021497859],    // (Frequency MHz, effective usage ratio)
  "gpu_active_ratio": 0.09,           // GPU active residency ratio (not frequency-scaled, 0–1)
  "cpu_power": 0.20486385,            // Watts
  "gpu_power": 0.017451683,           // Watts
  "ane_power": 0.0,                   // Watts
  "all_power": 0.22231553,            // Watts
  "sys_power": 5.876533,              // Watts
  "ram_power": 0.11635789,            // Watts
  "gpu_ram_power": 0.0009615385       // Watts (not sure what it means)
}
```

## 🌐 HTTP Server

You can use the `serve` subcommand to expose metrics over HTTP. This is useful for integrating with monitoring systems like [Prometheus](https://prometheus.io/) and [Grafana](https://grafana.com/).

```sh
macmon serve            # default port 9090, interval 1000ms
macmon serve --host 127.0.0.1  # listen on localhost only
macmon serve -p 8080    # custom port
macmon serve -i 500     # sampling interval 500ms
macmon serve &          # run in background
```

Two endpoints are available:

| Endpoint | Format | Description |
|---|---|---|
| `GET /json` | JSON | Current metrics snapshot (same format as `pipe --soc-info`) |
| `GET /metrics` | Prometheus | Metrics in [Prometheus text format](https://prometheus.io/docs/instrumenting/exposition_formats/) |

### Running as a background service (launchd)

To start `macmon serve` automatically on login and keep it running:

```sh
macmon serve --install              # install and start (default port 9090)
macmon serve --port 8080 --install  # with custom port
macmon serve --host 127.0.0.1 --install  # listen on localhost only
macmon serve --uninstall            # stop and remove
```

This creates a launchd agent at `~/Library/LaunchAgents/com.macmon.plist` that auto-starts on login and restarts on crash.

### Prometheus / Grafana setup

Add a scrape target to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: macmon
    static_configs:
      - targets: ["localhost:9090"]
```

For a ready-to-run local example with Prometheus + Grafana, see [`example-grafana`](example-grafana):

```sh
macmon serve --port 9090
cd example-grafana
docker compose up -d
```

This example provisions:

- Prometheus on `http://localhost:9091`
- Grafana on `http://localhost:9000`
- a prebuilt `Macmon Overview` dashboard

Grafana login:

- username: `macmon`
- password: `macmon`

Then import or build a Grafana dashboard querying metrics such as:

```
macmon_cpu_power_watts{chip="Apple M3 Pro"}
macmon_ecpu_usage_ratio{chip="Apple M3 Pro"}
macmon_memory_ram_used_bytes{chip="Apple M3 Pro"}
```

### Prometheus output example

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

# HELP macmon_cpu_usage_ratio Combined CPU effective usage (frequency-scaled, 0–1), weighted by core count
# TYPE macmon_cpu_usage_ratio gauge
macmon_cpu_usage_ratio{chip="Apple M3 Pro"} 0.037

# HELP macmon_cpu_active_ratio Combined CPU active residency ratio (not frequency-scaled, 0–1), weighted by core count
# TYPE macmon_cpu_active_ratio gauge
macmon_cpu_active_ratio{chip="Apple M3 Pro"} 0.092

# HELP macmon_ecpu_usage_ratio Efficiency CPU cluster effective usage (frequency-scaled, 0–1)
# TYPE macmon_ecpu_usage_ratio gauge
macmon_ecpu_usage_ratio{chip="Apple M3 Pro"} 0.083

# HELP macmon_ecpu_active_ratio Efficiency CPU cluster active residency ratio (not frequency-scaled, 0–1)
# TYPE macmon_ecpu_active_ratio gauge
macmon_ecpu_active_ratio{chip="Apple M3 Pro"} 0.18
```

## 📚 Library Usage

`macmon` can be used as a Rust library to collect Apple Silicon metrics in your own applications.

Add it to your `Cargo.toml`:

```toml
[dependencies]
macmon = { git = "https://github.com/vladkens/macmon" }
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
    println!("CPU temp:   {:.1} °C", metrics.temp.cpu_temp_avg);
    println!("RAM usage:  {} / {} bytes", metrics.memory.ram_usage, metrics.memory.ram_total);
    println!("eCPU:       {} MHz  {:.1}%", metrics.ecpu_usage.0, metrics.ecpu_usage.1 * 100.0);
    println!("pCPU:       {} MHz  {:.1}%", metrics.pcpu_usage.0, metrics.pcpu_usage.1 * 100.0);

    Ok(())
}
```

Use `get_metrics(duration_ms)` in a continuous polling loop. This is the default API for most callers: run the loop in a worker thread or task if you want macmon to manage the sampling cadence and keep the built-in smoothing used by the TUI, `pipe`, and `serve`.

Use `get_metrics_now(stale_after_ms)` only when you want to schedule sampling yourself. It does not sleep or smooth samples: the first call stores a baseline and returns `None`, later calls return metrics for the elapsed window, and stale baselines are discarded after `stale_after_ms`.

## 📦 Build from Source

1. Install [Rust toolchain](https://www.rust-lang.org/tools/install)

2. Clone the repo:

```sh
git clone https://github.com/vladkens/macmon.git && cd macmon
```

3. Build and run:

```sh
cargo run -r
```

## 🤝 Contributing

We love contributions! Whether you have ideas, suggestions, or bug reports, feel free to open an issue or submit a pull request. Your input is essential to helping us improve `macmon`. 💪

## 📝 License

`macmon` is distributed under the MIT License. For more details, check out the LICENSE file.

## 🔍 See also

- [tlkh/asitop](https://github.com/tlkh/asitop) – The original tool. Written in Python, requires sudo.
- [dehydratedpotato/socpowerbud](https://github.com/dehydratedpotato/socpowerbud) – Written in Objective-C, sudoless, no TUI.
- [op06072/NeoAsitop](https://github.com/op06072/NeoAsitop) – Written in Swift, sudoless.
- [graelo/pumas](https://github.com/graelo/pumas) – Written in Rust, requires sudo.
- [context-labs/mactop](https://github.com/context-labs/mactop) – Written in Go, requires sudo.

---

*P.S. One more thing... Monitoring your Mac's performance with `macmon` is like having a personal trainer for your processor — keeping those cores in shape! 💪*
