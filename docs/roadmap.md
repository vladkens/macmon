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
- [ ] **3. Open files and sockets of a process.** Enter on a process opens its list, as Activity
      Monitor's "Open Files and Ports" and Sloth show it: working directory, files and sockets
      (`local → remote` address), read with `proc_pidfdinfo` as `lsof` does. The list has a filter;
      Enter on a file reveals it in Finder (`open -R`), Esc goes back to the processes.
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
      them earn a place in an 80-column window.
- [ ] **6. Column picker.** Only if item 5 leaves too many columns: an overlay with checkboxes
      (like htop's F2) saved in `~/.config/macmon.json` under a new field. Released versions
      ignore the field but drop it when they save, so a missing field means the default columns.

Later, only on request: automatic grouping of an app's processes (filter terms with totals
cover the known-app case), per-process network (only `nettop` has it without root), history
graphs per process, Activity Monitor's Memory and Statistics tabs (context switches, faults,
Mach messages and system calls, virtual and shared/private memory), Mach port counts and recent
hangs (need a task port or private API).
