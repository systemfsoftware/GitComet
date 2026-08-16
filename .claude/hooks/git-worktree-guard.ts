#!/usr/bin/env bun

// Project-level guard hook: blocks `git worktree add` while worktrunk is
// installed (repo-owned, portable across machines and harnesses — works for
// any agent whose Bash PreToolUse matcher runs hooks, OMP included).
// Wired via .claude/settings.json; probe 2026-08-16: HOME_PERSISTENT=0 on this
// machine, so the Claude plugin/statusline paths were deliberately NOT wired.

const input = JSON.parse(await Bun.stdin.text())
const command = input?.command ?? ''
const toolName = input?.tool ?? ''

if (toolName !== 'Bash') process.exit(0)
if (!Bun.which('git-wt') && !Bun.which('wt')) process.exit(0)

if (/git\s+worktree\s+add\b/.test(command)) {
  const template = `*** SYSTEM DIRECTIVE ***
STATUS: CRITICAL — git worktree add BLOCKED
ACTION: IMMEDIATE COMPLIANCE REQUIRED

FORBIDDEN: git worktree add (worktrunk is installed on PATH)

Plain \`git worktree add\` creates worktree state that worktrunk cannot see —
branches, paths, and metadata get out of sync. Use the worktrunk workflow.

Use \`wt switch --create <branch>\` instead (or \`git-wt switch --create\`).

*** END DIRECTIVE ***`
  process.stderr.write(template)
  process.exit(2)
}

process.exit(0)
