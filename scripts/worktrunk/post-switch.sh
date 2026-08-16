#!/usr/bin/env bash
# Worktrunk post-switch: disable GitKraken-incompatible settings.
# Invoked by .config/wt.toml. Arg: worktree_path

set -e

WORKTREE_PATH="${1:?worktree_path required}"

# Unset extensions.relativeWorktrees so GitKraken (libgit2) can open the repo.
# No-op when unset. Git may re-set this when switching worktrees.
git -C "$WORKTREE_PATH" config --unset extensions.relativeWorktrees 2>/dev/null || true

echo "post-switch: done ($WORKTREE_PATH)"
