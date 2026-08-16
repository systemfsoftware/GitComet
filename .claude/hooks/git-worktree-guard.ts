#!/usr/bin/env -S deno run --allow-read --allow-env=PATH
// Project-level guard hook: blocks `git worktree add` while worktrunk is
// installed. Wired for Claude Code via .claude/settings.json.
//
// Claude Code PreToolUse payload (stdin, per the hooks reference):
//   { "hook_event_name": "PreToolUse", "tool_name": "Bash",
//     "tool_input": { "command": "..." }, ... }
//
// Parsing is delegated to just-bash (github.com/vercel-labs/just-bash) — a
// real bash parser. Never hand-split the command string: whitespace tokens
// miss env prefixes (GIT_DIR=x git ...), quoted subcommands (git "worktree"),
// compounds (a && git ... b), and subshells, and false-positive on strings
// inside echo/arguments. The AST gives every SimpleCommand with assignments
// already separated from the program name.
//
// Fail-open by design: when worktrunk is not installed, plain git worktree
// is fine; when a word is not statically known ($VAR, $(...)), it cannot
// match statically either way. Exit 2 blocks the tool call; stderr is the
// reason. Scopes are exact: --allow-read for the PATH stat probe,
// --allow-env=PATH to read PATH; stdin/stderr need no permission. First run
// downloads just-bash into the deno cache — pre-warm with
// `deno cache .claude/hooks/git-worktree-guard.ts`.

import { parse } from 'npm:just-bash@3.3.0'

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

// Static text of a shell word, or null when any part is not statically known
// (parameter/command/arithmetic expansion, glob, tilde, brace expansion).
function wordText(word: { parts: unknown[] } | null): string | null {
  if (!word) return null
  let out = ''
  for (const p of word.parts as Record<string, unknown>[]) {
    switch (p.type) {
      case 'Literal':
      case 'SingleQuoted':
      case 'Escaped':
        out += p.value as string
        break
      case 'DoubleQuoted': {
        const inner = wordText({ parts: p.parts as unknown[] })
        if (inner === null) return null
        out += inner
        break
      }
      default:
        return null // expansion — not statically decidable
    }
  }
  return out
}

// Does this SimpleCommand's argv run `git|git-wt [flags] worktree add`?
function isGitWorktreeAdd(cmd: Record<string, unknown>): boolean {
  const name = wordText(cmd.name as { parts: unknown[] } | null)
  if (name !== 'git' && name !== 'git-wt') return false
  const args = (cmd.args as { parts: unknown[] }[]).map((a) => wordText(a))
  let i = 0
  while (i < args.length) {
    const t = args[i]
    if (t === '-C' || t === '--git-dir' || t === '--git-common-dir') i += 2
    else if (t !== null && /^--(git-dir|git-common-dir)=.+/.test(t)) i += 1
    else break
  }
  return args[i] === 'worktree' && args[i + 1] === 'add'
}

// Deep-walk the AST for every SimpleCommand node (any compound form:
// &&/||/;/pipes, subshells, groups, if/for/while/case bodies, functions).
function anyWorktreeAdd(node: unknown): boolean {
  if (!node || typeof node !== 'object') return false
  if (Array.isArray(node)) return node.some(anyWorktreeAdd)
  const n = node as Record<string, unknown>
  if (n.type === 'SimpleCommand' && isGitWorktreeAdd(n)) return true
  return Object.values(n).some(anyWorktreeAdd)
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

let ast: unknown
try {
  ast = parse(command)
} catch {
  Deno.exit(0) // unparseable input is not ours to police
}

if (!anyWorktreeAdd(ast)) Deno.exit(0)

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