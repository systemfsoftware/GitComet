//! Watcher-exclude rules sourced from the IDE the user already configured.
//!
//! GitComet's file watcher treats git-ignore rules as its only exclude source. IDE
//! clients ship their own watcher exclusions — the prominent one being VS Code's
//! `files.watcherExclude` in `<workdir>/.vscode/settings.json` — which repos rely
//! on to keep build/output trees (`node_modules`, `target`, `dist`, …) out of file
//! watching. Respecting that same out-of-band configuration removes exactly the
//! churn the repo owner already opted out of elsewhere.
//!
//! Semantics mirror VS Code's glob rules (plan R2):
//! - `**` matches zero or more path segments; `*`/`?` match within one segment.
//! - A pattern containing no `/` matches the name at any depth.
//! - A pattern containing `/` is anchored to the repository root.
//! - A matching directory excludes its whole subtree; a trailing `/` restricts a
//!   pattern to directories.
//! - Syntax outside that set (`[...]`, `!`-prefixed patterns) never matches, and
//!   logs a trace line (only when the repo load trace is enabled), matching the
//!   malformed-file log precedent (plan T1.1).
//!
//! Conservative failure direction: any parse problem yields an empty rule set, so
//! paths stay watched (extra refreshes, never missed changes).

use std::path::{Path, PathBuf};

use super::repo_load_trace;

/// Directory and file of the VS Code settings inside the worktree.
const VSCODE_SETTINGS_DIR: &str = ".vscode";
const VSCODE_SETTINGS_FILE: &str = "settings.json";

/// One compiled `files.watcherExclude` entry.
#[derive(Debug)]
struct ExcludePattern {
    /// Compiled segments; `None` segments are `**`.
    segments: Vec<Option<CompiledSegment>>,
    /// True when the original pattern contains no `/` (multi-segment patterns
    /// are anchored by definition; a single segment matches at any depth).
    any_depth: bool,
    /// Pattern ended with a `/`, so it applies to directories only.
    dir_only: bool,
}

/// One compiled path segment.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CompiledSegment {
    /// Literal segment text with `*`/`?` resolved during matching.
    text: String,
}

impl CompiledSegment {
    fn matches(&self, segment: &str) -> bool {
        segment_glob_matches(&self.text, segment)
    }
}

/// `fnmatch`-style single-segment matcher for `*` (any run) and `?` (one char).
/// No other syntax reaches it: unsupported patterns are dropped at parse time.
fn segment_glob_matches(glob: &str, value: &str) -> bool {
    let glob = glob.as_bytes();
    let value = value.as_bytes();
    let (mut g, mut v) = (0usize, 0usize);
    let (mut star_g, mut star_v) = (usize::MAX, 0usize);
    while v < value.len() {
        if g < glob.len() && (glob[g] == b'?' || glob[g] == value[v]) {
            g += 1;
            v += 1;
        } else if g < glob.len() && glob[g] == b'*' {
            star_g = g;
            g += 1;
            star_v = v;
        } else if star_g != usize::MAX {
            g = star_g + 1;
            star_v += 1;
            v = star_v;
        } else {
            return false;
        }
    }
    while g < glob.len() && glob[g] == b'*' {
        g += 1;
    }
    g == glob.len()
}

/// True when the entry glob uses syntax the matcher does not support. Such
/// entries never match (conservative: the path stays watched).
fn has_unsupported_syntax(glob: &str) -> bool {
    glob.starts_with('!') || glob.contains(['[', ']'])
}

/// Whether `pattern` segments match `path` segments, treating `None` (`**`) as
/// zero-or-more segments and everything else as one segment.
fn path_matches(pattern: &[Option<CompiledSegment>], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((None, rest)) => {
            // `**` matches zero segments...
            if path_matches(rest, path) {
                return true;
            }
            // ...or consumes one segment and stays.
            !path.is_empty() && path_matches(pattern, &path[1..])
        }
        Some((Some(segment), rest)) => {
            !path.is_empty() && segment.matches(path[0]) && path_matches(rest, &path[1..])
        }
    }
}

/// For a pattern ending in `**` (e.g. `dist/**`), the trimmed prefix (`dist`)
/// also matches per VS Code's zero-segment `**`. Returns that prefix.
fn zero_segment_trim(pattern: &[Option<CompiledSegment>]) -> Option<&[Option<CompiledSegment>]> {
    match pattern.split_last() {
        Some((None, rest)) if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// POSIX-style segments of a worktree-relative path. Non-UTF-8 components
/// become empty strings, which never match a real pattern.
fn path_segments(rel: &Path) -> Vec<&str> {
    rel.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_str().unwrap_or_default()),
            _ => None,
        })
        .collect()
}

