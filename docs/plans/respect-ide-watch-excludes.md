---
title: Respect IDE Watcher Excludes - Plan
type: feat
date: 2026-08-16
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: legacy-requirements
---

# Respect IDE Watcher Excludes - Plan

## Goal Capsule

- **Objective:** The repo file watcher stops watching paths that the user's IDE is already configured to ignore — the primary source being VS Code's `files.watcherExclude` in `.vscode/settings.json` — and the behavior is configurable from the Settings window.
- **Authority:** The invoking user's request names the source and the configurable requirement; session-settled choices are labeled on KTD1.
- **Execution profile:** Standard, synchronous code work; keep the existing "git-dir always watched, worktree watch budget, degraded-watch warnings" guarantees intact.
- **Stop conditions:** All R-IDs implemented and the Verification Contract gates pass. A found defect in `repo_monitor.rs` semantics that broadens the feature (e.g. applying IDE excludes to git-state events) is a blocker to surface, not a silent scope change.

---

## Product Contract

### Summary

GitComet watches a repo's non-ignored worktree directories and reacts to file changes. Large build/output directories (`node_modules`, `target`, `dist`) already appear as `.gitignore` entries in well-formed repos, but repos with sparse ignore hygiene or IDE-local excludes still flood the inotify queue and the watch budget. The user asked to make the watcher respect common IDE exclude settings — the named example is VS Code's `files.watcherExclude` — and to make the behavior configurable.

### Problem Frame

The current watcher (`crates/gitcomet-state/src/store/repo_monitor.rs`) treats git-ignore rules as the only exclude source. The user's own repo (systemfsoftware/systemfsoftware) is configured with `files.watcherExclude: {"repos/": true}` in `.vscode/settings.json` — a directory the repo's watcher would otherwise watch and refresh on. This is out-of-band configuration the monitor loses today; the harm is wasted inotify watches, event churn, and queue-overflow risk that the IDE already opted out of.

### Requirements

**Exclude source and matching**

- R1. The monitor reads watcher-exclude entries from `<workdir>/.vscode/settings.json` key `files.watcherExclude`; an entry with value `true` is an exclude, `false` and non-boolean values are ignored. A missing file, unparseable JSON, or non-object `files.watcherExclude` yields an empty rule set.
- R2. Path matching follows VS Code glob semantics: `**` matches zero or more segments, `*`/`?` match within a segment, a pattern containing no `/` matches the name at any depth, a pattern containing `/` is anchored to the repository root, a directory match excludes the whole subtree, a trailing `/` restricts to directories. Patterns containing syntax outside that set (e.g. `[...]`) never match — conservative: still watched — and log a one-time trace line naming the pattern, matching T1.1's malformed-file log precedent.

**Watching behavior**

- R3. A worktree path that matches R2 is not watched: on Linux it is skipped in the per-directory watch set (alongside `is_ignored_dir`), and on every platform its events are dropped before classification (the parallel of `is_ignored_worktree_path_with_hint`).
- R4. `.vscode/settings.json` is a watch-config path: changing it reloads the exclude rules and rebuilds the worktree watches through the same path `.gitignore` edits use today (`classify_repo_event` rules-changed flag → `build_workdir_watcher`). The `.vscode` directory stays watchable even when an exclude pattern covers it, so the config can always reload itself. The carve-out lives inside `WatcherExcludes::is_excluded`: paths at or under `<workdir>/.vscode` never match, so every skip site (directory collection and event classification) inherits R4 without special-casing.
- R5. IDE excludes never apply to the git dir: `.git` handling, the `GIT_DIR_WATCH_DENYLIST`, and `is_git_related_path` are unchanged.

**Configurability**

- R6 — Respecting IDE watcher excludes is a setting — new `Msg::SetRespectIdeWatcherExcludesEnabled(enable)` — default `true`, rendered as a toggle in the Settings window's Change tracking card, persisted in the session file, and synced into `AppState` at startup the same way `Msg::SetGitLogSettings` is at `crates/gitcomet-ui-gpui/src/view/mod.rs:1420`.
- R7 — Flipping the setting while a repo is active restarts that repo's monitor with the new config (stop + start through the existing monitor-sync site in `crates/gitcomet-state/src/store/mod.rs:211-227`), so the change applies live without a restart.
- R8 — Excludes apply even when a path contains tracked files, matching VS Code behavior. This is recoverable: the git-dir watch and focus-triggered full refresh still run; the setting can be turned off.

