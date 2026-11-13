Done: 2026-11-11, PR #118

# Chip power

Finish homm's PR #117 (CLPC chip aggregate, DRAM, DCS and media counters) and then #118 (SMC power readings), so macmon reports the whole chip on macOS 27 without breaking existing JSON, Prometheus or library users.

## Behavior

- `chip_power`: the chip total in Watts: the CLPC package total, at least CPU + GPU + ANE + DRAM + DCS + media. DRAM, DCS and the package total above a positive SMC total are dropped first (#125), so a spike reaches neither `chip_power` nor `sys_power`. JSON `chip_power`, Prometheus `macmon_chip_power_watts`; the TUI's "Power" box shows it when there is no system total.
- `all_power`: deprecated, the same value as `chip_power` in Rust (`#[deprecated]`), JSON and Prometheus.
- `ram_power`: DRAM only: CLPC, else `Energy Model`, else PMP (M1, #119).
- `dcs_power`: new, `Option<f32>`: CLPC, else `Energy Model`, else PMP; `null` and no Prometheus gauge without a source, also with `Sampler::with_clpc()`.
- `media_power`: new, `Option<f32>`: CLPC AVE + ISP, which only the M6 driver has (M2 and M5 checked at runtime, M6 untested); `null` and no gauge elsewhere, where media is inside `chip_power`.
- `gpu_ram_power`: deprecated, read as before from `Energy Model` GPU SRAM.
- `sys_power`: the SMC total, at least `chip_power`.
- `dc_in_power` (#118): new, `Option<f32>` from SMC `PDTR`, `null` without the key.
- `display_power` (#118): new, `Option<f32>` from SMC `PBwo`, else `PDBR`; `null` without a reading.
- The raw sources of every new field (CLPC and `Energy Model` channels, SMC keys) appear in `macmon debug`.

## Decisions

- The maintainer's review of #117 and #118 (vladkens/macmon#117, comment 6671119044) sets the field split above: DRAM stays apart from DCS, `chip_power` replaces `all_power`, new fields are optional.
- `battery_power` from #118 is dropped: it reads wrong on AC on M1, M2 and M5.
- #118's change of `all_power` to include memory is dropped: `chip_power` covers it.
- No VDEC report exists in any driver; media IDs live in a separate table, not in every driver's keys.
- Updates to homm's branch are merges from `main`, never a rebase or force push.

## Checklist

- [x] #117: merge `main` (#119 PMP DRAM on M1, #125 RAM check) into the branch.
- [x] #117: `ram_power` DRAM only, `dcs_power` and `media_power` optional.
- [x] #117: `chip_power` with deprecated `all_power` in Rust, JSON and Prometheus.
- [x] #117: `gpu_ram_power` as on `main`, deprecated.
- [x] #117: readme power section and JSON example.
- [x] #117: `make check`, `make test`.
- [x] #117: `macmon pipe` and `macmon debug` idle and under CPU and GPU load on M5 / macOS 27.2 and M2 / macOS 27.0.1.
- [x] #117: the same on M1 / macOS 15.8.1 (#119's PMP DRAM path).
- [x] #118: `dc_in_power` and `display_power` optional, `battery_power` removed, `all_power` untouched.
- [x] #118: `make check`, `make test`, `macmon pipe` and `macmon debug` on M1, M2 and M5, on AC and on battery.

## Result

Shipped in #117: `chip_power` with deprecated `all_power`, DRAM-only `ram_power`, optional `dcs_power` and `media_power`, deprecated `gpu_ram_power`, in Rust, JSON and Prometheus. Shipped in #118: optional `dc_in_power` (SMC `PDTR`) and `display_power` (`PBwo`, else `PDBR`). On AC `dc_in_power` exceeds `sys_power` by the battery's charge power while charging (M2: ~6.7 W, M1: ~4.1 W, both matching the battery's amperage × voltage) and stays below the adapter's rating; on M5 with a full battery `PSTR` is `PDTR` one SMC update (~1 s) late. Untested: `display_power` with a real reading (none of M1, M2, M5 has `PBwo` or `PDBR`; `PBLR` reads 0 on M1 and M2 with the display on and off, so it isn't used), `media_power` on M6, M5 on battery, desktops.
