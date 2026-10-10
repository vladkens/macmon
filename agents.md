# macmon

Rootless Apple Silicon monitor and Rust library. Shared sampling and metric types live in `src/metrics.rs`, macOS API access in `src/sources.rs`, and CLI commands in `src/app/`.

The TUI is in `src/app/tui/`. Per-process usage (`src/app/procs.rs`) comes from libproc for the user's own processes, the setuid `/bin/ps` for other users' processes, and GPU time from the IORegistry (`IOAccelerator` children).

## Development

- Keep CLI-only code and dependencies behind the `app` feature; preserve library builds with `--no-default-features`.
- Keep normal metric collection rootless and preserve fallbacks for older systems.
- Add focused tests for changed behavior. `make prepare` applies formatting and fixes; do not use it as a read-only check.
- TUI colors only through `tui/theme.rs` (terminal ANSI colors, no RGB), so macmon follows the terminal's theme.
- Keep `~/.config/macmon.json` compatible with released versions: same field names and values (`view_type` is `"Sparkline"` / `"Gauge"`).
- Release builds abort on panic: every terminal mode the TUI enables must be undone in `restore_term_once`.
- The TUI saves settings on key presses: for manual runs set `HOME` to a temporary directory.

## Metric sources

- Before investigating or changing power sources, read [Power sources and macOS 27](readme.md#power-sources-and-macos-27) and the relevant evidence in [docs/clpc-discovery.md](docs/clpc-discovery.md).
- Check counter availability before treating zero CPU/ANE readings as zero consumption.
- Verify CLPC report IDs, CPU/GPU/ANE assignments, and units against an independent reference under separate component loads. Table positions and response to load alone do not establish a mapping.
- For sampling changes, validate rootless readings with `macmon pipe` on the affected hardware and macOS version. Report runtime results separately from build and unit-test results, and identify untested configurations.

## Workflow

- Before each commit, run `make check` (it also checks the library alone, with `--no-default-features`) and `make test`.
- Keep planned work in [docs/roadmap.md](docs/roadmap.md), not in GitHub issues; link each item's plan there and tick the item in the PR that finishes it.
- Only the agent the person talks to may commit and push small updates that change no code (docs, the roadmap, `agents.md`) straight to `main`. Subagents commit and push only to their own `feat/<name>` branch. Every other change reaches `main` through a pull request, one per feature:
  1. Agree the plan with the person. For multi-step work write the plan to `docs/plans/yyyymmdd-<name>.md`: the agreed behavior, decisions and a task checklist. While working, only tick its checkboxes: no evidence, progress or status prose.
  2. Branch from an up-to-date `main` as `feat/<name>`, whatever the change, without tracking it: `git switch -c feat/<name> --no-track origin/main` (or `git worktree add <path> -b feat/<name> --no-track origin/main`), and push with `git push -u origin feat/<name>`. With `push.default=upstream`, a branch that tracks `origin/main` pushes straight to `main`. Each implementing agent works in its own git worktree, so parallel tasks don't touch each other's files.
  3. Commit on the branch every step that passes the checks, without asking: one line in the style of the history (`feat: …`, `fix: …`, `chore: …`, `docs: …`), no body, docs updated in the same commit. Any agent on the task may commit; fix-ups are fine, the branch is squashed.
  4. Push the branch and open the PR with `gh pr create`. The title becomes the squash commit, in the same style. The body is short: what changed, compatibility (JSON, Prometheus, library API, config), the checks run, and what was tested by hand and what wasn't.
  5. The orchestrating agent reviews the diff, reruns the checks and sends findings back to the implementing agent until the PR is clean, then hands it to the person. CI runs `make check` and `make test`, but no runtime check on real hardware. Copilot reviews every PR automatically (repository ruleset): fix what is right, answer every thread (what was fixed and in which commit, or why not), then resolve it (`resolveReviewThread` through `gh api graphql`).
  6. The person does the final review and merges: squash, one commit on `main`, the branch is deleted. Never commit or push code to `main` directly.
  7. On conflicts, the orchestrator (or an agent it asks) rebases the branch onto `main`, reruns the checks and pushes with `--force-with-lease`.
- The agent the person talks to orchestrates: it writes each task for a subagent, verifies every report itself (the diff, `make check`, `make test`, runtime results where relevant) instead of trusting it, and keeps the person informed. It does directly only small edits that depend on its own context.
- Ask the person only when JSON or Prometheus output, the config format or the library API must break, a new dependency is needed, or the request is ambiguous. Otherwise decide and say so in the report.
- `changelog.md` is written only in a release commit, by the person's changelog skill from the git history: don't edit it in feature work.
