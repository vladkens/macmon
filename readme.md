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
- 📊 CPU utilization per reported CPU/GPU cluster
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
  pipe   Output metrics in JSON format
  debug  Print debug information
  help   Print this message or the help of the given subcommand(s)

Options:
  -i, --interval <INTERVAL>  Update interval in milliseconds [default: 1000]
  -h, --help                 Print help
  -V, --version              Print version

Controls:
  c - change color
  v - switch charts view: gauge / sparkline
  q - quit
```

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

Cluster keys in the JSON output are now derived directly from the reported channel names, for example `ECPU`, `PCPU1`, `GPUPH`, or `GPU`.

### Output

```jsonc
{
  "timestamp": "2025-02-24T20:38:15.427569+00:00",
  "usage": {
    "cpu": {
      "ECPU": [1181, 0.33062646, 4],   // (Frequency MHz, Cluster Load, Cores)
      "PCPU": [1974, 0.06072718, 4],   // (Frequency MHz, Cluster Load, Cores)
      "PCPU1": [2055, 0.1648863, 4]    // (Frequency MHz, Cluster Load, Cores)
    },
    "gpu": {
      "GPUPH": [461, 0.21497859, 10]   // (Frequency MHz, Cluster Load, GPU Units)
    }
  },
  "power": {
    "cpu": 0.20486385,                    // Watts
    "gpu": 0.017451683,                   // Watts
    "ram": 0.11635789,                    // Watts
    "sys": 5.876533,                      // Watts
    "gpu_ram": 0.0009615385,              // Watts
    "ane": 0.0,                           // Watts
    "all": 0.22231553                     // Watts
  },
  "temp": {
    "cpu_temp_avg": 43.73614,         // Celsius
    "gpu_temp_avg": 36.95167          // Celsius
  },
  "memory": {
    "ram_total": 25769803776,         // Bytes
    "ram_usage": 20985479168,         // Bytes
    "swap_total": 4294967296,         // Bytes
    "swap_usage": 2602434560          // Bytes
  }
}
```

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
