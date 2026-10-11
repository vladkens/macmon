# Process info and reveal in Finder (roadmap items 2 and 3)

For the selected process, `i` lists its working directory, open files and sockets in an overlay
over the process list (like Activity Monitor's "Open Files and Ports" and Sloth), and `o` reveals
its executable in Finder. One PR, owner-approved as one feature; aim ≤ 400 production lines
(`make loc`), stop and report past 450.

## Behavior

- The process box title shows `info | open | kill` on the right while a process is selected and
  the filter is not being typed (hint style: key in the default color, rest dim). The right title
  drops as a whole when it doesn't fit. `i` / `o` are guarded by `procs_visible()` like `k`.
- `o` on the process list: `open -R <executable path>`; nothing when the path is empty (inside an
  `.app` this reveals the binary in `Contents/MacOS`).
- `i` opens the info overlay over the whole process box rect (`Clear` + `draw_box`); while it is
  open every key goes to it (Ctrl-C still quits). It closes when the process list goes hidden.
  - Top border: `<name> <pid>`, then the `/ filter` hint, or the filter text (with the cursor
    while typing) as in the process box.
  - Rows: a type column (`cwd`, `file`, `tcp`, `udp`, `unix`, `pipe`, `kqueue`, `other`) and the
    path or address. There is always a selected row (clamped), starting at the first.
  - Sockets: `local → remote` as `std::net::SocketAddr` text (IPv6 bracketed, no DNS), ports
    converted from network byte order; `→ remote` left out when the remote port is 0 (listeners,
    unbound UDP); TCP adds its state (`listen`, `established`, …). Unix sockets show their own
    path, else the peer's.
  - Paths too long for the row are cut in the middle with `…`: the head and the file name stay.
  - Bottom border: the selected row's full path or address, cut from the start when too long;
    hints on its right: `Enter keep | Esc clear | ↑↓ select` while typing, else `open | Esc close`.
  - ↑↓, PgUp/PgDn, Home/End and the wheel move the selection, and end filter input like in the
    process list; `/` filters with the process filter's rules (comma terms, `!` excludes; matched
    against the type and the path); `o` reveals the selected file or the cwd in Finder, sockets,
    pipes and the rest do nothing.
  - Esc: while typing it clears the filter and ends input; with a filter set it clears it;
    otherwise it closes. Focus loss ends filter input.
  - Refreshed on every process sample while open; the selection stays on its fd (the cwd has its
    own key). Pid reuse: the start time is compared with the sampled one (as kill does); a
    different process reads as `Already exited`.
  - When the process can't be read: `Not permitted` (EPERM, e.g. other users' processes without
    root) or `Already exited` (ESRCH), in place of the rows.
- Help: `i  open files and sockets of the selected process`, `o  show the selected process in
  Finder`. Roadmap items 2 and 3 are ticked with the text updated to `i` / `o`.

## Decisions

- libproc as lsof reads it: cwd with `proc_pidinfo(PROC_PIDVNODEPATHINFO)` (call it first; errno
  only after a failed call), descriptors with `proc_pidinfo(PROC_PIDLISTFDS)` (size with a null
  buffer first, `PROC_PIDLISTFD_SIZE` per entry; 0 after a successful cwd call means no fds), each
  with `proc_pidfdinfo` (`PROC_PIDFDVNODEPATHINFO = 2` for vnodes, `PROC_PIDFDSOCKETINFO = 3` for
  sockets). Only the pid-level calls return `Err(errno)`; a failing fd (EBADF when it was closed
  meanwhile) is skipped.
- libc 0.2.190 has `proc_pidinfo`, `proc_pidfdinfo`, `PROC_PIDLISTFDS`, `PROC_PIDLISTFD_SIZE`,
  `proc_fdinfo` (`proc_fdtype` is `u32`, cast `PROX_FDTYPE_*`), `PROC_PIDVNODEPATHINFO`,
  `proc_vnodepathinfo`, `vnode_info_path` (`vip_path` is `[[c_char; 32]; 32]`: read it with
  `as_flattened()`), `vinfo_stat`. The rest is written by hand from the SDK's `sys/proc_info.h`,
  only the fields that are read plus padding arrays; none of them is packed, `socket_info` is
  8-aligned. Sizes and offsets from clang on this SDK, each locked by a test like
  `rusage_layout_matches_sdk`: `proc_fileinfo` 24; `vnode_fdinfowithpath` 1200 (path at 176);
  `socket_fdinfo` 792 (`psi` at 24); `socket_info` 768 (`soi_family` 160, `soi_kind` 232,
  `soi_proto` 240, the union 528); `in_sockinfo` 80 (`insi_vflag` 24 — bit 1 is IPv4,
  `insi_faddr` 32, `insi_laddr` 48, IPv4 at +12); `tcp_sockinfo` 120 (`tcpsi_state` 80);
  `un_sockinfo` 528 (`unsi_addr` 16, `unsi_caddr` 271; 255-byte unions, the path 2 bytes in).
  The kernel checks a minimum buffer size, so every call also checks it returned
  `size_of::<T>()`.
