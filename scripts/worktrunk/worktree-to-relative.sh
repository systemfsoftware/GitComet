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
for wt_gitdir in .git/worktrees/*/gitdir; do
  [[ -f "$wt_gitdir" ]] || continue
  target=$(cat "$wt_gitdir")
  [[ "$target" == /* ]] || continue

  base_dir=$(dirname "$wt_gitdir")
  rel_path=$(realpath --relative-to="$base_dir" "$target" 2>/dev/null) || continue
  echo "$rel_path" > "$wt_gitdir"
  echo "worktree-to-relative: $(basename "$base_dir") gitdir -> $rel_path"
  ((converted++)) || true
done

# 2. Convert each worktree's .git file to relative (GitKraken-driven; no-op without it)
if ! gitkraken_present; then
  echo "worktree-to-relative: GitKraken not detected — skipping .git file conversion"
  [[ $converted -gt 0 ]] && echo "worktree-to-relative: converted $converted gitdir entries"
  exit 0
fi

for wt_path in $(git worktree list --porcelain | awk '/^worktree / {print $2}'); do
  [[ "$wt_path" == "$REPO_ROOT" ]] && continue
  [[ -f "$wt_path/.git" ]] || { echo "worktree-to-relative: skipping $wt_path (not accessible)" >&2; continue; }

  gitdir_line=$(head -1 "$wt_path/.git")
  [[ "$gitdir_line" == gitdir:* ]] || continue
  abs_path="${gitdir_line#gitdir: }"
  [[ "$abs_path" == /* ]] || continue

  rel_path=$(realpath --relative-to="$wt_path" "$abs_path" 2>/dev/null) || continue
  echo "gitdir: $rel_path" > "$wt_path/.git"
  echo "worktree-to-relative: $(basename "$wt_path") .git -> $rel_path"
  ((converted++)) || true
done

[[ $converted -gt 0 ]] || echo "worktree-to-relative: all paths already relative"
