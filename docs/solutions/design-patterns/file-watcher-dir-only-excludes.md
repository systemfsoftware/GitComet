---
title: "Respecting IDE watcher excludes: dir-only gating, bare `**`, and coalesced rebuilds"
module: gitcomet-state watcher
date: 2026-08-16
problem_type: design_pattern
component: background_job
severity: high
applies_when:
  - "Adding an out-of-band exclude source (IDE watcherExclude, tool ignore lists) to a filesystem watcher"
  - "Writing a segment-based path matcher with globstar semantics, directory-only rules, or subtree exclusion"
  - "Reloading watcher rules on config-file edits without per-save rebuild storms"
tags: [file-watcher, exclude-rules, glob-matcher, debounce, dir-only]
---

# Directory-only exclude rules, bare `**`, and watcher rebuild coalescing

## Problem

GitComet's repo file watcher gained an out-of-band exclude source: VS Code's
`files.watcherExclude` map in the IDE settings file inside the worktree. Four
defect classes surfaced in code review (run 20260816-205047) and are now pinned
in tests; each is a durable invariant, not a one-off patch.

## Mechanisms

1. **Directory-only gating at function scope.** A pattern ending in `/` applies
   to directories only. The event hint "this path is a file" describes the
   event path — not its ancestors, which are directories by construction. A
   matcher that bails out at the top when `dir_only && hint == file` silently
   exempts every file-typed event below the excluded tree. Observable
   consequence: a Create(File) under an excluded directory was classified as a
   worktree change even though the pattern ended in a slash; recursive watcher
   backends (macOS/Windows) surfaced it.

2. **Bare `**` inverted to "matches nothing".** A pattern of just `**` compiles
   to one globstar segment. Any "first segment must exist" guard
   (`first().and_then(as_ref) else return false`) turns the documented
   zero-or-more-at-any-depth semantics into the exact inverse of VS Code.
   Silent, because nothing observed the empty result.

3. **Exponential backtracking on repeated globstars.** A backtracking
   globstar matcher with `T(n,k) = T(n,k-1) + T(n-1,k)` doubles call count per
   extra `**`. Since `**` is zero-or-more, `**/**` is behaviorally identical to
   `**`; consecutive globstars can be collapsed at parse time, keeping matching
   linear. The rule source is user-controlled — an adversarial `settings.json`
   stalls the watcher thread (availability).

4. **Rules-change short-circuit swallows sibling flags.** When an event batch
   contains the config file and a git-state path together, an early
   "rules changed, return worktree flag" return skips the path loop that sets
   `index`/`git_state`/`tags`. Batched events are the norm on FSEvents and RDC.
   Consequence: stale branch/commit UI until the next git event.

5. **Per-event watcher rebuilds.** Rebuilding the watcher (drop all watches,
   re-walk, re-register) once per config event turns an editor save burst into
   a rebuild storm — each rebuild also re-opens the git repository. The flush
   of the debounce window is the natural single rebuild point.

## Invariants

1. **Candidate-scoped rule gating.** For path rules, the rule's restriction
   (dir-only, file-only) constrains the event path only; every strict ancestor
   candidate is a directory and is always eligible. Gate per candidate, never
   at function top.

```
for (index, segments) in candidates.enumerate():
    if dir_only and hint == File and index == 0:  # event path itself
        continue
    if match(pattern, segments): return True
```

2. **Degenerate globs are defined, not rejected:** bare `**` matches every
   path; a trailing `**` trims to the prefix (zero segments); consecutive `**`
   collapse at parse. If a syntax is unsupported, its entry must never match
   (conservative: path stays watched).

3. **Rules reload in place; watchers rebuild once per flush.** Reload the rule
   state during event classification (cheap, keeps the watch alive); set a
   pending-rebuild flag; run the expensive rebuild when the debounced change
   flushes. A burst costs one rebuild, and the flushed event carries the
   refresh the store needs.

4. **Sibling signals survive short-circuits.** Any "reconfigure" outcome
   merges with the flags computed from the full path scan; never return early
   out of the classification loop.

5. **Self-reload carve-out at one chokepoint.** The source of the rule
   settings must never exclude itself: enforce an exemption for the settings
   directory at matcher entry one, not at each call site.

## Verification

- State suite: 758 lib + 16 integration tests; matcher pins added for the
  dir-hint rows (an excluded-directory-subtree file with a file hint returns
  excluded; a file named like the directory returns not-excluded), bare `**`,
  globstar collapse, and the mixed config+git-state event.
- The UI toggle test clicks the real rendered row through the gpui visual
  harness — a direct handler call cannot pin row wiring.
- Clippy `-D warnings` and `cargo fmt --check` clean on the touched crates.

## Prevention

- Any new rule source (per-repo override, shared ignore files) must reuse
  `WatcherExcludes`' candidate model (`pattern_excludes`, `is_excluded`,
  `split_segments`) instead of re-deriving glob semantics.
- When mocking watcher restarts in tests, assert on monitor thread identity —
  a "still running" boolean cannot falsify a spurious restart.
- Track deferred items in docs/residual-pi/respect-ide-watch-excludes.md the
  related plan: docs/plans/respect-ide-watch-excludes.md.