#!/usr/bin/env bash
# Worktrunk post-start: install dependencies (background).
# Invoked by .config/wt.toml. Arg: worktree_path
# Resolves the manager's tool via lib.sh find_tool(); a missing tool fails
# LOUD (non-zero + remediation) — this runs in background, silence is death.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/worktrunk/lib.sh
. "$SCRIPT_DIR/lib.sh"

WORKTREE_PATH="${1:?worktree_path required}"

echo "install-deps: installing dependencies in worktree..."
cd "$WORKTREE_PATH"

# Detect package manager, resolve its tool, run install.
if [[ -f "pnpm-lock.yaml" ]]; then
    command -v corepack >/dev/null 2>&1 || loud_fail corepack
    corepack pnpm install --frozen-lockfile
elif [[ -f "package-lock.json" ]]; then
    command -v npm >/dev/null 2>&1 || loud_fail npm
    npm ci
elif [[ -f "yarn.lock" ]]; then
    command -v yarn >/dev/null 2>&1 || loud_fail yarn
    yarn install --frozen-lockfile
elif [[ -f "bun.lock" ]]; then
    command -v bun >/dev/null 2>&1 || loud_fail bun
    bun install --frozen-lockfile
elif [[ -f "Cargo.toml" ]]; then
    CARGO_BIN="$(find_tool cargo)" || loud_fail cargo
    "$CARGO_BIN" build
elif [[ -f "go.mod" ]]; then
    GO_BIN="$(find_tool go)" || loud_fail go
    "$GO_BIN" mod download
elif [[ -f "Gemfile" ]]; then
    command -v bundle >/dev/null 2>&1 || loud_fail bundle
    bundle install
elif [[ -f "pyproject.toml" || -f "requirements.txt" ]]; then
    command -v pip >/dev/null 2>&1 || loud_fail pip
    pip install -e . 2>/dev/null || pip install -r requirements.txt
else
    echo "install-deps: no recognized package manager, skipping"
fi

echo "install-deps: done"