/// Segment lists for `rel` and every ancestor directory, in
/// [self, parent, grandparent, …] order. Computed once per exclusion check so
/// the hot per-event path does not re-allocate per pattern.
fn ancestor_segment_lists(rel: &Path) -> Vec<Vec<&str>> {
    let mut lists = Vec::new();
    let mut candidate: Option<&Path> = Some(rel);
    while let Some(current) = candidate {
        lists.push(path_segments(current));
        candidate = current.parent();
    }
    lists
}

/// Whether `pattern` excludes `rel` — the path itself or any ancestor
/// directory (a matched directory excludes its whole subtree).
fn pattern_excludes(
    pattern: &ExcludePattern,
    candidates: &[Vec<&str>],
    is_dir_hint: Option<bool>,
) -> bool {
    for (index, segments) in candidates.iter().enumerate() {
        // `index == 0` is the event path itself; the rest are ancestor
        // directories, which are directories by construction. A directory-only
        // pattern can therefore never match against the event path when the
        // hint says it is a file — but it can match the ancestors, whose
        // subtree exclusion covers the file (plan R2, finding 4).
        let is_self = index == 0;
        if pattern.any_depth {
            // No `/` in the pattern: it is a single segment that matches any
            // depth, and an intermediate match excludes the subtree. (A
            // directory-only pattern always carries a trailing `/`, which
            // anchors it — so `dir_only` cannot combine with `any_depth`.)
            let Some(segment) = pattern.segments.first().and_then(|s| s.as_ref()) else {
                // Bare `**`: zero or more segments at any depth — it matches
                // every path (a matching directory excludes its subtree).
                return true;
            };
            if segments.iter().any(|part| segment.matches(part)) {
                return true;
            }
        } else if path_matches(&pattern.segments, segments) {
            if pattern.dir_only && is_dir_hint == Some(false) && is_self {
                // The pattern covers the whole event path, which is a file:
                // a directory-only pattern does not apply to it.
                continue;
            }
            return true;
        } else if is_dir_hint != Some(false)
            && let Some(trimmed) = zero_segment_trim(&pattern.segments)
            && path_matches(trimmed, segments)
        {
            // `dist/**` excludes `dist` itself (zero-segment `**` in the
            // trailing position). A file hint is not enough for the trimmed
            // prefix alone.
            return true;
        }
    }
    false
}

/// The watcher-exclude rule set for one repository worktree.
///
/// Immutable after load; the monitor reloads it when the config file changes.
#[derive(Debug, Default)]
pub(crate) struct WatcherExcludes {
    enabled: bool,
    patterns: Vec<ExcludePattern>,
}

impl WatcherExcludes {
    /// Loads the exclude rules for `workdir`.
    ///
    /// `enabled` gates the whole mechanism: a disabled rule set is empty and
    /// never reads the config file.
    pub(crate) fn load(workdir: &Path, enabled: bool) -> Self {
        let patterns = if enabled {
            parse_vscode_watcher_exclude(workdir).unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { enabled, patterns }
    }

    /// Path of the config file this rule set reads from.
    pub(crate) fn config_path(workdir: &Path) -> PathBuf {
        workdir.join(VSCODE_SETTINGS_DIR).join(VSCODE_SETTINGS_FILE)
    }

    /// Whether the rule set is active (the monitor was started with the setting
    /// enabled); used to re-load with the same setting after a config change.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns `true` when `rel` (worktree-relative) falls under an exclude
    /// rule. `is_dir_hint` disambiguates directory-only patterns
    /// (`Some(false)` = definitely a file).
    pub(crate) fn is_excluded(&self, rel: &Path, is_dir_hint: Option<bool>) -> bool {
        if !self.enabled {
            return false;
        }
        // Plan R4 / T2.4: the config file must always be able to reload itself,
        // so nothing at or under `.vscode` is ever excluded.
        let first = rel.components().next();
        if matches!(
            first,
            Some(std::path::Component::Normal(part))
                if part == std::ffi::OsStr::new(VSCODE_SETTINGS_DIR)
        ) {
            return false;
        }
        let candidates = ancestor_segment_lists(rel);
        self.patterns
            .iter()
            .any(|pattern| pattern_excludes(pattern, &candidates, is_dir_hint))
    }
}

/// Parses `files.watcherExclude` from `<workdir>/.vscode/settings.json`.
///
/// Returns `None` on any structural problem (missing file, invalid JSON, wrong
/// types) — the caller treats that as an empty rule set; diagnosability comes
/// from the trace lines (plan T1.1).
fn parse_vscode_watcher_exclude(workdir: &Path) -> Option<Vec<ExcludePattern>> {
    let path = WatcherExcludes::config_path(workdir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            repo_load_trace::trace!(
                "watcher_excludes: {} not found — treating as empty",
                path.display()
            );
            return Some(Vec::new());
        }
        Err(error) => {
            repo_load_trace::trace!(
                "watcher_excludes: could not read {}: {error} — treating as empty",
                path.display()
            );
            return Some(Vec::new());
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            repo_load_trace::trace!(
                "watcher_excludes: could not parse {}: {error} — treating as empty",
                path.display()
            );
            return Some(Vec::new());
        }
    };
    let Some(excludes) = value
        .get("files.watcherExclude")
        .and_then(serde_json::Value::as_object)
    else {
        repo_load_trace::trace!(
            "watcher_excludes: {} has no `files.watcherExclude` object — treating as empty",
            path.display()
        );
        return Some(Vec::new());
    };

