# Fallback sources from mactop

Read the power and temperature sources that [mactop](https://github.com/metaspartan/mactop) (MIT) uses where macmon reads 0 or a wrong value today, as last fallbacks after the sources macmon already has. Four separate PRs, in this order, each on its own branch.

## Behavior

- PR 1, `ram_power`: CLPC, else `Energy Model` or PMP when they read more than 0, else the sum of SMC `PZD1`, `PZD2`, … (one key per die). `macmon debug` shows `PSTR`, `PDTR`, `PD0R` and every `PZD*` key.
- PR 2, `sys_power`: the first of SMC `PSTR`, `PDTR`, `PD0R` that reads more than 0, still at least `chip_power`. `PDTR` and `PD0R` only on Macs without a battery: on a laptop `PD0R` includes charging, so laptops keep `PSTR` alone. Fixes `sys_power` = `chip_power` on Mac Studio, where `PSTR` and `PDTR` read 0.
- PR 3, `cpu_power`: CLPC, else `Energy Model` when it reads more than 0, else the PMP `Energy` histograms of the CPU clusters (`EACC0`, `PACC0`, `MACCn` and their ` SRAM` channels, in the `PMP` group and the per-die `PMP0`, `PMP1`, …). Each channel's power is the residency-weighted average of its state bins (`0.250W`, `1W`, …), the lowest bin counted as 0. Today CPU reads 0 when the CLPC IDs are unknown and `Energy Model` is frozen.
- PR 4, `cpu_temp_avg`: SMC keys that don't follow CPU load (`Ts0P`, `Ts1P` on M5) are left out, die readings below 10 °C are invalid (a GPU sensor reads ~2 °C for a second after boot, mactop#70), and an unreadable SMC key is skipped instead of failing the whole sample.

No JSON, Prometheus, library API or config change, and no new dependency: existing fields only get a value where they read 0 or wrong.

## Decisions

- Every fallback is per sample: CLPC wins when it has the counter (also at an idle 0); `Energy Model` and PMP only when they read more than 0, since a frozen `Energy Model` reads 0 on macOS 27; the new source last. mactop instead switches to its fallback for the rest of the process on the first such sample; a latch is added only if per-sample values are seen jumping between the two sources on hardware.
- SMC `PZD*` is an instantaneous reading, not an energy counter: fine as a last fallback, never preferred over CLPC. The `PSTR` check of `with_power_totals` still drops a reading above the system total.
- Only `flt` SMC keys are read, as today; mactop's other number types and its 1000 W cap are taken only if a Mac shows the need.
- `PZC0` / `PZC1` are not CPU power (they don't follow CLPC CPU under load on M5): not used.
- PR 2 doesn't add `dc_in_power` (planned for #118 in `20261111-chip-power.md`); if #118 lands first, PR 2 reads `PDTR` through it.
- PR 4 changes the key set only after `macmon debug` of `Ts*` idle and under `macmon stress cpu` on M1, M2 and M5: on M1–M4 `Ts*` may be other sensors, so the rule must hold on all three.
- Ported logic carries a short comment crediting metaspartan/mactop (MIT); the PR bodies thank mactop#98 for the M5 Max/Ultra data.

## Checklist

- [x] PR 1: SMC `PZD*` as the last `ram_power` fallback; `PSTR`, `PDTR`, `PD0R`, `PZD*` in `macmon debug`.
- [x] PR 1: `make check`, `make test`, `make loc REF=origin/main`.
- [x] PR 1: on M5 / macOS 27.2, M2 / macOS 27.0.1 and M1 / macOS 15.8.1: `macmon debug` (which Macs have `PZD*`), `macmon pipe` idle, under a memory-streaming load and under `macmon stress cpu`, SMC `PZD*` next to CLPC (or `Energy Model` / PMP) DRAM; the fallback path with CLPC DRAM disabled in a local build.
- [ ] PR 2: `PSTR` → `PDTR` → `PD0R` without a battery.
- [ ] PR 2: `make check`, `make test`, `make loc`; `macmon pipe` on the M1, M2 and M5 laptops on AC and on battery shows the same `sys_power` as before. No desktop available: ask a Mac Studio / Mac mini owner for `macmon debug` and `macmon pipe`, name it untested otherwise.
- [ ] PR 3: PMP `Energy` CPU histograms as the last `cpu_power` fallback, per-die PMP groups.
- [ ] PR 3: `make check`, `make test`, `make loc`, `make bench` (own cost of the extra PMP channels).
- [ ] PR 3: on M2 and M5 with the CLPC table off in a local build, `macmon pipe` idle and under `macmon stress cpu` against CLPC and `sudo powermetrics --samplers cpu_power` (run by the person); M1 / macOS 15 unchanged.
- [ ] PR 4: `macmon debug` `Ts*` idle and under `macmon stress cpu` on M1, M2 and M5 first; then the key rule, the 10 °C floor and skipping unreadable keys.
- [ ] PR 4: `make check`, `make test`, `make loc`; `macmon pipe` idle and under `macmon stress cpu` on M1, M2 and M5, `cpu_temp_avg` against the per-key values.