### Acceptance Examples

- AE1 — Repo root contains `.vscode/settings.json` with `{"files.watcherExclude": {"**/node_modules": true}}`; `node_modules/` appears later. The directory is not added to the watched set and writes inside it never cause a refresh. (`Covers R1, R2, R3`)
- AE2 — Same repo after the file changes to drop `**/node_modules`: the edit to `settings.json` reloads rules, rebuilds the watch set, and subsequent `node_modules` writes do refresh. (`Covers R2, R4`)
- AE3 — Token `files.watcherExclude` with `"repos/": true`: root `repos/` is excluded; sibling `src/repos/` stays watched (anchored pattern). (`Covers R2`)
- AE4 — Setting off (R6) makes the same repo behave as today, `node_modules` watched again after the toggle commit. (`Covers R6, R7`)
- AE5 — Malformed `settings.json` at start: no directories excluded, monitor starts normally, no crash, log line emitted. (`Covers R1`)
- AE6 — A tracked file lives inside an excluded directory (`repos/` excludes `repos/README.md`): an edit to that file does not trigger a live refresh, and the focus-triggered full refresh does pick the change up. (`Covers R8`)

### Scope Boundaries

- **In** — VS Code `files.watcherExclude` as the single IDE source; the configurable on/off setting; Linux per-directory watch set and event classification on all platforms.
- **Deferred for later** — other IDE sources (Zed, JetBrains); `.code-workspace` multi-root files; user-defined pattern lists; per-repo overrides.
- **Not part of this change** — git status/scan behavior (`git status` already honors `.gitignore`); the degraded-watch budget and its warning text; the git-dir watch denylist.

### Success Criteria