    let mut patterns = Vec::new();
    for (glob, include) in excludes {
        if include.as_bool() != Some(true) {
            // `false` is an explicit non-exclude; non-booleans are ignored.
            continue;
        }
        // A trailing `/` marks a directory-only pattern; strip it first. The
        // anchoring decision uses the ORIGINAL glob: a trailing slash implies a
        // slash, so an anchored (root-relative) pattern.
        let (glob, dir_only) = match glob.strip_suffix('/') {
            Some(trimmed) => (trimmed, true),
            None => (glob.as_str(), false),
        };
        if has_unsupported_syntax(glob) {
            repo_load_trace::trace!(
                "watcher_excludes: pattern {:?} in {} uses unsupported syntax and is ignored",
                glob,
                path.display()
            );
            continue;
        }
        let segments = split_segments(glob);
        let any_depth = !glob_contains_slash(glob, dir_only);
        if segments.is_empty()
            || segments
                .iter()
                .all(|segment| segment.as_ref().is_some_and(|c| c.text.is_empty()))
        {
            // `"/"` (or a run of slashes) compiles to empty literal
            // segments, which never match a real path component.
            repo_load_trace::trace!(
                "watcher_excludes: pattern {:?} in {} is empty and is ignored",
                glob,
                path.display()
            );
            continue;
        }
        patterns.push(ExcludePattern {
            segments,
            any_depth,
            dir_only,
        });
    }
    Some(patterns)
}

/// Whether the original pattern contains a slash (after stripping the trailing
/// dir-only marker, which itself implies an anchor).
fn glob_contains_slash(glob: &str, had_trailing_slash: bool) -> bool {
    had_trailing_slash || glob.contains('/')
}

