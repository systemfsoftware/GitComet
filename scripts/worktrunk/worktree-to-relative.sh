#!/usr/bin/env bash
# Convert all worktree links to relative paths.
# Run from the main repo when worktrees are accessible.
# GitKraken (libgit2) may not support extensions.relativeWorktrees.
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/worktrunk/lib.sh
. "$SCRIPT_DIR/lib.sh"

REPO_ROOT="${1:-$(git rev-parse --show-toplevel)}"
cd "$REPO_ROOT"

converted=0

# 1. Convert main repo's .git/worktrees/*/gitdir to relative
for wt_gitdir in "$REPO_ROOT"/.git/worktrees/*/gitdir; do
    if convert_gitdir_entry "$wt_gitdir"; then
        echo "worktree-to-relative: $(basename "$(dirname "$wt_gitdir")") gitdir -> $(cat "$wt_gitdir")"
        ((converted++)) || true
    fi
done

# 2. Convert each worktree's .git file to relative (GitKraken-driven; no-op without it)
if ! gitkraken_present; then
    echo "worktree-to-relative: GitKraken not detected — skipping .git file conversion"
    [[ $converted -gt 0 ]] && echo "worktree-to-relative: converted $converted gitdir entries"
    exit 0
fi

while IFS= read -r wt_path; do
    [[ "$wt_path" == "$REPO_ROOT" ]] && continue
    if convert_worktree_gitfile "$wt_path"; then
        echo "worktree-to-relative: $(basename "$wt_path") .git -> $(head -1 "$wt_path/.git")"
        ((converted++)) || true
    fi
done < <(git worktree list --porcelain | awk '/^worktree / {print $2}')

[[ $converted -gt 0 ]] || echo "worktree-to-relative: all paths already relative"