- Classification: `soi_kind == SOCKINFO_TCP` → `tcp`; `SOCKINFO_IN` with `soi_type == SOCK_DGRAM`
  → `udp`; `SOCKINFO_UN` → `unix`; other sockets → `other`.
- Reading lives in a new `src/app/fds.rs` (app feature, beside procs.rs); the overlay in a new
  `src/app/tui/info.rs`, wired like the kill popup in `src/app/tui/mod.rs`: a field on `App`,
  key and mouse routing right after help (help → info → kill → process list), rendered after the
  process box, closed in `set_procs_visible(false)`, typing ended in `Event::FocusLost`, re-read
  in `update_procs` while open (on the UI thread; per-fd calls take microseconds).
- System access goes through a small trait (list the files of a pid, reveal a path) with the real
  implementation and a test fake, like `KillSys`; `Default` is the fake under `cfg(test)`. One
  test builds the real implementation and lists the test's own pid (so it isn't dead code in the
  test build); no test ever calls reveal for real.
- `open -R` runs `/usr/bin/open` with null stdio, spawned and reaped on a thread, so the UI never
  blocks and no zombie stays.
- Reuse from proc_view.rs instead of copying: `Filter` with `matches` taking a matcher closure;
  the filter input keys and `filter_title` as free functions over `(&mut String, &mut bool, key)`
  / `(filter, typing, room)`; `scroll_offset`, `cut_start`, `head`/`tail`; add `cut_middle`.

## Tasks

### 1. Read open files and sockets

- [x] `src/app/fds.rs`: the lean hand-written structs and constants with size and offset tests,
      and `fn open_files(pid) -> Result<Vec<OpenFile>, i32>` (cwd first, then fds in order) where
      `OpenFile` has the fd (none for cwd), the kind and the text. Declare it with
      `#[cfg_attr(not(test), expect(dead_code))] mod fds;` in src/app/main.rs until task 2 uses
      it.
- [x] Tests on the test's own process only, checking that their own entries are in the list (other
      tests run in parallel threads): the cwd equals `std::env::current_dir()`; a temp file it
      opens is listed with `fs::canonicalize(path)`; a TCP listener on 127.0.0.1:0 shows as `tcp`
      with that port and `listen`; a UDP socket shows as `udp`. pid 1 gives EPERM (skip when
      `geteuid() == 0`); `i32::MAX` gives ESRCH.
- [x] `make check` and `make test` pass; commit `feat: read open files and sockets of a process`.

### 2. Info overlay and reveal in Finder

- [ ] Remove the `expect(dead_code)` from task 1. Title actions `info | open | kill`, `o` reveal,
      the info overlay with every behavior above, the footer and help lines, the roadmap ticks.
- [ ] Tests with the fake: actions on the title only with a selection and not while typing (update
      `kill_is_on_the_process_title_with_a_selection_while_not_typing` for the longer title); `o`
      reveals the selected process's path and nothing for an empty path; `i` opens the overlay
      with fake rows; navigation and the wheel, the filter and its hints, `o` on a file and on a
      socket, the Esc order, focus loss; refresh keeps the selection on its fd; a reused pid,
      `Not permitted` and `Already exited`; middle cut and the bottom-border path at a narrow
      width; any window size renders without panic.
- [ ] `make check`, `make test` and `make loc REF=origin/feat/hint-style` (report the delta);
      commit `feat: process info overlay and reveal in Finder`.

## Manual check (outside the tasks)

Run the TUI with `HOME` set to a temporary directory: `i` on a shell, a browser helper and `ssh`
(sockets), `o` on several processes, `i` on a root process (`Not permitted`).
