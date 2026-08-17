---
title: IDE Excludes Still Trip Watch Budget - Plan
type: fix
date: 2026-08-17
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-plan-bootstrap
execution: code
origin: docs/plans/respect-ide-watch-excludes.md
---

# IDE Excludes Still Trip Watch Budget - Plan

## Goal Capsule

- **Objective:** A worktree whose `.vscode/settings.json` excludes the high-cardinality trees (the motivating case is a tracked `repos/` vendor tree) stays under the Linux watch budget, so live watching stays on.
- **Authority:** The user report is the symptom. `docs/plans/respect-ide-watch-excludes.md` owns the exclude source and matcher. This plan only makes that source load for real IDE files and makes a still-over-budget warning tell the truth.
- **Execution profile:** Lightweight bugfix in `gitcomet-state`. Test-first on the JSONC load path.
- **Stop conditions:** R1–R5 hold and the Verification Contract gates pass. Do not raise `MAX_WORKTREE_WATCH_DIRS`, do not change host `fs.inotify.max_user_watches`, do not add user-level or non-VS-Code sources.

---

## Product Contract

### Summary

GitComet already skips IDE-excluded directories in the Linux watch census. Real VS Code / Cursor workspace files are JSONC. The loader uses strict `serde_json`, so comments or a trailing comma empty the rule set. Tracked vendor trees then stay in the census, the walk stops at budget + 1, and the UI reports 4097 folders.

### Problem Frame

`crates/gitcomet-state/src/store/repo_monitor.rs` sets `MAX_WORKTREE_WATCH_DIRS` to 4096. `collect_watchable_dirs_capped` stops once the result is longer than `max_subdirs + 1`, so an over-budget repo always reports 4097 folders even when the real tree is much larger.

`GitignoreMatcher::path_is_tracked` treats a directory as not-ignored when any index entry lives under it. A committed `repos/` or similar vendor tree is therefore never skipped by gitignore. Only `files.watcherExclude` can drop it. That is why the prior plan exists.

`WatcherExcludes` reads only `<workdir>/.vscode/settings.json`. `parse_vscode_watcher_exclude` calls `serde_json::from_str`. On error it returns an empty rule set and a `repo_load_trace` line. VS Code writes JSONC (line comments, block comments, trailing commas). The prior residual already names this as a documented limitation. The user-facing warning still says "Add build/output dirs to .gitignore" and never says the exclude file failed to load.

### Requirements

**Load**

- R1. `WatcherExcludes::load(workdir, true)` accepts a `.vscode/settings.json` that is valid VS Code JSONC: `//` comments, `/* */` comments, and trailing commas. True `files.watcherExclude` entries from that file become exclude rules.
- R2. A file that is not JSON and not JSONC still yields an empty rule set. The load records a failed-parse status distinct from "file missing" and from "parsed, zero true entries".

**Census**

- R3. A tracked directory that matches a loaded exclude rule is absent from `collect_watchable_dirs_capped`, the same as today's strict-JSON path.

**Warning**

- R4. `RepoWatchDegradedReason::TooManyFolders` copy names `.vscode/settings.json` `files.watcherExclude` as a remedy, not only `.gitignore`.
- R5. When the settings file existed and failed to parse, that warning says the file could not be parsed. When the walk stopped at the cap, the count is worded as more than the budget, not as an exact folder total.

### Acceptance Examples

- AE1. `.vscode/settings.json` is JSONC with a comment, a trailing comma, and `"repos/": true`. `repos/` is tracked. `collect_watchable_dirs` does not contain `repos` or anything under it. (`Covers R1, R3`)
- AE2. The same file with `{ not json` yields zero rules and a failed-parse status. (`Covers R2`)
- AE3. Over-budget after a failed parse: the warning names the folder budget and says settings.json could not be parsed. (`Covers R4, R5`)
- AE4. Over-budget after a successful load: the warning names watcherExclude and does not claim the capped probe count is the exact tree size. (`Covers R4, R5`)

### Scope Boundaries

- **In** — JSONC load of the existing workspace file; load-status on the rule set; warning copy; tests that a JSONC `repos/` exclude drops a tracked tree from the census.
- **Deferred** — user-level VS Code / Cursor settings; `.code-workspace`; `files.exclude` / `search.exclude`; VS Code default watcherExclude; other IDEs.
- **Not this change** — `MAX_WORKTREE_WATCH_DIRS`; host inotify sysctl; gitignore tracked-path carve-out; matcher glob semantics.

