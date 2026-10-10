# Roadmap

Work on one item at a time, each through its own PR. When an item is taken, its plan goes to
`docs/plans/yyyymmdd-<name>.md` and is linked here. Tick an item in the PR that finishes it.

Process data beyond CPU% and memory comes from libproc, which reads only the user's own
processes without root (all of them under sudo). Other users' processes show `-` there, as
POWER does today.

- [ ] **1. Kill the selected process** (#84). `k` → `Kill <pid> <name>? y/n` → SIGTERM; the exit
      is tracked on every tick; after 2 s `still running · k force kill` → another y/n →
      SIGKILL. Never pid ≤ 0, launchd or macmon itself; the start time is checked before every
      signal. Plan: [docs/plans/20261010-kill-process.md](plans/20261010-kill-process.md).
- [ ] **2. Show in Finder.** A key on the selected process runs `open -R` on its executable.
      Works for every process (the path is readable for all users).
- [ ] **3. More process columns, and a column picker.** New columns: CPU TIME (total, all
      processes: already sampled from `rusage` and `ps`), THREADS (`proc_taskinfo`), IDLE WAKE/s
      (`rusage` `ri_pkg_idle_wkups`), FDS (open file descriptors), DISK R/W/s (`rusage` disk
      bytes), PEAK MEM (`ri_lifetime_max_phys_footprint`). A small overlay with checkboxes picks
      the columns, saved in `~/.config/macmon.json` under a new field; released versions ignore
      it but drop it when they save, so a missing field means the default columns.
- [ ] **4. Process details.** Enter on a process opens a view like Activity Monitor's inspector:
      path, parent (Enter jumps to it), user, CPU; then three tabs. Memory: footprint, peak,
      resident, virtual. Statistics: threads, CPU time, context switches, faults, page ins, Mach
      messages, Mach and Unix system calls (all from `proc_taskinfo`), idle wake ups. Open files
      and ports: working directory, files and sockets (`proc_pidfdinfo`, as `lsof` reads them)
      with a filter; Enter on a file reveals it in Finder (`open -R`).
- [ ] **5. A C library for other UIs.** Ship macmon as a dylib with a C header, so Swift, Python
      or C apps sample metrics without parsing `macmon pipe`. Builds on #59 and
      [homm/macmon-bindings](https://github.com/homm/macmon-bindings) (Python and Swift today,
      published from a fork). Decision point before the plan: a C API of plain structs, as in
      #59, or a small one that returns the `pipe` JSON (sampler new / next / free, SoC info,
      last error), which keeps the ABI stable while metrics change. Release packaging: the dylib,
      the header and an `XCFramework`; bindings stay outside this repository.

Later, only on request: automatic grouping of an app's processes (filter terms with totals
cover the known-app case), per-process network (only `nettop` has it without root), history
graphs per process, Mach port counts and recent hangs (need a task port or private API), shared
and private memory sizes.
