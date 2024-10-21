# `macmon` – Mac Monitor

<div align="center">

Sudoless performance monitoring CLI tool for Apple Silicon processors.

[<img src="https://badgen.net/github/assets-dl/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badgen.net/github/release/vladkens/macmon" />](https://github.com/vladkens/macmon/releases)
[<img src="https://badgen.net/github/license/vladkens/macmon" />](https://github.com/vladkens/macmon/blob/main/LICENSE)
[<img src="https://badgen.net/static/-/buy%20me%20a%20coffee/ff813f?icon=buymeacoffee&label" alt="donate" />](https://buymeacoffee.com/vladkens)

</div>

<div align="center">
  <img src="https://github.com/vladkens/macmon/blob/assets/macmon.png?raw=true" alt="preview" />
</div>

## Motivation

Apple Silicon processors don't provide an easy way to see live power consumption. I was interested in this information while testing local LLM models. `asitop` is a nice and simple TUI to quickly see current metrics, but it reads data from `powermetrics` and requires root privileges. `macmon` uses a private macOS API to gather metrics (essentially the same as `powermetrics`) but runs without sudo. 🎉

## 🌟 Features

- 🚫 Works without sudo
- ⚡ Real-time CPU / GPU / ANE power usage
- 📊 CPU utilization per cluster
- 💾 RAM / Swap usage
- 📈 Historical charts + avg / max values
- 🌡️ Average CPU / GPU temperature
- 🎨 Switchable colors (6 variants)
- 🪟 Can be rendered in a small window
- 🦀 Written in Rust

## 🍺 Install via Homebrew

```sh
brew install vladkens/tap/macmon
```

## 📦 Install from source

1. Install [Rust toolchain](https://www.rust-lang.org/tools/install)

2. Clone the repo:

```sh
git clone https://github.com/vladkens/macmon.git && cd macmon
```

3. Build and run:

```sh
cargo run -r
```

4. (Optionally) Binary can be moved to bin folder:

```sh
sudo cp target/release/macmon /usr/local/bin
```

## 🚀 Usage

```sh
Usage: macmon [OPTIONS] [COMMAND]

Commands:
  raw   Print raw metrics data instead of TUI
  help  Print this message or the help of the given subcommand(s)

Options:
  -i, --interval <INTERVAL>  Update interval in milliseconds [default: 1000]
  -h, --help                 Print help
  -V, --version              Print version

Controls:
  c - change color
  v - switch charts view: gauge / sparkline
  q - quit
```

## 🤝 Contributing
We love contributions! Whether you have ideas, suggestions, or bug reports, feel free to open an issue or submit a pull request. Your input is essential in helping us improve `macmon` 💪

## 📝 License
`macmon` is distributed under the MIT License. For more details, check out the LICENSE.

## 🔍 See also
- [tlkh/asitop](https://github.com/tlkh/asitop) – Original tool. Python, requires sudo.
- [dehydratedpotato/socpowerbud](https://github.com/dehydratedpotato/socpowerbud) – ObjectiveC, sudoless, no TUI.
- [op06072/NeoAsitop](https://github.com/op06072/NeoAsitop) – Swift, sudoless.
- [graelo/pumas](https://github.com/graelo/pumas) – Rust, requires sudo.
- [context-labs/mactop](https://github.com/context-labs/mactop) – Go, requires sudo.

---

*PS: One More Thing... Remember, monitoring your Mac's performance with `macmon` is like having a personal trainer for your processor — keeping those cores in shape! 💪*