fn split_segments(glob: &str) -> Vec<Option<CompiledSegment>> {
    // Consecutive `**` are collapsed: zero-or-more segments is idempotent, so
    // `**/**` behaves identically to `**` while keeping matching linear in the
    // number of segments (finding 9).
    let mut segments = Vec::new();
    let mut prev_globstar = false;
    for segment in glob.split('/') {
        if segment == "**" {
            if !prev_globstar {
                segments.push(None);
                prev_globstar = true;
            }
        } else {
            segments.push(Some(CompiledSegment {
                text: segment.to_string(),
            }));
            prev_globstar = false;
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_settings(workdir: &Path, contents: &str) {
        fs::create_dir_all(workdir.join(".vscode")).expect("mkdir .vscode");
        fs::write(WatcherExcludes::config_path(workdir), contents).expect("write settings.json");
    }

    fn temp_workdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("gitcomet-watcher-excludes-test")
            .tempdir()
            .expect("create tempdir")
    }

    fn rel(path: &str) -> &Path {
        Path::new(path)
    }

    #[test]
    fn missing_file_yields_empty_rules() {
        let dir = temp_workdir();
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("src"), Some(true)));
        assert!(!excludes.is_excluded(rel("node_modules"), Some(true)));
    }

    #[test]
    fn empty_settings_yield_empty_rules() {
        let dir = temp_workdir();
        write_settings(dir.path(), "");
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("node_modules"), Some(true)));
    }

    #[test]
    fn invalid_json_yields_empty_rules() {
        let dir = temp_workdir();
        write_settings(dir.path(), "{ not json");
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("node_modules"), Some(true)));
    }

    #[test]
    fn non_object_watcher_exclude_yields_empty_rules() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": 42}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("node_modules"), Some(true)));
    }

    #[test]
    fn only_true_entries_are_excludes() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"node_modules": true, "dist": false, "vendor": "yes"}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("node_modules"), Some(true)));
        assert!(!excludes.is_excluded(rel("dist"), Some(true)));
        assert!(!excludes.is_excluded(rel("vendor"), Some(true)));
    }

    #[test]
    fn anchored_pattern_with_trailing_slash() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"repos/": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("repos"), Some(true)));
        assert!(excludes.is_excluded(rel("repos/x"), None));
        assert!(excludes.is_excluded(rel("repos/a/b"), None));
        // A sibling under a different root is not excluded (anchored).
        assert!(!excludes.is_excluded(rel("src/repos"), Some(true)));
        // Trailing slash = directories only.
        assert!(!excludes.is_excluded(rel("repos"), Some(false)));
    }

    #[test]
    fn pattern_without_slash_matches_any_depth() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"repos": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("repos"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/repos"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/repos/deep"), None));
    }

    #[test]
    fn double_star_matches_any_depth() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"**/node_modules": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("x/node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/node_modules/package"), None));
    }

    #[test]
    fn double_star_trailing_matches_zero_segments() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"**/node_modules/**": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("x/node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("x/node_modules/lib"), None));
        assert!(!excludes.is_excluded(rel("lib"), Some(true)));
        assert!(!excludes.is_excluded(rel("x/lib"), Some(true)));
    }

    #[test]
    fn dist_double_star_matches_dist_itself() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"dist/**": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("dist"), Some(true)));
        assert!(excludes.is_excluded(rel("dist/out.js"), None));
        assert!(!excludes.is_excluded(rel("dist2"), Some(true)));
        assert!(!excludes.is_excluded(rel("src/dist"), Some(true)));
    }

    #[test]
    fn question_mark_and_star_match_within_segments() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"cache?": true, "*.min.js": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("cache1"), Some(true)));
        assert!(!excludes.is_excluded(rel("cache12"), Some(true)));
        assert!(excludes.is_excluded(rel("app.min.js"), Some(false)));
        assert!(!excludes.is_excluded(rel("app.source.js"), Some(false)));
    }

    #[test]
    fn unsupported_syntax_never_matches() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"[abc]/": true, "!keep": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("a"), Some(true)));
        assert!(!excludes.is_excluded(rel("keep"), Some(true)));
    }

    #[test]
    fn unicode_and_space_paths() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"выкладка/": true, "build dir/": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("выкладка"), Some(true)));
        assert!(excludes.is_excluded(rel("build dir"), Some(true)));
        assert!(excludes.is_excluded(rel("выкладка/файл"), None));
    }

    #[test]
    fn disabled_is_empty_even_with_rules() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"node_modules": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), false);
        assert!(!excludes.is_excluded(rel("node_modules"), Some(true)));
    }

    #[test]
    fn vscode_directory_is_never_excluded() {
        let dir = temp_workdir();
        // A pattern that would otherwise swallow `.vscode` itself.
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"**/.vscode": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel(".vscode"), Some(true)));
        assert!(!excludes.is_excluded(rel(".vscode/settings.json"), Some(false)));
        // Other dirs matching the pattern still are.
        assert!(excludes.is_excluded(rel("a/.vscode"), Some(true)));
    }

    #[test]
    fn ancestor_directory_match_excludes_subtree() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"target": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("target"), Some(true)));
        assert!(excludes.is_excluded(rel("target/debug"), Some(true)));
        assert!(excludes.is_excluded(rel("target/debug/deps/foo"), None));
    }

    #[test]
    fn dir_only_pattern_excludes_file_events_below_the_directory() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"repos/": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        // The excluded directory's own subtree is excluded even when the
        // event hint says the event path is a file (review finding 4).
        assert!(excludes.is_excluded(rel("repos/x.txt"), Some(false)));
        assert!(excludes.is_excluded(rel("repos/a/b/file.rs"), Some(false)));
        // But a FILE named like the directory is not the directory.
        assert!(!excludes.is_excluded(rel("repos"), Some(false)));
    }

    #[test]
    fn bare_double_star_excludes_everything() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"**": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("anything"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/c/file.rs"), Some(false)));
        // The `.vscode` carve-out still wins for the config file itself.
        assert!(!excludes.is_excluded(rel(".vscode/settings.json"), Some(false)));
    }

    #[test]
    fn consecutive_double_stars_collapse() {
        let dir = temp_workdir();
        write_settings(
            dir.path(),
            r#"{"files.watcherExclude": {"**/**/**/node_modules": true}}"#,
        );
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(excludes.is_excluded(rel("node_modules"), Some(true)));
        assert!(excludes.is_excluded(rel("a/b/c/node_modules/package"), Some(false)));
        assert!(!excludes.is_excluded(rel("a/b/c/lib"), Some(true)));
    }

    #[test]
    fn bare_slash_pattern_is_ignored() {
        let dir = temp_workdir();
        write_settings(dir.path(), r#"{"files.watcherExclude": {"/": true}}"#);
        let excludes = WatcherExcludes::load(dir.path(), true);
        assert!(!excludes.is_excluded(rel("anything"), Some(true)));
    }
}
