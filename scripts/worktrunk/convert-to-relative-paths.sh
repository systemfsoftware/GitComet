#!/usr/bin/env bash
# Convert all existing worktrees under a shared root to use relative paths.
# This updates .git files without setting extensions.relativeWorktrees

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/worktrunk/lib.sh
. "$SCRIPT_DIR/lib.sh"

WORKTREES_ROOT="${1:-/mnt/shared/.worktrees}"
MAIN_REPO="${2:-/mnt/shared}"

convert_worktree_to_relative() {
    local worktree_path="$1"
    local git_file="$worktree_path/.git"

    [[ ! -f "$git_file" ]] && {
        echo "convert-to-relative: no .git file in $worktree_path"
        return 1
    }

    local gitdir_line
    gitdir_line=$(head -1 "$git_file")
    [[ "$gitdir_line" != gitdir:* ]] && {
        echo "convert-to-relative: invalid .git file format in $worktree_path"
        return 1
    }

    local abs_path="${gitdir_line#gitdir: }"
    [[ "$abs_path" != /* ]] && {
        echo "convert-to-relative: already relative: $(basename "$worktree_path")"
        return 0
    }

    local rel_path
    if ! rel_path=$(realpath --relative-to="$worktree_path" "$abs_path" 2>/dev/null); then
        echo "convert-to-relative: failed to compute relative path for $worktree_path"
        return 1
    fi

    echo "gitdir: $rel_path" > "$git_file"
    echo "convert-to-relative: $(basename "$worktree_path") -> $rel_path"
}

echo "Converting worktrees to relative paths..."
echo "Worktrees root: $WORKTREES_ROOT"
echo ""

converted=0
failed=0
skipped=0

for worktree_dir in "$WORKTREES_ROOT"/*/; do
    [[ -d "$worktree_dir" ]] || continue
    [[ "$(basename "$worktree_dir")" == ".gitkeep" ]] && continue

    worktree_name=$(basename "$worktree_dir")
    echo "[$worktree_name]"

    if convert_worktree_to_relative "$worktree_dir"; then
        ((converted++))
    else
        ((failed++))
    fi
    echo ""
done

echo "=============================="
echo "Converted: $converted"
echo "Failed: $failed"
echo "=============================="

exit $((failed > 0 ? 1 : 0))