### Key Decisions

- **Workspace file stays the only source.** Same source as the origin plan KTD1. User-level settings are a new source, not a load bug. Governs R1.
- **JSONC is the file format, not a best-effort strip.** Comments inside strings must survive. Governs R1, R2.

---

## Planning Contract

### Key Technical Decisions

- KTD1. **Parse JSONC, then read `files.watcherExclude` as today.** Keep the existing glob matcher. Change only the text-to-`serde_json::Value` step in `parse_vscode_watcher_exclude`.
- KTD2. **Use a workspace-pinned JSONC crate that accepts comments and trailing commas.** Do not hand-roll a comment stripper. Do not switch to JSON5 (unquoted keys and other extras are not VS Code JSONC). Pin it under `[workspace.dependencies]` and take it from `gitcomet-state` the same way `serde_json` is taken. If two crates both cover JSONC, pick the smaller one that `serde_json::Value` can consume. Restrict parser options to comments and trailing commas; do not enable JSON5 extras.
- KTD3. **Record load status on `WatcherExcludes` as a `Copy` enum with no payloads:** `Disabled`, `Missing`, `Unreadable`, `Parsed`. Carry the true-entry count as a sibling `parsed_rule_count: usize` on the struct (0 when not `Parsed`). `WatcherExcludes::load(workdir, enabled) -> Self` stays the constructor. Add `pub(crate) fn load_status(&self) -> LoadStatus`. `WatcherExcludes::default()` is `Disabled` with count 0, so existing `default()` test fixtures keep compiling. Split parse into a private `parse_vscode_watcher_exclude_with_status(workdir) -> (Vec<ExcludePattern>, LoadStatus, usize)` so the four early returns each set the matching variant. `is_excluded` is unchanged.
- KTD4. **Widen `TooManyFolders` and keep `RepoWatchDegradedReason` `Copy`.** Shape: `{ dir_count: usize, capped: bool, load_status: LoadStatus }`. `WatchSetupOutcome::WorktreeSubdirsSkipped` carries the same `{ dir_count, capped }` pair; set `capped = true` when the walk exited because `result.len() > max_subdirs + 1`. The reducer says "more than {budget} folders" when `capped`, else the literal count. R4 still names watcherExclude when status is `Disabled` (the toggle is the on-ramp). Recovery reload at `attempt_degraded_watch_recovery` assigns `WatcherExcludes::load(...)` so status refreshes with the rules. Update `repo_watch_degraded_pushes_warning_notification`.
- KTD5. **Do not raise the 4096 budget.** If JSONC load still leaves a repo over budget, the honest warning is the product. Raising the cap spends inotify watches the budget exists to protect.

### Assumptions

- The motivating repo's excludes live in workspace `.vscode/settings.json`, not only in user settings. That matches the origin plan's named file.
- A JSONC-capable crate can be added without touching UI crates.

### Sequencing

U1 (parse + status) then U2 (warning + census tests that need status).

---

## Implementation Units

### U1. JSONC load and status

- **Goal:** Real VS Code settings load. Failed parse is distinguishable from missing or empty.
- **Requirements:** R1, R2
- **Files:** `crates/gitcomet-state/src/store/watcher_excludes.rs`; `crates/gitcomet-state/Cargo.toml`; root `Cargo.toml` / `Cargo.lock` for the new workspace pin.
- **Approach:** Replace `serde_json::from_str` with the KTD2 parser. Keep the rest of `parse_vscode_watcher_exclude` (true-only entries, unsupported-syntax skip, empty-pattern skip). Implement KTD3 (`LoadStatus`, `load_status()`, `default()` = Disabled). Existing strict-JSON tests stay green. Add JSONC fixtures.
- **Test Scenarios**
  - T1.1 JSONC with `//`, `/* */`, and a trailing comma loads `"repos/": true` and excludes `repos` and `repos/x`. Status is `Parsed`, count 1.
  - T1.2 A string value containing `//` is not treated as a comment.
  - T1.3 `{ not json` → zero rules, status = `Unreadable`.
  - T1.4 Missing file → zero rules, status = `Missing`.
  - T1.5 Valid JSON with no `files.watcherExclude` object → zero rules, status = `Parsed`, count 0.
  - T1.6 Pre-existing strict-JSON tests in `watcher_excludes.rs` (`missing_file_yields_empty_rules`, `invalid_json_yields_empty_rules`, `only_true_entries_are_excludes`, and the other origin-plan matcher rows) still pass.
