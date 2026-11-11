# Roadmap

The maintainer's backlog of ideas: features, promotion and suggestions. Agents change it only when asked.

Process data beyond CPU% and memory comes from libproc, which reads only the user's own
processes without root (all of them under sudo). Other users' processes show `-` there, as
POWER does today.

- [ ] **A C library for other UIs.** Ship macmon as a dylib with a C header, so Swift, Python
      or C apps sample metrics without parsing `macmon pipe`. Builds on #59 and
      [homm/macmon-bindings](https://github.com/homm/macmon-bindings) (Python and Swift today,
      published from a fork). Decision point before the plan: a C API of plain structs, as in
      #59, or a small one that returns the `pipe` JSON (sampler new / next / free, SoC info,
      last error), which keeps the ABI stable while metrics change. Release packaging: the dylib,
      the header and an `XCFramework`; bindings stay outside this repository.
- [ ] **More process columns.** Candidates: CPU TIME (total, all processes: already sampled
      from `rusage` and `ps`), THREADS (`proc_taskinfo`), IDLE WAKE/s (`rusage`
      `ri_pkg_idle_wkups`), FDS (open file descriptors), DISK R/W/s (`rusage` disk bytes), PEAK
      MEM (`ri_lifetime_max_phys_footprint`). Fixed set, no configuration: new columns go into
      `DROP_ORDER`, so a narrow window drops them first. Decision point before the plan: which of
      them earn a place in an 80-column window. Done: CPU TIME (`TIME`, before `CPU%`).
      Plan: [docs/plans/completed/20261014-cpu-time.md](plans/completed/20261014-cpu-time.md).
- [ ] **Column picker.** Only if the More process columns item leaves too many columns: an overlay with checkboxes
      (like htop's F2) saved in `~/.config/macmon.json` under a new field. Released versions
      ignore the field but drop it when they save, so a missing field means the default columns.
- [ ] **A clearer info overlay.** Two PRs. First, the files list: `exe` and `cwd` first, then
      files sorted by directory, a type and open mode (`r-`, `-w`, `rw`) on every row, duplicates
      as `×N`, devices, pipes, kqueues and unnamed sockets left out of the list and counted in a
      line above it (`116 fds  8 files  3 dev  96 pipes …`). Second, a process summary line
      above the counts (cpu, mem, power, threads, uptime, parent) and presets on `s` / `S` (`all`,
      `files`, `network`, `home`, `not home`) saved in `~/.config/macmon.json`.
      Plan (first PR): [docs/plans/completed/20261011-grouped-files.md](plans/completed/20261011-grouped-files.md).
- [ ] **Who has a file open.** A view of the open files of all processes with the filter,
      to find who reads or writes a file or holds a port (Sloth's main window, `lsof <path>`).
      Without root libproc reads only the user's own processes; other users' processes are
      counted as not readable. Decision point before the plan: a separate tab or a mode of the
      process list.
- [ ] **Run in a VM.** macmon in a macOS VM (Apple Virtualization, UTM), where IOReport,
      the SMC and the frequency tables are missing. Three PRs: start without IOReport hardware
      counters, with CPU usage from `host_processor_info`; hide unavailable power, with
      a VM flag in the SoC info and the Prometheus gauges left out; GPU usage from the
      IOAccelerator client time.
      Plan (first PR): [docs/plans/completed/20261012-vm-start.md](plans/completed/20261012-vm-start.md).
      Plan (second PR): [docs/plans/completed/20261012-vm-power.md](plans/completed/20261012-vm-power.md).
- [ ] **Social preview card.** The repository still shows GitHub's template image. A
      1280×640 card from a real TUI screenshot of the current release (HTML rendered to PNG),
      uploaded by hand in the repository settings.
- [ ] **Disk and network metrics.** What node exporter has and macmon doesn't (asked on
      Reddit): disk read / write and network in / out, system-wide. Done as data only, with uptime
      and battery: JSON, Prometheus and `macmon debug`. Open: showing them in the TUI.
- [ ] **Where a process works.** With agents building and testing in several projects at
      once, the busiest process is often just `rustc`, `cargo` or `node`, and the name doesn't
      say which project it belongs to. Show the working directory in the list (a column, or the
      NAME cell for such tools, `~` for home, cut from the left), so the project is visible while
      scrolling, without opening `i` (asked on Reddit too: which project in Claude or ChatGPT
      uses what). Other users' processes show no directory (libproc needs root). Decision point
      before the plan: a column, a second line, or a toggle.
- [ ] **Who is busy lately.** CPU% is one sample and jumps; TIME covers the whole life of a
      process. To find what loads the Mac right now (a runaway process, an agent's test run, a
      build), show CPU used over a recent window, e.g. the last 15 s or 1 min, from the CPU time
      already sampled, and sort by it. Decision point before the plan: a new column or a switch
      of TIME between "total" and "recent", and the window length.
- [ ] **Processes in `pipe` and JSON.** The process list exists only in the TUI, so a script
      or an LLM agent can't read it (asked on Reddit, as `top` flags for an alias; copying those
      flags isn't the point). Decision point before the plan: a new subcommand or a field of
      `pipe` and `serve`, and what sampling processes costs there.
- [x] **Filter queries.** Filter the process list by more than the name, with words like
      `mem:>1g` and `user:root`, as fsearch does. `port:3000` finds the process on a local port.
      Plan: [docs/plans/completed/20261106-filter-query.md](plans/completed/20261106-filter-query.md).
- [x] **Process command line.** `node` and `python` differ only by their arguments: `cmd:`
      in the filter, a `cmd` row in the `i` overlay and the arguments after the name in the list,
      from `sysctl` `KERN_PROCARGS2`, read only for these. Own processes only: other users' need
      a `ps` run per refresh, about 7 times the cost of reading the own ones.
- [x] **A cheaper process list.** CPU a minute with the list at 1 s, `ps` included: 1.5–1.8 s
      → 0.6–0.7 on an M5, 3.2–3.4 → 1.8–2.0 on an M2, against 0.3 for htop and 1 for btop, which
      read 0 for other users' processes. `ps` prints `ucomm` (`comm` reads every process'
      arguments) and runs about every 3 s, at least 3 s apart as it runs on a tick, so other
      users' rows are that old and their new processes that late, and an own process exec-ing a
      setuid binary is gone until the next run; the executable path and fixed fields are read once
      per exec.
      No gain, left out: a GPU client's creator cached by registry entry, idle processes read
      less often. Most of what's left is `proc_pid_rusage` and the GPU walk, every tick.

Later, only on request: per-process network (only `nettop` has it without root), history
graphs per process, Activity Monitor's Memory and Statistics tabs (context switches, faults,
Mach messages and system calls, virtual and shared/private memory), Mach port counts and recent
hangs (need a task port or private API).

Cleanup, no hurry: the Mach calls (`mach_host_self`, `mach_task_self`) come from `libc`, which
deprecates them in favor of the `mach2` crate, so they sit under `#[allow(deprecated)]`
(JohnTitor/mach2#34). Declare them with our own `extern "C"`, or move to `mach2` once it has
what we need.
