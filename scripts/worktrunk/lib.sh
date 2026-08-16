#!/usr/bin/env bash
# Shared helpers for Worktrunk hook scripts.
# Source from scripts/worktrunk/*.sh

# Returns 0 (prints primary path to stdout) if worktree; 1 if main repo.
resolve_primary_repo() {
    local worktree_root="$1"
    local git_dir git_common_dir
    git_dir="$(cd "$worktree_root" && git rev-parse --git-dir)"
    git_common_dir="$(cd "$worktree_root" && git rev-parse --git-common-dir)"
    if [[ "$git_dir" == "$git_common_dir" ]]; then
        return 1
    fi
    (cd "$worktree_root" && cd "$git_common_dir/.." && pwd)
}

# Resolve a build tool: PATH, then env homes, then common roots (SH1 ladder).
# For ephemeral HOME (overlay), also checks /home/*/.cargo/bin for persistent toolchains.
# Prints the path on stdout; returns 1 when not found.
find_tool() {
    local tool="$1" found roots r c
    if found=$(command -v "$tool" 2>/dev/null); then
        printf '%s' "$found"; return 0
    fi
    roots=("${CARGO_HOME:-}/bin" "${RUSTUP_HOME:-}/bin" "$HOME/.cargo/bin" \
           /usr/local/cargo/bin /opt/cargo/bin /opt/homebrew/bin /usr/local/go/bin)
    for r in "${roots[@]}"; do
        [[ -n "$r" && -x "$r/$tool" ]] && { printf '%s' "$r/$tool"; return 0; }
    done
    if [[ "$tool" == cargo ]]; then
        for c in /home/*/.cargo/bin/cargo; do  # persistent homes when $HOME is ephemeral
            [[ -x "$c" ]] && { printf '%s' "$c"; return 0; }
        done
    fi
    return 1
}

# Fail loudly: post-start runs in background, so a missing tool MUST be
# visible (non-zero exit + remediation), never a silent "command not found".
loud_fail() {
    echo "ERROR: required tool '$1' not found (PATH, \$CARGO_HOME, common roots)." >&2
    echo "       fix: install $1, or extend find_tool() roots, then re-run this hook." >&2
    exit 1
}

# GitKraken presence — gates the libgit2 compatibility conversions (SH1).
gitkraken_present() {
    command -v gitkraken >/dev/null 2>&1 && return 0
    command -v gitkraken-cli >/dev/null 2>&1 && return 0
    [[ -d "$HOME/.config/GitKraken" || -d "$HOME/.gitkraken" ]]
}

# Detect the package manager from lockfiles present in the current directory.
# Echoes the manager command ("corepack", "npm", "yarn", "bun", "cargo");
# returns 1 and echoes nothing when no recognized lockfile exists.
detect_manager() {
    if [[ -f "pnpm-lock.yaml" ]]; then
        printf '%s' corepack
    elif [[ -f "package-lock.json" ]]; then
        printf '%s' npm
    elif [[ -f "yarn.lock" ]]; then
        printf '%s' yarn
    elif [[ -f "bun.lock" ]]; then
        printf '%s' bun
    elif [[ -f "Cargo.toml" ]]; then
        printf '%s' cargo
    else
        return 1
    fi
}