The prerequisites are the R- requirements without additional metrics; watcher correctness is pinned by the named test scenarios in the units (R3's no-miss property is bounded by the git-dir watch + focus refresh that predate this change).

---

## Planning Contract

### Key Technical Decisions

- KTD1 — **Source is `.vscode/settings.json#files.watcherExclude` only.** (session-settled: user-directed — chosen over other IDE files and a GitComet-owned pattern list: the user named VS Code's file as THE example) The reader is structured so a second source (e.g. `.zed/settings.json`) can be added as another loader behind the same matcher, but no abstraction is built ahead of need.
- KTD2 — **Glob semantics mirror VS Code immediately, pinned by tests.** Directory match excludes the sub-tree, `**` matches zero segments, no-slash patterns match at any depth, slash patterns are root-anchored. This differs from gix ignores (`dist/**` does not match `dist` in gitignore terms) so the matcher must not silently inherit gix semantics.
- KTD3 — **Purpose-built segment matcher in `crates/gitcomet-state`** (no new dependency): a small glob walk with a pinned contract table (R2), instead of `gix::glob::Pattern` whose bash-mode semantics differ from VS Code at exactly the pinned edges. ~80-120 lines + tests, deterministic.
- KTD4 — **Tracked files are not carved out of IDE excludes** (differs from the gitignore matcher's `path_is_tracked` carve-out). Linux never watches an excluded directory, so a carve-out could only fire for events that never arrive; VS Code behaves the same; the toggle off is the escape hatch. Pinned by AE6 and T2.7.
- KTD5 — **The toggle restarts the active monitor through the existing monitor-sync site.** `handle_reducer_effects` already stops non-active repos and calls `RepoMonitorManager::start` for the active one; the manager records the last-applied setting and force-restarts when it changes. No new Effect variant, no message special-casing.
- KTD6 — **Default enabled.** The request demands respecting IDE excludes; disabled would keep every new excluded repo on today's behavior with no opt-in prompt.

### High-Level Technical Design

The exclude rule set is a per-monitor struct loaded once per monitor thread, reloaded on its config event:

`crates/gitcomet-state/src/store/watcher_excludes.rs` — `WatcherExcludes::load(workdir, enabled)` reads and parses `.vscode/settings.json` when `enabled` (R1), building a `Vec<ExcludePattern>`; `is_excluded(rel: &Path) -> bool` implements R2/R3 against the workspace-relative POSIX path. `repo_monitor.rs` threads an instance alongside `GitignoreRules`:

- directory collection `collect_watchable_dirs_capped` / `watch_created_dirs` skip excluded dirs (Linux watch set);
- `is_ignored_worktree_path_with_hint` drops excluded paths (all platforms);
- `classify_repo_event` runs `is_ide_watcher_excludes_config_path` (path == `<workdir>/.vscode/settings.json`) alongside the `.gitignore` paths; its rule flag extends `gitignore_changed` to a combined `rules_changed`;
- `build_workdir_watcher` passes the excludes through, and `attempt_degraded_watch_recovery` re-loads them alongside `GitignoreRules`.

Config flow (state → watcher):

- `AppState.respect_ide_watch_excludes: bool` (default true, via a manual `Default` impl) mirrors `Msg::SetRespectIdeWatcherExcludesEnabled` and the four persistence touch points named in U3 (`UiSession`, `UiSettings`, `UiSessionFile`, both mapping functions).
- `RepoMonitorManager::start` gains the setting; `handle_reducer_effects` reads it from state and the manager restarts the active monitor on change (KTD5).
- The settings window adds a Switch row (`toggle_row`) to the Change-tracking card and the flow `persist_preferences` + `Msg` dispatch, following `set_history_show_tags` at `crates/gitcomet-ui-gpui/src/view/settings_window.rs:1969`.

### Assumptions

- The `.vscode` directory is usually not gitignored in the repos GitComet watches; if a repo ignores it, that `.gitignore` rule still wins — the carve-out in R4 covers IDE exclude patterns, not `.gitignore`.
- notify event paths are absolute (already canonized in `repo_monitor_thread`), so `strip_prefix(workdir)` yields stable relative paths for matching.
- `files.watcherExclude` entries with value `false` are explicit non-excludes; GitComet has no built-in default exclude list for `false` to override.
- VS Code writes `settings.json` as JSONC (comments and trailing commas allowed). The parse is strict `serde_json` (R1): a file that only fails JSONC constructs yields an empty rule set and the T1.1 log line names `settings.json`, so the silent-emptiness case is diagnosable.

### Sequencing

U1 (matcher) → U2 (monitor wiring); U3 (state/config/persistence) and U4 (settings UI) are independent of each other and only need U1/U2 at integration time. All four units land on the same branch; commits reflect unit boundaries.

---

## Implementation Units

### U1. Watcher-exclude parser and matcher

- **Goal:** Make `WatcherExcludes` with load + is_excluded, behavior nailed by tests.
- **Files:** `crates/gitcomet-state/src/store/watcher_excludes.rs`(new); `crates/gitcomet-state/src/store/mod.rs` (mod declaration); unit tests in the new file.
- **Approach:** `WatcherExcludes::load(workdir: &Path, enabled: bool) -> Self` — disabled or absent/invalid file yields an empty set; parse `serde_json::Value` (already a state dep); extract `files.watcherExclude` map of `(glob, bool)`. `is_excluded(&Path) -> bool` — convert to POSIX rel string, match each pattern (R2) in order; first match wins; subtree-match via ancestor scan. Unsupported syntax (char class `[`, `]`, `!`-prefixed patterns) compiles to never-match and logs a one-time trace line naming the pattern (R2). Paths at or under `<workdir>/.vscode` return false unconditionally — the R4 config carve-out lives here, one enforcement point.
- **Test Scenarios**
  - T1.1 file missing / empty / invalid JSON / `files.watcherExclude` not an object → empty set, and the parser emits a trace log line naming `settings.json`.
  - T1.2 entries `true` vs `false` vs non-boolean → only `true` is an exclude.
  - T1.3 anchored: `repos/` matches `repos`, `repos/x`, `repos/a/b`; not `src/repos`.
  - T1.4 depth-any: `repos` matches `repos` and `a/b/repos` + subtrees.
  - T1.5 `**/node_modules` and `**/node_modules/**` match nested `x/node_modules` incl. the dir itself (zero-segment `**`).
  - T1.6 `dist/**` matches `dist` itself and all descendants.
  - T1.7 `*`/`?` segment match; `**` zero-or-more; trailing `/` dir-only semantics.
  - T1.8 unicode/space paths match without normalization surprises.
  - T1.9 disabled `WatcherExcludes` returns false for everything.
- **Verification:** `cargo test -p gitcomet-state watcher_excludes` green; `cargo clippy -p gitcomet-state`.

### U2. Monitor wiring

- **Goal:** The watching paths — collect, observe, classify, reload — use `WatcherExcludes`.
- **Files:** `crates/gitcomet-state/src/store/repo_monitor.rs`, its `#[cfg(test)] mod tests`, and `crates/gitcomet-state/src/store/tests/repo_monitor.rs`; no changes outside `gitcomet-state`.
- **Approach:** Thread `mut watcher_excludes: WatcherExcludes` (load in `repo_monitor_thread` and in `attempt_degraded_watch_recovery`) into `build_workdir_watcher` → `setup_workdir_watch{,_with_limit}` → `collect_watchable_dirs_capped` → `add_subtree_watches`; and into `classify_repo_event` → `is_ignored_worktree_path_with_hint` and `watch_created_dirs`. Skip = `is_ignored_dir(...) || watcher_excludes.is_excluded(path)`. Add `is_ide_watcher_excludes_config_path` (`path == workdir/.vscode/settings.json`); in `classify_repo_event` reload the exclude set when its path appears and set the existing `gitignore_changed` → rename to `rules_changed` (the caller's rebuild condition keeps working). Non-Linux recursive watcher: the same classification drops excluded events — parity achieved.
- **Test Scenarios**
  - T2.1 excluded dirs absent from `collect_watchable_dirs_capped` output (and over budget still capped).
  - T2.2 `classify_repo_event` drops events under an excluded path; kept events are unchanged in kind/change compositing (e.g. index event in an excluded workdir stays an index change).
  - T2.3 `watcher_excludes` config file event → `rules_changed: true`, reload replaces the rule set (stale rule gone) — with/without the setting enabled.
  - T2.4 `.vscode` excluded pattern (`**/.vscode`) → the dir still watches; settings.json edit triggers reload.
  - T2.5 existing `.gitignore` re-init test (`gitignore_rules_change_rebuilds_watch_set`-style) still passes with excludes enabled and disabled.
  - T2.6 monitor started with enabled=false behaves as today: existing tests in `store/tests/repo_monitor.rs` pass after mechanical call-site parameter updates (no `.vscode` interference).
- T2.7 tracked file under an excluded directory: its edit produces no refresh (classification drops it), and the tracked-path carve-out of the gitignore matcher is not consulted for IDE excludes — pins AE6.
- **Verification:** `cargo test -p gitcomet-state repo_monitor`; `cargo test -p gitcomet-state` full.

### U3. Config surface: state, message, persistence

- **Files:** `crates/gitcomet-state/src/model.rs` (AppState field), `crates/gitcomet-state/src/msg/message.rs` + reducer.rs (Msg variant + reducer arm), `crates/gitcomet-state/src/store/mod.rs` (start/re-sync with setting), `crates/gitcomet-state/src/store/repo_monitor.rs` (`RepoMonitorManager::{start, sync_active}` signatures), `crates/gitcomet-state/src/session.rs` (UiSettings + SessionFile load/persist).
- **Approach:** Add `AppState::respect_ide_watch_excludes: bool` — do not put it inside `GitLogSettings` since that struct is a composite replica for the log panel. `AppState` derives `Default` today, so the derived impl must be replaced with a manual `Default` that sets the field to `true` (a bare `bool` field would default to `false` and invert the settled default for store-only startup paths). `Msg::SetRespectIdeWatcherExcludesEnabled(bool)`; reducer sets the field; `handle_reducer_effects` sync reads it. `RepoMonitorManager` records the last applied value and restarts the active repo on change (KTD5). Persistence touches four places, mirroring how `history_show_tags` flows: add `respect_ide_watch_excludes: Option<bool>` to `UiSession`, `UiSettings`, and `UiSessionFile`, with mapping lines in both `load_from_path` and `persist_ui_settings_to_path`; boot dispatch from `view/mod.rs` startup flow (per U4) so a persisted `false` applies from the first monitor start.
- **Test Scenarios**
  - T3.1 reducer: message toggles field; other state untouched.
  - T3.2 `RepoMonitorManager::is_running` unchanged; sync restarts only the active repo exactly once per config change (spy on the watch thread start).
  - T3.3 session round-trip: `persist_ui_settings` writes `None` default, `Some(false)`, and reads them back; old session files without the field load as `None` (backward compatible).
  - T3.4 manager default when the message never fires = `true`.
- **Verification:** `cargo test -p gitcomet-state` full (the boot-dispatch UI test belongs to U4, which delivers the dispatch).

### U4. Settings-window toggle

- **Files:** `crates/gitcomet-ui-gpui/src/view/settings_window.rs`, `crates/gitcomet-ui-gpui/src/view/mod.rs` (startup store dispatch), `crates/gitcomet-ui-gpui/src/view/test_support.rs` (expose for UI tests).
- **Approach:** Add `SettingsWindowView.respect_ide_watch_excludes: bool` (reads `UiSession.respect_ide_watch_excludes` default true); a `toggle_row` "Respect IDE watcher excludes" in the Change-tracking card (`settings_window_change_tracking_card`) with a supporting description naming the source (`.vscode/settings.json` → `files.watcherExclude`) and a `set_respect_ide_watch_excludes` handler: update field, `persist_preferences(cx)`, `store.dispatch(Msg::SetRespectIdeWatcherExcludesEnabled(v))`. No new `SettingsSection` — a single switch row follows the `history_show_tags` pattern. Dispatch on app boot from restored session (view/mod.rs line ~1419, next to the `SetGitLogSettings` dispatch) so state ↔ session stay in sync.
- **Test Scenarios**
  - T4.1 toggle renders in the Change tracking card with the persisted value.
  - T4.2 clicking flips the field, persists, and dispatches the message.
  - T4.3 restored session `false` shows unchecked; boot dispatch sets state before the first repo activation (deterministic order test).
- **Verification:** `cargo test -p gitcomet-ui-gpui respect_ide` (covers the toggle row, the toggle handler, and the boot-dispatch test, all named `respect_ide*`); the U3 state suite still passes.

---

## Verification Contract

| Gate | Command | Applies to | Exit signal |
|---|---|---|---|
| State unit + integration | `cargo test -p gitcomet-state` | U1-U3 | All pass — existing monitor tests keep their behavior after mechanical call-site parameter updates |
| Module-scoped | `cargo test -p gitcomet-state watcher_excludes` and `cargo test -p gitcomet-state repo_monitor` | U1, U2 | All pass |
| UI | `cargo test -p gitcomet-ui-gpui settings_window` (and the new boot-dispatch test) | U4 | All pass |
| Clippy | `cargo clippy -p gitcomet-state` and `cargo clippy -p gitcomet-ui-gpui` | all | no new warnings |
| Compile | `cargo check --workspace` | all | succeeds |

Behavioral verification is covered by AE1-AE5 (each maps to a unit). No browser surface exists — the change keys off the settings window and the store.

---

## Definition of Done

**Global.** All R-IDs implemented; the Verification Contract gates green; the branch diff contains only this feature and its tests; no experimental code left behind (clean `git status` after the units).

**Per-unit.** Each unit's File/Approach lands as specified and passes the unit's Test Scenarios (T rows) and its Verification rows; unit tests exercise observable behavior, not internal plumbing (each pins a public contract in R/KTD).

**Cleanup criterion.** The final diff removes every temporary probe/trace; the plan's own artifact (`docs/plans/respect-ide-watch-excludes.md`) is the only docs file added.