# Roadmap

Work on one item at a time, each through its own PR. When an item is taken, its plan goes to
`docs/plans/yyyymmdd-<name>.md` and is linked here. Tick an item in the PR that finishes it.

Process data beyond CPU% and memory comes from libproc, which reads only the user's own
processes without root (all of them under sudo). Other users' processes show `-` there, as
POWER does today.

- [x] **1. Kill the selected process** (#89). `k` → popup: Terminate / Force kill / Cancel; the
      start time is checked before the signal; launchd and macmon are refused.
      Plan: [docs/plans/completed/20261010-kill-process.md](plans/completed/20261010-kill-process.md).
- [x] **2. Show in Finder.** `e` on the selected process runs `open -R` on its executable.
      Works for every process (the path is readable for all users).
      Plan: [docs/plans/completed/20261011-process-info.md](plans/completed/20261011-process-info.md).
- [x] **3. Open files and sockets of a process.** `i` on a process opens its list, as Activity
      Monitor's "Open Files and Ports" and Sloth show it: working directory, files and sockets
      (`local → remote` address), read as `lsof` does: the working directory with
      `proc_pidinfo(PROC_PIDVNODEPATHINFO)`, the descriptors with `PROC_PIDLISTFDS` and each one with
      `proc_pidfdinfo`. The list has a filter; `e` on a file reveals it in Finder (`open -R`), Esc
      goes back to the processes.
      Plan: [docs/plans/completed/20261011-process-info.md](plans/completed/20261011-process-info.md).
- [ ] **4. A C library for other UIs.** Ship macmon as a dylib with a C header, so Swift, Python
      or C apps sample metrics without parsing `macmon pipe`. Builds on #59 and
      [homm/macmon-bindings](https://github.com/homm/macmon-bindings) (Python and Swift today,
      published from a fork). Decision point before the plan: a C API of plain structs, as in
      #59, or a small one that returns the `pipe` JSON (sampler new / next / free, SoC info,
      last error), which keeps the ABI stable while metrics change. Release packaging: the dylib,
      the header and an `XCFramework`; bindings stay outside this repository.
- [ ] **5. More process columns.** Candidates: CPU TIME (total, all processes: already sampled
      from `rusage` and `ps`), THREADS (`proc_taskinfo`), IDLE WAKE/s (`rusage`
      `ri_pkg_idle_wkups`), FDS (open file descriptors), DISK R/W/s (`rusage` disk bytes), PEAK
      MEM (`ri_lifetime_max_phys_footprint`). Fixed set, no configuration: new columns go into
      `DROP_ORDER`, so a narrow window drops them first. Decision point before the plan: which of
      them earn a place in an 80-column window. Done: CPU TIME (`TIME`, before `CPU%`).
      Plan: [docs/plans/completed/20261014-cpu-time.md](plans/completed/20261014-cpu-time.md).
