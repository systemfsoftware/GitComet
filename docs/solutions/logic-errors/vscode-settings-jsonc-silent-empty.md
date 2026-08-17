---
title: "VS Code settings JSONC parsed as empty watcher excludes"
date: 2026-08-17
category: logic-errors
module: gitcomet-state watcher
problem_type: logic_error
component: background_job
symptoms:
  - "Live watching stays disabled after IDE watcherExclude was added"
  - "TooManyFolders warning still reports the capped folder budget"
root_cause: wrong_api
resolution_type: code_fix
severity: high
tags: [file-watcher, jsonc, watcher-exclude, vscode]
---

# VS Code settings JSONC parsed as empty watcher excludes

## Problem

Respecting IDE watcher excludes does not drop a tracked vendor tree from the Linux watch census when the workspace settings file is real VS Code JSONC. Live watching stays off and the UI still reports the capped folder budget.

## Symptoms

- The degraded-watch warning still fires after `files.watcherExclude` was added.
- The reported folder count is the budget plus one, not the real tree size.
- Git-ignore still counts committed vendor directories because they are tracked.

## What Didn't Work

- Adding `files.watcherExclude` in workspace settings. The matcher already honors true entries. The file never loaded.
- Strict JSON parse with a silent empty fallback. VS Code writes comments and trailing commas. Parse fails. The rule set is empty. The census is unchanged.
- Raising the watch budget or host inotify limits. That spends watches the budget exists to protect.

## Solution

Parse the workspace settings file as JSONC: line comments, block comments, and trailing commas. Restrict extras that are not VS Code JSONC (unquoted keys, single-quoted strings, hex numbers). Keep the existing glob matcher.

Record load status as a Copy enum: Disabled, Missing, Unreadable, Parsed. When the file existed and did not parse, the degraded-watch warning says so. When the walk stopped at the cap, the count is worded as more than the budget.

A comment-looking sequence inside a JSON string must stay a string. A fixture with a homepage URL containing `//` still loads the exclude map.

## Why This Works

The failure mode is fail-closed-to-watch: any parse problem yields zero excludes, so the census includes every tracked directory. Tracked vendor trees cannot be skipped by git-ignore. Only a loaded exclude rule can drop them.

JSONC parse makes the file the user actually wrote produce rules. Load status makes a remaining over-budget case diagnosable instead of looking like excludes were ignored.

## Prevention

- Never treat an IDE settings file as strict JSON when the editor writes JSONC.
- Do not map every load failure to the same empty set without a status the warning can name.
- Pin a JSONC fixture with comments, a trailing comma, and a `//` inside a string.

## Related Issues

- Related design pattern: watcher exclude matcher invariants (dir-only gating, bare globstar, coalesced rebuilds)
- Residual test: injected-budget Watching outcome after a JSONC vendor-tree exclude (GitHub issue 58)