- **Verification:** `cargo test -p gitcomet-state watcher_excludes`

### U2. Census proof and warning copy

- **Goal:** A JSONC `repos/` exclude drops a tracked vendor tree from the watch set. The over-budget warning names the real remedy and does not lie about the count.
- **Requirements:** R3, R4, R5
- **Files:** `crates/gitcomet-state/src/store/repo_monitor.rs`; `crates/gitcomet-state/src/msg/message.rs`; `crates/gitcomet-state/src/store/reducer.rs`; the existing `repo_watch_degraded_pushes_warning_notification` test.
- **Approach:** Thread `load_status()` and `capped` into `WorktreeSubdirsSkipped` / `TooManyFolders` per KTD4. When the walk hits the cap, the warning says more than `max_dirs` folders, not an exact total. Mention `files.watcherExclude`. If status is `Unreadable`, say settings.json could not be parsed. Keep `MAX_WORKTREE_WATCH_DIRS` at 4096.
- **Test Scenarios**
  - T2.1 JSONC settings with `"repos/": true`, tracked files under `repos/`, plus a sibling `src/`: `collect_watchable_dirs` contains `src` and does not contain `repos`. Assert `load_status() == Parsed` and `parsed_rule_count == 1`.
  - T2.2 Same fixture under a small injected budget: if only `repos/` pushed it over, setup is `Watching`, not `WorktreeSubdirsSkipped`.
  - T2.3 Failed-parse settings plus an over-budget tree: warning text includes parse failure and watcherExclude. Assert `load_status() == Unreadable`.
  - T2.4 Successful load, walk hits the cap: warning contains "more than" the budget and watcherExclude, and does not treat the probe length as an exact census. `capped` is true.
  - T2.5 Existing `ide_excludes_apply_even_to_tracked_files` and `repo_watch_degraded_pushes_warning_notification` still pass after the reason shape change.
  - T2.6 `WatcherExcludes::default()` reports `Disabled` and excludes nothing.
- **Verification:** `cargo test -p gitcomet-state repo_monitor repo_watch_degraded`; `cargo test -p gitcomet-state`

---

## Verification Contract

| Gate | Command | Applies to | Exit signal |
|---|---|---|---|
| Matcher + load | `cargo test -p gitcomet-state watcher_excludes` | U1 | All pass, including T1.1–T1.6 |
| Monitor + warning | `cargo test -p gitcomet-state repo_monitor repo_watch_degraded` | U2 | All pass, including T2.1–T2.6 |
| Crate | `cargo test -p gitcomet-state` | U1, U2 | All pass |
| Clippy | `cargo clippy -p gitcomet-state -- -D warnings` | U1, U2 | Clean |
| Compile | `cargo check -p gitcomet-state` | U1, U2 | Succeeds |

No UI or browser surface. The settings toggle is unchanged.

---

## Definition of Done

**Global.** R1–R5 hold. Verification Contract gates green. Diff is this fix and its tests. No host sysctl change. No budget constant change.

**Per-unit.** Each unit's tests fail on the bug they name: T1.1 fails if JSONC still yields an empty set; T2.1 fails if a tracked excluded tree stays in the census; T2.4 fails if the warning reports the capped probe as an exact total.

**Cleanup.** No probe crates, no leftover parse-debug prints beyond the existing `repo_load_trace` / setup `eprintln` lines.

---

## Appendix

Software-wiki queries before this write (lex + vec + hyde, intent: JSONC `settings.json` parse; intent: inotify watch budget / too-many-folders) returned no settled answer. Top hits were plugin discovery and OpenCode context composition. The load bug is grounded in this repo: `serde_json::from_str` at `crates/gitcomet-state/src/store/watcher_excludes.rs`, budget 4096 at `crates/gitcomet-state/src/store/repo_monitor.rs`, tracked-path carve-out in `GitignoreMatcher::path_is_tracked`, residual note in `docs/residual-pi/respect-ide-watch-excludes.md`.