- [ ] **6. Column picker.** Only if item 5 leaves too many columns: an overlay with checkboxes
      (like htop's F2) saved in `~/.config/macmon.json` under a new field. Released versions
      ignore the field but drop it when they save, so a missing field means the default columns.
- [ ] **7. A clearer info overlay.** Two PRs. First, the files list: `exe` and `cwd` first, then
      files sorted by directory, a type and open mode (`r-`, `-w`, `rw`) on every row, duplicates
      as `×N`, devices, pipes, kqueues and unnamed sockets left out of the list and counted in a
      line above it (`116 fds  8 files  3 dev  96 pipes …`). Second, a process summary line
      above the counts (cpu, mem, power, threads, uptime, parent) and presets on `s` / `S` (`all`,
      `files`, `network`, `home`, `not home`) saved in `~/.config/macmon.json`.
      Plan (first PR): [docs/plans/20261011-grouped-files.md](plans/20261011-grouped-files.md).
- [x] **8. macmon's own cost, before the release.** Measure what the TUI, `pipe` and `serve` cost
      at 1000 ms and 250 ms: CPU time per minute and power (macmon's own POWER, `powermetrics`
      task energy, Activity Monitor's Energy Impact), with the process list shown and hidden.
      Profile the hot paths (sampling, IORegistry GPU reads, `ps`, rendering) and fix what is
      cheap to fix; report before / after numbers in the PR.
      Plan: [docs/plans/completed/20261014-own-cost.md](plans/completed/20261014-own-cost.md).
- [x] **9. Scrolling in the lists.** A scrollbar on the right border of the process list and the
      info overlay (ratatui's `Scrollbar`: `↑` / `↓` at the ends, a thumb between, as btop
      draws it), ↑ / ↓ wrapping around at the ends (not the wheel, PgUp / PgDn, Home / End), and
      `c` copying the selected path (the executable in the list, the row's full text in the info
      overlay); `? | q back` on the overlay's bottom border.
      Plan: [docs/plans/completed/20261012-list-scroll.md](plans/completed/20261012-list-scroll.md).
- [ ] **10. Pause and filter off.** A key that freezes the process list and the info overlay, so
      a row can be read and selected without the list jumping (Sloth has it). The metrics keep
      updating; the title shows that the list is paused. And `f` turning the filter off and on
      again without retyping it, to look outside it for a moment: the filter text stays, faint,
      while it is off.
- [ ] **11. Who has a file open.** A view of the open files of all processes with the filter,
      to find who reads or writes a file or holds a port (Sloth's main window, `lsof <path>`).
      Without root libproc reads only the user's own processes; other users' processes are
      counted as not readable. Decision point before the plan: a separate tab or a mode of the
      process list.
- [ ] **12. Run in a VM.** macmon in a macOS VM (Apple Virtualization, UTM), where IOReport,
      the SMC and the frequency tables are missing. Three PRs: start without IOReport hardware
      counters, with CPU usage from `host_processor_info`; hide unavailable power, with
      a VM flag in the SoC info and the Prometheus gauges left out; GPU usage from the
      IOAccelerator client time.
      Plan (first PR): [docs/plans/completed/20261012-vm-start.md](plans/completed/20261012-vm-start.md).
      Plan (second PR): [docs/plans/completed/20261012-vm-power.md](plans/completed/20261012-vm-power.md).
- [ ] **13. Social preview card.** The repository still shows GitHub's template image. A
      1280×640 card from a real TUI screenshot of the current release (HTML rendered to PNG),
      uploaded by hand in the repository settings.
- [ ] **14. `serve` as a LaunchDaemon.** For the next minor release. `serve --install` creates a
      LaunchAgent, which starts only after login; an always-on Mac scraped by Prometheus needs it
      at boot with nobody logged in (asked on Reddit). An install option for a LaunchDaemon in
      `/Library/LaunchDaemons` (needs sudo). Tested by hand in the VM: a daemon running `macmon
      serve` starts at boot without a login, as root and with `UserName`; with `UserName` the
      `StandardErrorPath` file must be writable by that user.
- [ ] **15. Disk and network metrics.** What node exporter has and macmon doesn't (asked on
      Reddit): disk read / write and network in / out, system-wide, in the TUI, JSON and
      Prometheus. Decision point before the plan: which of them, and where they fit in the TUI.
- [ ] **16. App totals with their helpers.** Asked on Reddit: a filter term totals only the
      processes whose name matches, so Safari's `com.apple.WebKit.WebContent` tabs are left out.
      Group an app's helper processes under it, expandable to show which ones are counted. Until
      then, the total row could say it covers matching processes only.

Order for 0.11: 10, then 5 (decide which columns fit 80 cells; 6 only if too many), the second
PR of 7, the third PR of 12. Items 11 and 4 need their decision first. Items 10, 12 and 13 are
small and self-contained, good for an outside contributor.

Later, only on request: automatic grouping of an app's processes (filter terms with totals
cover the known-app case), per-process network (only `nettop` has it without root), history
graphs per process, Activity Monitor's Memory and Statistics tabs (context switches, faults,
Mach messages and system calls, virtual and shared/private memory), Mach port counts and recent
hangs (need a task port or private API).

Cleanup, no hurry: the Mach calls (`mach_host_self`, `mach_task_self`) come from `libc`, which
deprecates them in favor of the `mach2` crate, so they sit under `#[allow(deprecated)]`
(JohnTitor/mach2#34). Declare them with our own `extern "C"`, or move to `mach2` once it has
what we need.
