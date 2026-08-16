#!/usr/bin/env bun

// Project-level guard hook: blocks `git worktree add` while worktrunk is
// installed (repo-owned; wired for Claude Code via .claude/settings.json).
// Wired via .claude/settings.json; probe 2026-08-16: HOME_PERSISTENT=0 on this
// machine, so the Claude plugin/statusline paths were deliberately NOT wired.

// Claude Code PreToolUse payload shape (hooks reference, stdin):
//   { "hook_event_name": "PreToolUse", "tool_name": "Bash",
//     "tool_input": { "command": "..." }, "tool_use_id": "..." }
// Tolerate the legacy flat {tool, command} shape for other harnesses.
let input
try {
  input = JSON.parse(await Bun.stdin.text())
} catch {
  process.exit(0)
}

const toolName = input?.tool_name ?? input?.tool ?? ''
const command = input?.tool_input?.command ?? input?.command ?? ''

if (toolName !== 'Bash') process.exit(0)
if (!Bun.which('git-wt') && !Bun.which('wt')) process.exit(0)

// Token scan, not substring regex: match `git|git-wt`, optional -C <path> /
// --git-dir / --git-common-dir flags, then `worktree add`. This catches the
// agent-synthesized `git -C <dir> worktree add` form and does not fire on
// harmless strings that merely contain the command text (echo, help, grep).
const argv = command
  .split(/\s+/)
  .map((t) => t.trim())
  .filter(Boolean)
let gitIdx
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === 'git' || argv[i] === 'git-wt') { gitIdx = i; break }
}
if (gitIdx === undefined) process.exit(0)

// Walk the argv: `worktree add` must occur after any leading flags.
let i = gitIdx + 1
while (i < argv.length && (argv[i] === '-C' || argv[i] === '--git-dir' || argv[i] === '--git-common-dir')) {
  // -C consumes the next token
  i += (argv[i] === '-C') ? 2 : 1
}
// Also allow `--git-dir=...` and `--git-common-dir=...` assignment form.
while (i < argv.length && /^--(git-dir|git-common-dir)=.+/.test(argv[i])) i++
if (argv[i] !== 'worktree') process.exit(0)
if (argv[i + 1] !== 'add') process.exit(0)

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