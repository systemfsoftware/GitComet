---
module: worktrunk scaffold / Claude Code hooks
date: 2026-08-16
problem_type: integration_issue
component: tooling
severity: critical
symptoms:
  - "PreToolUse guard hook never blocked `git worktree add` despite being wired in .claude/settings.json"
  - "Hook exited 0 on the exact command it was built to deny; enforcement was silently absent"
  - "Setting the hook worked in a hand-fed smoke test but not under the real harness"
root_cause: wrong_api
resolution_type: code_fix
tags:
  - claude-code
  - hooks
  - pretooluse
  - worktrunk
related_components:
  - development_workflow
---

# Claude Code PreToolUse hook silently disabled by wrong settings shape and payload fields

## Problem

A repo-owned PreToolUse guard hook (`.claude/hooks/git-worktree-guard.ts`, wired through
`.claude/settings.json`) was supposed to block raw `git worktree add` while worktrunk is
installed. It never blocked anything. Enforcement was absent from the very first run.

Two independent contract mismatches made it inert:

1. **Settings shape.** The documented Claude Code hook schema requires a top-level `"hooks"`
   wrapper and a `"type": "command"` field on every handler:

   ```json
   {
     "hooks": {
       "PreToolUse": [
         {
           "matcher": "Bash",
           "hooks": [{ "type": "command", "command": "...", "timeout": 5 }]
         }
       ]
     }
   }
   ```

   The repo shipped `{ "PreToolUse": [...] }` at top level with no `"type"`. Claude Code
   does not document the flattened form; the hook was silently dropped.

2. **Payload fields.** PreToolUse delivers JSON on stdin of the shape
   `{ "tool_name": "Bash", "tool_input": { "command": "..." } }`. The guard read
   `input.tool` and `input.command`, which do not exist, so its tool-name and command
   checks never matched and it exited 0 for every command.

## Root cause

Both are contract drift: code written against a guessed wire format instead of the
documented one. The smoke test made it worse — it piped the *guessed* shape
(`{"tool": "Bash", "command": "..."}`), so the test passed while production silently
failed. A self-supplied fixture cannot validate against a contract: it validates the
author's assumption.

## Fix

- Restructure `.claude/settings.json` to the documented `{"hooks": {"PreToolUse": ...}}`
  shape with `"type": "command"` handlers.
- Read the documented fields with a tolerant fallback for harness variance:
  `input?.tool_name ?? input?.tool` and `input?.tool_input?.command ?? input?.command`.
- Guard `JSON.parse` so empty or malformed stdin exits 0 instead of crashing the hook.
- Replace substring-regex matching with a tokenized argv scan: `git|git-wt`, optional
  `-C <dir>` / `--git-dir=...` flags, then `worktree add`. A substring regex both
  false-positives (`echo "git worktree add"`) and false-negatives (`git -C /x worktree add`).

Verified with a fixture that mirrors the *real* PreToolUse payload, not the guessed one:

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"git worktree add ../foo"}}' \
  | ./.claude/hooks/git-worktree-guard.ts   # exit 2, directive on stderr
```

## Prevention

- For any hook, copy the input shape from the current hooks reference
  (code.claude.com/docs/en/hooks), not from memory or from an echo of your own fixture.
- Smoke-test with at least the four-case matrix: target command, `git -C` variant,
  literal-string containment (must pass), and empty/malformed stdin (must exit 0).
- Treat a 5-second timeout as a lower-bound worry, not the contract: the harness kills
  a slow hook, and a killed hook is a silent pass. Keep hook bodies fast and fail loudly.

## Runtime choice

The guard's first version was a bun script. Wrong substrate: this machine's
script convention is deno with exact scopes or plain bash (machine CLAUDE.md
OP15/OP17), and a committed repo hook should not add a runtime collaborators
may lack. Porting to deno exposed two more silent-exit traps, both observed
this session on deno 2.9.5:

1. `Deno.stdin.text()` does not exist — the TypeError landed in the JSON.parse
   catch and exited 0 on every input. Read stdin with a chunked
   `Deno.stdin.read()` loop.
2. A PATH probe via `Deno.stat` needs `--allow-read`, and reading PATH needs
   `--allow-env=PATH`; without them the probe throws inside its own catch and
   the guard fails open invisibly.

Gate for both: the Prevention matrix above (block, `git -C`, containment,
empty stdin, plus a PATH-stripped run asserting fail-open) — every early exit
is exercised, not just the intended ones. A catch that swallows a missing-API
or permission error is indistinguishable from a decision to pass.