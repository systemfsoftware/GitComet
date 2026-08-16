#!/usr/bin/env bash
# Worktrunk pre-start: symlink .issues, convert gitdir paths to relative,
# disable GitKraken-incompatible settings.
# Invoked by .config/wt.toml. Args: worktree_path [primary_worktree_path]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/worktrunk/lib.sh
. "$SCRIPT_DIR/lib.sh"

WORKTREE_PATH="${1:?worktree_path required}"
PRIMARY_PATH="${2:-$(resolve_primary_repo "$WORKTREE_PATH" 2>/dev/null || true)}"

# GitKraken (libgit2) compat — runtime-guarded: one canonical content for every
# machine, skipped with a single log line where GitKraken is absent.
if gitkraken_present; then
    if [[ -n "$PRIMARY_PATH" ]]; then
        # Convert the current worktree's gitdir entry to a relative path.
        worktrees_dir="$PRIMARY_PATH/.git/worktrees"
        expected="$WORKTREE_PATH/.git"
        if [[ -d "$worktrees_dir" ]]; then
            for wt_gitdir_file in "$worktrees_dir"/*/gitdir; do
                [[ -f "$wt_gitdir_file" ]] || continue
                [[ "$(cat "$wt_gitdir_file")" == "$expected" ]] || continue
                if convert_gitdir_entry "$wt_gitdir_file"; then
                    echo "  gitdir: $(basename "$(dirname "$wt_gitdir_file")") -> $(cat "$wt_gitdir_file")"
                fi
            done
        fi
        # Convert the worktree's own .git file.
        if convert_worktree_gitfile "$WORKTREE_PATH"; then
            echo "  .git -> $(head -1 "$WORKTREE_PATH/.git")"
        fi
    fi
    # Unset extensions.relativeWorktrees (git sets it on worktree create; libgit2 chokes).
    git -C "$WORKTREE_PATH" config --unset extensions.relativeWorktrees 2>/dev/null || true
else
    echo "pre-start: GitKraken not detected — skipping compat conversions"
fi

if [[ -z "$PRIMARY_PATH" ]]; then
    echo "pre-start: running in primary repo (not a worktree) — done"
    exit 0
fi

# Symlink the .issues directory if it exists in the primary repo.
ISSUES_DIR="$PRIMARY_PATH/.issues"
WORKTREE_ISSUES="$WORKTREE_PATH/.issues"

if [[ ! -d "$ISSUES_DIR" ]]; then
    echo "pre-start: no .issues directory in primary, skipping symlink"
    exit 0
fi

# Replace only the symlink we own (handles stale and dangling links). A real
# .issues file or directory is never deleted — it may be user content.
if [[ -L "$WORKTREE_ISSUES" ]]; then
    rm -f "$WORKTREE_ISSUES"
elif [[ -e "$WORKTREE_ISSUES" ]]; then
    echo "pre-start: .issues exists and is not a symlink, leaving it alone"
    exit 0
fi

RELATIVE_ISSUES=$(realpath --relative-to="$WORKTREE_PATH" "$ISSUES_DIR")
ln -s "$RELATIVE_ISSUES" "$WORKTREE_ISSUES"
echo "pre-start: symlinked .issues -> $RELATIVE_ISSUES"