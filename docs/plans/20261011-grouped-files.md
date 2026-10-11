# Grouped files list in the info overlay (roadmap item 7, first PR)

The info overlay (`i`) lists the open files of a process in fd order today. This PR makes the
list readable for processes with hundreds of descriptors: a counts line, `exe` and `cwd` first,
files grouped under their directory, an open mode, duplicates collapsed, pipes and kqueues only
counted. Aim ≤ 200 production lines (`make loc REF=origin/main`), stop and report past it.

## Behavior

```
╭─ Google Chrome Helper (Renderer) 4127 ─ / filter ─────────────────────────────── finder ─╮
│ 116 fds  8 files  3 dev  1 tcp  6 unix  96 pipes  3 kqueues                              │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ exe       /Applications/Google Chrome.app/…/MacOS/Google Chrome Helper (Renderer)        │
│ cwd       /                                                                              │
│           /Applications/Google Chrome.app/…/Versions/131.0.6778.86/Resources/            │
│ file  r     chrome_100_percent.pak                                                       │
│ file  r     icudtl.dat                                                                   │
│ file  r   ~/Library/…/Indexed Rules/37/9.57.0/Ruleset Data  ×2                           │
│ file  rw  ~/Library/…/Default/Shared Dictionary/db                                       │
│ dev   rw  /dev/null  ×3                                                                  │
│ tcp       192.168.1.20:50122 → 140.82.112.4:443 (established)                            │
│ unix      /var/run/mDNSResponder                                                         │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
```

- Counts line, the first line inside the box, then a divider in the border style (`├──┤`):
  `N fds`, then the count of each kind among the descriptors (not exe / cwd), zero kinds left
  out, numbers as text, labels faint: files, dirs, dev, tcp, udp, unix, pipes, kqueues, other.
  With a filter set the first pair reads `shown/total fds` (shown: the descriptors behind the
  listed rows), like `proc 12/312`; the kind counts stay totals. Pairs that don't fit drop from the
  end. Below 8 inner rows the counts line and the divider are hidden. The bottom border keeps only
  the typing hints.
- Rows in this order: `exe` (`ProcInfo.path`, when not empty), `cwd`, vnode descriptors (`dir`,
  `file`, `dev`), then `tcp`, `udp`, `unix`, `other`. Pipes, kqueues and rows without text are
  not listed.
- A type on every row and an open mode after it: `r`, `w`, `rw` (`fi_openflags` FREAD / FWRITE)
  for `dir`, `file`, `dev`; blank for exe, cwd and sockets. Both faint unless selected.
- Rows with the same kind, text and mode collapse into one with a faint `  ×N` after the text.
- Among `file` rows (after the filter), when 2 or more distinct files share a directory, a
  directory row comes first (no type, no mode, the directory with a trailing `/`) and those files
  show only their name, indented 2 cells. A file alone in its directory shows its full path. Vnode
  rows sort by (kind dir / file / dev, directory, name), sockets by text.
- Paths show `$HOME` as `~`. Long paths are cut by components: the first 2 components, `…`, then
  the longest tail of whole components that fits; else 1 head component; else `cut_middle`.
  Socket text keeps `cut_middle`.
- Every row is selectable. The selection follows its key (kind, text, mode, directory row) across
  refreshes and filter changes; when the key is gone, the row at the same index (clamped).
- `e` reveals exe, cwd, a directory row (the directory), `dir` and `file` rows (the full path);
  not `dev`, not sockets.
- `/` keeps the filter rules (comma terms, `!` excludes), matched against the type, the full path
  and its `~` form, before grouping and collapsing; directory rows come from what passes.
- Everything else stays: top border, Esc / q, Not permitted / Already exited, refresh per sample.

## Decisions

- `fds.rs`: `Kind` gains `Exe`, `Dir`, `Dev`; `OpenFile` gains `mode: &'static str`. The vnode
  struct becomes `proc_fileinfo` (hand-written: `fi_openflags`, padding; 24 bytes) followed by
  libc's `vnode_info_path`; `vst_mode & S_IFMT`: `S_IFDIR` → dir, `S_IFCHR` → dev, else file. The
  layout test locks `fi_openflags` at 0, `vst_mode` at 28, `vip_path` at 176, size 1200.
- `Exe` rows come from the overlay (`ProcInfo.path`), not from `open_files`.
- Rows are rebuilt from the files on each use (filter, sort, collapse, group); the home directory
  is a field of `Info` read from `HOME` once, set directly in tests.

## Tasks

### 1. Dir, dev and open mode

- [x] `fds.rs`: the `Dir` and `Dev` kinds (`Exe` comes with its use in task 2), the mode, the vnode struct with libc's `vnode_info_path`, the layout
      test; tests on the test's own process: `/dev/null` is `dev`, a temp dir is `dir`, a file
      opened write-only is `file` `w`.
- [x] `make check` and `make test` pass; commit `feat: dir, dev and open mode of open files`.

### 2. Grouped list

- [ ] `info.rs`: counts line and divider, row order, type and mode columns, `×N`, directory
      groups, `~` and the component cut, selection by key, `e` on the new rows, the filter on the
      `~` form; roadmap item 7 links this plan.
- [ ] Tests: grouping and collapse on a Chrome-like fixture, a single file keeps its full path, a
      filter turning a group into a full-path row, `~`, the component cut at several widths,
      selection by key across a refresh, `e` on a directory row, the counts line with and without
      a filter and hidden at low height, any window size renders without panic.
- [ ] `make check`, `make test`, `make loc REF=origin/main`; commit
      `feat: grouped files list in the info overlay`.
