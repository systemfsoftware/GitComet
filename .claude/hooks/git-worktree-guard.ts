#!/usr/bin/env -S deno run --allow-read --allow-env=PATH
// Project-level guard hook: blocks `git worktree add` while worktrunk is
// installed. Wired for Claude Code via .claude/settings.json.
//
// Claude Code PreToolUse payload (stdin, per the hooks reference):
//   { "hook_event_name": "PreToolUse", "tool_name": "Bash",
//     "tool_input": { "command": "..." }, ... }
//
// Fail-open by design: when worktrunk is not installed, plain git worktree
// is fine without it. Exit 2 blocks the tool call; stderr is the reason.
// Scopes are exact: --allow-read for the PATH stat probe, --allow-env=PATH
// to read PATH itself; stdin/stderr need no permission.

// Read stdin to EOF (Deno.stdin.text() is absent on deno 2.9).
async function readStdinAll(): Promise<string> {
  const chunks: Uint8Array[] = []
  const buf = new Uint8Array(65536)
  for (;;) {
    const n = await Deno.stdin.read(buf)
    if (n === null) break
    chunks.push(buf.slice(0, n))
  }
  const total = chunks.reduce((a, c) => a + c.length, 0)
  const out = new Uint8Array(total)
  let off = 0
  for (const c of chunks) {
    out.set(c, off)
    off += c.length
  }
  return new TextDecoder().decode(out)
}

async function onPath(bin: string): Promise<boolean> {
  for (const dir of (Deno.env.get('PATH') ?? '').split(':')) {
    if (!dir) continue
    try {
      const info = await Deno.stat(`${dir}/${bin}`)
      if (info.isFile) return true
    } catch {
      // not there — keep scanning
    }
  }
  return false
}

let input: unknown
try {
  input = JSON.parse(await readStdinAll())
} catch {
  Deno.exit(0)
}

const payload = input as Record<string, unknown>
const toolName = (payload?.tool_name ?? payload?.tool) as string | undefined
if (toolName !== 'Bash') Deno.exit(0)

if (!(await onPath('wt')) && !(await onPath('git-wt'))) Deno.exit(0)

const toolInput = payload?.tool_input as Record<string, unknown> | undefined
const command = (toolInput?.command ?? payload?.command) as string | undefined
if (!command) Deno.exit(0)

// Token scan over argv, not a substring regex: matches `git|git-wt`, optional
// -C <dir> / --git-dir / --git-common-dir flags (both `--flag <val>` and
// `--flag=val` forms), then `worktree add`. A substring regex false-positives
// on `echo "git worktree add"` and misses `git -C <dir> worktree add`; argv
// tokens get both right.
const argv = command.split(/\s+/).filter(Boolean)

let gitIdx = -1
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === 'git' || argv[i] === 'git-wt') {
    gitIdx = i
    break
  }
}
if (gitIdx < 0) Deno.exit(0)

let i = gitIdx + 1
while (i < argv.length) {
  const t = argv[i]
  if (t === '-C' || t === '--git-dir' || t === '--git-common-dir') i += 2
  else if (/^--(git-dir|git-common-dir)=.+/.test(t)) i += 1
  else break
}

if (argv[i] !== 'worktree' || argv[i + 1] !== 'add') Deno.exit(0)

await Deno.stderr.write(
  new TextEncoder().encode(`*** SYSTEM DIRECTIVE ***
STATUS: CRITICAL — git worktree add BLOCKED
ACTION: IMMEDIATE COMPLIANCE REQUIRED

FORBIDDEN: git worktree add (worktrunk is installed on PATH)

Plain \`git worktree add\` creates worktree state that worktrunk cannot see —
branches, paths, and metadata get out of sync. Use the worktrunk workflow.

Use \`wt switch --create <branch>\` instead (or \`git-wt switch --create\`).

*** END DIRECTIVE ***
`),
)
Deno.exit(2)