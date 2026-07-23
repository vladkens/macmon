# `macmon` – Mac Monitor

<div align="center">

[<img src="https://badges.ws/github/assets-dl/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/release/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badges.ws/github/license/vladkens/macmon" />](https://github.com/vladkens/macmon/blob/main/LICENSE)
[<img src="https://badges.ws/badge/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

`macmon` is a sudoless performance monitor for Apple Silicon Macs. It reads real-time CPU / GPU / ANE power usage, temperatures, and memory stats through a private macOS API — the same data `powermetrics` exposes — without requiring root access.

<div align="center">
  <img src="https://github.com/vladkens/macmon/blob/assets/macmon.png?raw=true" alt="preview" />
</div>

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

Controls:
  c - change color
  v - switch charts view: gauge / sparkline
  d - toggle detailed CPU/RAM view
  q - quit
```

## 📚 Library Usage

`macmon` can be used as a Rust library to collect Apple Silicon metrics in your own applications.

Add it to your project:

```sh
cargo add macmon
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
    println!("eCPU:       {} MHz  {:.1}%", metrics.ecpu_freq_mhz, metrics.ecpu_usage_ratio * 100.0);
    println!("pCPU:       {} MHz  {:.1}%", metrics.pcpu_freq_mhz, metrics.pcpu_usage_ratio * 100.0);

    Ok(())
}
```

`get_metrics(duration_ms)` blocks the calling thread while collecting one
IOReport delta over the complete interval. For a UI, server, or async
application, create the sampler inside a dedicated worker thread and send the
completed metrics back through a channel:

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

Creating `Sampler` inside the worker keeps its low-level macOS handles on that
thread. In an async runtime, use its blocking-thread facility rather than
calling `get_metrics` directly from an executor worker.

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

<details>
<summary>Output</summary>

```jsonc
{
  "timestamp": "2025-02-24T20:38:15.427569+00:00",
  "temp": {
    "cpu_temp_avg": 43.73614, // Celsius
    "gpu_temp_avg": 36.95167, // Celsius
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
  "cpu_usage_ratio": 0.036854, // Combined effective CPU usage (frequency-scaled, weighted by core count, 0–1)
  "cpu_active_ratio": 0.092, // Combined active residency ratio (not frequency-scaled, weighted by core count, 0–1)
  "ecpu_freq_mhz": 1181, // Average frequency while active
  "ecpu_usage_ratio": 0.082656614, // Effective usage (frequency-scaled, 0–1)
  "ecpu_active_ratio": 0.18, // Active residency (not frequency-scaled, 0–1)
  "pcpu_freq_mhz": 1974, // Average frequency while active
  "pcpu_usage_ratio": 0.015181795, // Effective usage (frequency-scaled, 0–1)
  "pcpu_active_ratio": 0.04, // Active residency (not frequency-scaled, 0–1)
  "ecpu_cores": [
    { "die_id": 0, "core_id": 0, "freq_mhz": 1600, "usage_ratio": 0.14, "active_ratio": 0.24 },
    { "die_id": 0, "core_id": 1, "freq_mhz": 1700, "usage_ratio": 0.12, "active_ratio": 0.2 },
  ],
  "pcpu_cores": [
    { "die_id": 0, "core_id": 0, "freq_mhz": 2100, "usage_ratio": 0.05, "active_ratio": 0.08 },
    { "die_id": 0, "core_id": 1, "freq_mhz": 2200, "usage_ratio": 0.07, "active_ratio": 0.06 },
  ],
  "gpu_freq_mhz": 461, // Average frequency while active
  "gpu_usage_ratio": 0.021497859, // Effective usage (frequency-scaled, 0–1)
  "gpu_active_ratio": 0.09, // GPU active residency ratio (not frequency-scaled, 0–1)
  "cpu_power": 0.20486385, // Watts
  "gpu_power": 0.017451683, // Watts
  "ane_power": 0.0, // Watts
  "all_power": 0.22231553, // Watts
  "sys_power": 5.876533, // Watts
  "ram_power": 0.11635789, // Watts
  "gpu_ram_power": 0.0009615385, // Watts (not sure what it means)
}
```

</details>

Deprecated compatibility fields remain available in Rust and serialized JSON:
`cpu_usage_pct` → `cpu_usage_ratio`, `ecpu_usage` →
`(ecpu_freq_mhz, ecpu_usage_ratio)`, `pcpu_usage` →
`(pcpu_freq_mhz, pcpu_usage_ratio)`, and `gpu_usage` →
`(gpu_freq_mhz, gpu_usage_ratio)`.

## 🌐 HTTP Server

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

### Running as a background service (launchd)

To start `macmon serve` automatically on login and keep it running:

```sh
macmon serve --install                   # install and start (default port 9090)
macmon serve --port 8080 --install       # with custom port
macmon serve --host 127.0.0.1 --install  # listen on localhost only
macmon serve --uninstall                 # stop and remove
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

</details>

## 🧪 Stress Test

Use `macmon stress` to generate load while checking metric behavior:

```sh
macmon stress
macmon stress --duration 30
macmon stress --full --duration 30
macmon stress --full --workers 8 --duration 30
```

The default remains the predictable cyclic CPU load with a fixed 50% duty cycle and 4 CPU workers. Use `--full` for continuous CPU and GPU load; when `--workers` is omitted, full mode uses all logical CPUs.

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
