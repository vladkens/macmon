# Chip power

Finish homm's PR #117 (CLPC chip aggregate, DRAM, DCS and media counters) and then #118 (SMC power readings), so macmon reports the whole chip on macOS 27 without breaking existing JSON, Prometheus or library users.

## Behavior

- `chip_power`: the chip total in Watts. The CLPC package aggregate when it is valid, unless the sum CPU + GPU + ANE + DRAM + DCS + media from the available sources is higher: a total below its parts is a bad reading. The sum uses `ram_power` after the #125 check, so a DRAM spike reaches neither `chip_power` nor `sys_power`. JSON `chip_power`, Prometheus `macmon_chip_power_watts`; the TUI's "Power" box shows it when there is no system total.
- `all_power`: deprecated, the same value as `chip_power`, kept in the Rust struct (`#[deprecated]`), JSON and Prometheus (`macmon_all_power_watts`).
- `ram_power`: DRAM only: CLPC DRAM where the driver is known, else `Energy Model` DRAM, else PMP DRAM (M1, #119). Still dropped to 0 above the SMC system total (#125).
- `dcs_power`: new, `Option<f32>`: CLPC DCS, else `Energy Model` DCS, else PMP DCS (M1); `null` in JSON and no Prometheus gauge without a source.
- `media_power`: new, `Option<f32>`: CLPC AVE + VDEC + ISP where the driver has separate reports (M6 so far, unverified on hardware); `null` and no gauge otherwise.
- `gpu_ram_power`: deprecated, read as before from `Energy Model` GPU SRAM. On macOS 27 the CLPC GPU report already includes GPU SRAM where the firmware adds it.
- `sys_power` is unchanged.
- `dc_in_power` (#118): new, `Option<f32>` from SMC `PDTR`, `null` without the key.
- `display_power` (#118): new, `Option<f32>` from SMC `PBwo`, else `PDBR`; `null` without a reading.
- The raw sources of every new field (CLPC and `Energy Model` channels, SMC keys) appear in `macmon debug`.

## Decisions

- The maintainer's review of #117 and #118 (vladkens/macmon#117, comment 6671119044) sets the field split above: DRAM stays apart from DCS, `chip_power` replaces `all_power`, new fields are optional.
- `battery_power` from #118 is dropped: it reads wrong on AC on M1, M2 and M5.
- #118's change of `all_power` to include memory is dropped: `chip_power` covers it.
- #117's CLPC extraction fixes, scripts and tests stay as they are.
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
- [ ] #118: `dc_in_power` and `display_power` optional, `battery_power` removed, `all_power` untouched.
- [ ] #118: `make check`, `make test`, `macmon pipe` and `macmon debug` on M1, M2 and M5, on AC and on battery.
