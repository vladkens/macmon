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
      Plan (first PR): [docs/plans/completed/20261011-grouped-files.md](plans/completed/20261011-grouped-files.md).
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
- [x] **10. Pause and filter off.** A key that freezes the process list and the info overlay, so
      a row can be read and selected without the list jumping (Sloth has it). The metrics keep
      updating; the title shows that the list is paused. And `f` turning the filter off and on
      again without retyping it, to look outside it for a moment: the filter text stays, faint,
      while it is off.
      Plan: [docs/plans/completed/20261017-pause-and-filter.md](plans/completed/20261017-pause-and-filter.md).
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
- [x] **14. `serve` as a LaunchDaemon.** For the next minor release. `serve --install` created a
      LaunchAgent, which starts only after login; an always-on Mac scraped by Prometheus needs
      it at boot with nobody logged in (asked on Reddit). `sudo macmon serve --install` now
      installs a LaunchDaemon in `/Library/LaunchDaemons`, replacing the user's agent, and
      `--install --agent` is the old LaunchAgent, without sudo. Tested by hand in the VM: a
      daemon running `macmon serve` starts at boot without a login, as root and with `UserName`;
      with `UserName` the `StandardErrorPath` file must be writable by that user.
- [ ] **15. Disk and network metrics.** What node exporter has and macmon doesn't (asked on
      Reddit): disk read / write and network in / out, system-wide, in the TUI, JSON and
      Prometheus. Decision point before the plan: which of them, and where they fit in the TUI.
- [x] **16. App totals with their helpers.** Asked on Reddit: a filter term totals only the
      processes whose name matches, so Safari's `com.apple.WebKit.WebContent` tabs are left out.
      `app:safari` in the filter matches the processes of an app: those running from its `.app`
      bundle or with such a responsible process, so the total covers the whole app.
- [x] **17. Memory pressure.** Asked on Reddit: RAM use on a Mac is nearly always 70%+ and says
      little; memory pressure, as Activity Monitor shows it, tells whether the Mac is short of
      memory. Read it without root (sysctl `kern.memorystatus_level` and
      `kern.memorystatus_vm_pressure_level`), plot the percent in the RAM box's graph and gauge
      instead of RAM use, colored by the level, keep the box's title and border as they were and
      add both to JSON and Prometheus.
- [ ] **18. Where a process works.** With agents building and testing in several projects at
      once, the busiest process is often just `rustc`, `cargo` or `node`, and the name doesn't
      say which project it belongs to. Show the working directory in the list (a column, or the
      NAME cell for such tools, `~` for home, cut from the left), so the project is visible while
      scrolling, without opening `i` (asked on Reddit too: which project in Claude or ChatGPT
      uses what). Other users' processes show no directory (libproc needs root). Decision point
      before the plan: a column, a second line, or a toggle.
- [ ] **19. Who is busy lately.** CPU% is one sample and jumps; TIME covers the whole life of a
      process. To find what loads the Mac right now (a runaway process, an agent's test run, a
      build), show CPU used over a recent window, e.g. the last 15 s or 1 min, from the CPU time
      already sampled, and sort by it. Decision point before the plan: a new column or a switch
      of TIME between "total" and "recent", and the window length.
- [ ] **20. Processes in `pipe` and JSON.** The process list exists only in the TUI, so a script
      or an LLM agent can't read it (asked on Reddit, as `top` flags for an alias; copying those
      flags isn't the point). Decision point before the plan: a new subcommand or a field of
      `pipe` and `serve`, and what sampling processes costs there.
- [x] **21. Supported macOS versions.** Asked on Reddit, and the readme didn't say: macOS 14,
      current when macmon first came out, and later.
- [x] **22. Filter queries.** Filter the process list by more than the name, with words like
      `mem:>1g` and `user:root`, as fsearch does.
      Plan: [docs/plans/completed/20261106-filter-query.md](plans/completed/20261106-filter-query.md).
- [x] **23. Process command line.** `node` and `python` differ only by their arguments: `cmd:`
      in the filter and a `cmd` row in the `i` overlay, from `sysctl` `KERN_PROCARGS2`, read only
      while a filter uses it. Own processes only: other users' need a `ps` run per refresh, about
      7 times the cost of reading the own ones.
- [ ] **24. A cheaper process list.** macmon's own cost with the process list is 1.7 to 2 s of
      CPU a minute at 1 s, against 0.3 for htop and 1 for btop, and 0.3 without the list. Most of
      it is `ps`, which gives other users' processes; look for a cheaper way.

Order for 0.11: 5 (decide which columns fit 80 cells; 6 only if too many), the second PR of 7,
the third PR of 12. Items 11 and 4 need their decision first. Items 12 and 13 are small and
self-contained, good for an outside contributor.

Later, only on request: per-process network (only `nettop` has it without root), history
graphs per process, Activity Monitor's Memory and Statistics tabs (context switches, faults,
Mach messages and system calls, virtual and shared/private memory), Mach port counts and recent
hangs (need a task port or private API).

Cleanup, no hurry: the Mach calls (`mach_host_self`, `mach_task_self`) come from `libc`, which
deprecates them in favor of the `mach2` crate, so they sit under `#[allow(deprecated)]`
(JohnTitor/mach2#34). Declare them with our own `extern "C"`, or move to `mach2` once it has
what we need.
