# Fallback sources from mactop

A backup way to get the same power and temperature numbers if Apple breaks the sources macmon reads now, as macOS 27 broke `Energy Model`: the sources [mactop](https://github.com/metaspartan/mactop) (MIT) uses, as last resorts after the ones macmon already has. They stay unused while the current sources work. One PR; the maintainer waived the ~200-line limit for it.

## Behavior

- Power fallbacks, per sample, for CPU, GPU, ANE, DRAM and DCS. Through macOS 26 as on main: CLPC, else `Energy Model`, else PMP (a 0 counts), and SMC only when none of them has a reading. On macOS 27+: CLPC, also at an idle 0; else the first of PMP, `Energy Model`, SMC that reads above 0 and isn't a catch-up spike (above ten times the SMC system total plus 100 W); else the first reading, so a genuine 0 still reads 0. SMC is read only when it's needed. DCS stays `null` without any source.
- `ram_power`: last fallback, the sum of SMC `PZD1` … `PZD9` (one key per die; a `PZD0` total would count twice), none when any of them fails to read.
- `cpu_power`: last fallback, the PMP `Energy` histograms of the CPU clusters (`EACC0`, `PACC0`, `MACCn` and their ` SRAM` channels, in the `PMP` group and the per-die `PMP0`, `PMP1`, …). Each channel reads the residency-weighted average of its power bins (`0.250W`, `1W`, …), the lowest bin counted as 0; a sample with a channel that doesn't read has no PMP value. The `PMP` group's channels, else the per-die groups'. Subscribed only on macOS 27+ without CLPC counters; if that subscription fails, macmon keeps the usual one.
- `sys_power`: the first of SMC `PSTR`, `PDTR`, `PD0R` above 0, else the first readable; `PDTR` and `PD0R` only on Macs without a battery, decided once at start by whether `AppleSmartBattery` exists. Still at least `chip_power`.
- `cpu_temp_avg`: without `Ts0P`, `Ts1P`, `TsOP`; readings of 10 °C and below are invalid (CPU and GPU); an unreadable SMC key is left out instead of failing the sample.
- `macmon debug` adds `PD0R` and every `PZD*` key to #118's `PSTR`, `PDTR`, `PBwo`, `PDBR`, and shows the PMP CPU histograms.

No JSON, Prometheus, library API or config change, and no new dependency.

## Decisions

- No latch: mactop switches to its fallback for the rest of the process on the first zero; macmon decides per sample, and a frozen `Energy Model`'s catch-up (617 W CPU and 59 W DRAM in one sample on M2 / macOS 27.0.1) reads as 0 instead.
- Spike check only on macOS 27+, where `Energy Model` freezes and catches up; with margin `10 × sys + 100 W`. `PSTR` lags ~1 s behind IOReport's interval averages, so a load step reads far above it: 13.6 W of CPU against 1.2 W on M1 / macOS 15 (a stricter `2 × sys + 5 W` zeroed `cpu_power` at 3 of 3 load starts there), 13.06 W against 5.72 W on M5. The margin keeps steps up to an Ultra's ~200 W GPU from a 10 W idle total, while the catch-ups seen are far above it: 617 W against 7 W on M2, kW on M5 Max (mactop#94). The strict `ram_power` / `dcs_power` / `chip_power` checks of #125 stay as they are.
- macOS 26 and older keep main's order exactly: `Energy Model` works there, and a 0 is a reading. On macOS 27+ PMP comes before `Energy Model`, which freezes and catches up there while PMP doesn't, and a zero no longer hides a positive reading: on M1, M2 and M5 the two never overlap, so no reading changes there.
- SMC `PZD*` is instantaneous, not an energy counter: only a last fallback. Present only on M5 of the three Macs; M1 and M2 have no SMC key that follows memory load alone.
- `PMP` and per-die `PMP0`, `PMP1`, …: mactop merges all of them and sums distinct group/channel pairs, relying on multi-die chips leaving the bare `PMP` group with zero channels (mactop 6585fe5: "the bare PMP group exists with zero channels" on Ultra; 53e5f90 for M5 Max). macmon takes the `PMP` group's CPU channels when it has any, else the per-die ones, so no chip can count both.
- PMP CPU histograms exist only on M5 of the three Macs. Their bins are 0.25–1 W wide: under CPU and memory load 4–6% above CLPC, at idle or light load 0.4–1.5 W high. They add channels that cost about as much as CLPC per sample (~3–7 ms on M5), hence the macOS 27+ without CLPC condition; on macOS 26 and older `Energy Model` works.
- `PDTR` and `PD0R` only without a battery: on a laptop `PD0R` includes charging, and `PDTR` (DC in) differs from `PSTR` on the M5 laptop. No desktop was available: Mac Studio behavior is from mactop#96/#98, and a desktop run is still open.
- #118 landed first and reads `PDTR` as `dc_in_power`; `sys_power` reads it again for its own fallback, two plain SMC reads without shared code.
- Temperature keys: under `macmon stress cpu`, `Ts0P` and `Ts1P` stay flat on M1 and M5, `Ts1P` and `TsOP` on M2; `Ts0P` follows load on M2 but is one of 47 keys. Leaving out every `Ts*P` key holds on all three. M1's `Ts5*` keys read a flat 0, 8 or 10 °C, hence the 10 °C floor, which also covers a GPU sensor reading ~2 °C after boot (mactop#70).
- SMC power keys are read as `flt`: a `PZD*` key of another type fails the read and voids the DRAM sum. `PZC0` / `PZC1` are not CPU power and stay unused.
- The ported pieces live in `src/fallback.rs`, whose module comment credits metaspartan/mactop (MIT); `metrics.rs` only calls them. The PR body thanks mactop#98 for the M5 Max/Ultra data.

## Checklist

- [x] SMC `PZD*` as the last `ram_power` fallback; SMC power keys in `macmon debug`.
- [x] Per-source fallback order with the system-total check.
- [x] `PSTR` → `PDTR` → `PD0R` without a battery.
- [x] PMP CPU histograms as the last `cpu_power` fallback, subscribed only when needed; in `macmon debug`.
- [x] `Ts*` per key idle and under `macmon stress cpu` on M1, M2 and M5; then the key rule, the 10 °C floor and skipping unreadable keys.
- [x] `make check`, `make test`, `make loc REF=origin/main`; own cost of the PMP channels.
- [x] On M5 / macOS 27.2, M2 / macOS 27.0.1 and M1 / macOS 15.8.1: `macmon debug`, `macmon pipe -s 5 -i 1000` idle, under `macmon stress cpu`, `stress gpu` and a memory load; each fallback forced in a local build (CLPC off, `Energy Model` off, no battery, no `PSTR`) against the primary source; temperatures per key.
- [ ] A Mac Studio or Mac mini owner's `macmon debug` and `macmon pipe` (desktop `sys_power`).
