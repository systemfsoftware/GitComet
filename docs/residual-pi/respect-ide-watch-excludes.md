# Residual register: respect-ide-watch-excludes

Source: ce-code-review run `20260816-205047-861ab0df` (branch `respect-excludes`,
base `d4afa95b`). Fixes landing in `2d1e62b4`; deferred items below are tracked,
not dropped.

## Deferred review findings

### #2 — Rapid config toggles stack stop+start cycles (P1, adversarial)

**Status: accepted with mitigation; not fixed.**

`sync_active_repo` still force-restarts per settings toggle. It was left in
place because the suggested debounce (settle 250 ms before restarting) trades a
bounded, user-driven storm (each toggle = one API-shaped restart; cycles are
bounded by human click rate) for a delayed live application of a `KTD5`
session-settled behavior (restart immediately when the setting changes).

Mitigations already landed in `2d1e62b4`:
- `#1` coalesces rules-change **rebuilds** into the debounce window, so even a
  burst of toggles runs one watcher rebuild per burst instead of one per event;
- `#3` gates `flush`/`flush_if_active` on `monitor_enabled`, so a draining old
  monitor cannot emit stale `RepoExternallyChanged` after the restart.

Remaining exposure: N toggles within ~250 ms create N short-lived monitor
threads, each running an initial scan. States converge on the last toggle.
Owner: watch exclude maintainer. Revisit if rapid toggling shows up in traces
(`repo_monitor_async_join_start` / `repo_monitor_stop_requested`).

## Pre-existing (partitioned by review, not introduced by this branch)

- Zombie handle after root-watch init failure: a monitor whose thread exited
  early stays in `RepoMonitorManager.handles`; `start()` occupancy skip and the
  `config_changed && is_running` gate both fail to revive it. Repairing the
  watch requires deactivate+reactivate. Follow-up: reclaim stale entries in
  `start()`/`sync_active_repo` (`join.is_finished()` or `monitor_enabled ==
  false`).

## Advisory follow-ups (soft buckets, no defect)

- Per-event `WatcherExcludes::config_path(workdir)` allocation in
  `classify_repo_event` (hoist next to `WatcherExcludes::load`, or cache the
  path on the loaded rule set).
- Per-event ancestor segment-list allocation in `is_excluded` (mirror the
  gitignore TTL cache if profiling shows the matcher).
- `excludes_enabled` memo is manager-global; the next per-repo override feature
  must make it per-repo.
- `WatcherExcludes::default()` derives a disabled matcher while the feature
  default is enabled; fixtures forgetting `load(.., true)` silently test the
  off path.
- Documented limitation: strict JSON parse means JSONC comments in
  `.vscode/settings.json` produce an empty rule set (trace-gated log line
  names the file).
- `.vscode` carve-out keeps config self-reload working even for catchall
  excludes; tracked-file edits under excluded trees refresh only on focus —
  matches VS Code behavior, worth a tooltip mention someday.