#!/usr/bin/env bash
# Worktrunk post-start: generate all build artifacts (background).
# Invoked by .config/wt.toml. Arg: worktree_path
#
# A fresh worktree has none of the gitignored generated files that checks
# depend on. The build produces them so tests and checks are green out of the
# box. This must run AFTER install-deps.sh.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/worktrunk/lib.sh
. "$SCRIPT_DIR/lib.sh"   # loud_fail / find_tool — this script runs in background

WORKTREE_PATH="${1:?worktree_path required}"

echo "generate-artifacts: generating build artifacts..."
cd "$WORKTREE_PATH"

# Detect build tool, resolve its binary, run the build.
MANAGER="$(detect_manager || true)"
case "$MANAGER" in
    corepack)
        command -v corepack >/dev/null 2>&1 || loud_fail corepack
        corepack pnpm build
        ;;
    npm)
        command -v npm >/dev/null 2>&1 || loud_fail npm
        npm run build
        ;;
    yarn)
        command -v yarn >/dev/null 2>&1 || loud_fail yarn
        yarn build
        ;;
    bun)
        command -v bun >/dev/null 2>&1 || loud_fail bun
        bun run build
        ;;
    cargo)
        CARGO_BIN="$(find_tool cargo)" || loud_fail cargo
        "$CARGO_BIN" build --release || "$CARGO_BIN" build
        ;;
    "")
        if [[ -f "Makefile" ]]; then
            command -v make >/dev/null 2>&1 || loud_fail make
            make build || make all
        else
            echo "generate-artifacts: no recognized build system, skipping"
        fi
        ;;
    *)
        loud_fail "$MANAGER"
        ;;
esac

echo "generate-artifacts: done"
