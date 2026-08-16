use super::mergetool_builtin::{
    BuiltinMergeCommand, MergetoolFiles, builtin_merge_command, builtin_tool_program,
};
use super::{GixRepo, conflict_stages::gix_index_stage_blob_bytes_optional};
use crate::util::{bytes_to_text_preserving_utf8, run_git_simple};
use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::path_utils::canonicalize_or_original;
use gitcomet_core::process::background_command as no_window_command;
use gitcomet_core::services::{
    CommandOutput, MergetoolResult, Result, validate_conflict_resolution_text,
};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
#[cfg(any(not(windows), test))]
use std::process::Command;
use std::sync::{Mutex, OnceLock};

impl GixRepo {
    /// Launch an external mergetool for a conflicted file.
    ///
    /// The implementation:
    /// 1. Reads `merge.tool` from git config to determine the tool name.
    ///    Repository-local `mergetool.<tool>.cmd` is blocked by default unless
    ///    explicitly trusted via a GitComet global consent key.
    /// 2. Extracts conflict stages (`:1:`, `:2:`, `:3:`) into temp files.
    /// 3. Invokes the tool with the BASE, LOCAL, REMOTE and MERGED files, using
    ///    git's built-in argument convention for the tool (see
    ///    [`super::mergetool_builtin`]) so it opens in merge mode rather than as
    ///    a read-only diff.
    /// 4. Reads trust-exit config to decide success semantics:
    ///    `mergetool.<tool>.trustExitCode`, then `mergetool.trustExitCode`.
    /// 5. Reads back the merged file and stages it on success.
    pub(super) fn launch_mergetool_impl(&self, path: &Path) -> Result<MergetoolResult> {
        // `path` originates from index entries (path_buf_from_git_bytes), so a
        // hostile repository controls it completely; reject anything that could
        // escape the worktree before it reaches stage/merged paths.
        let conflict_path = sanitize_conflict_path_for_worktree(path)?;
        let workdir = &self.spec.workdir;
        let repo = self.reopen_repo()?;
        let MergetoolConfig {
            tool_name,
            tool_cmd,
            tool_path,
            trust_exit_code,
            write_to_temp,
            keep_temporaries,
        } = resolve_mergetool_config(&repo, env_has_display())?;
        let stage_paths = materialize_mergetool_stage_files(
            &repo,
            workdir,
            &conflict_path,
            write_to_temp,
            keep_temporaries,
        )?;

        let base_path = &stage_paths.base;
        let local_path = &stage_paths.local;
        let remote_path = &stage_paths.remote;
        let merged_path = workdir.join(&conflict_path);

        // 4. Snapshot merged contents before tool invocation so we can
        //    detect actual content changes when trustExitCode is false.
        let pre_merged_state = if trust_exit_code {
            None
        } else {
            Some(read_merged_file_state(&merged_path)?)
        };

        // Build and invoke the mergetool command
        let output = if let Some(ref custom_cmd) = tool_cmd {
            run_custom_mergetool_command(
                custom_cmd,
                workdir,
                base_path,
                local_path,
                remote_path,
                &merged_path,
            )?
        } else {
            // No custom command — use the argument convention git's built-in
            // definition for this tool would use. Passing bare positional paths
            // leaves tools such as KDiff3 in read-only 3-way diff mode because
            // nothing tells them where to write the merge result.
            // `mergetool.<tool>.path` wins; otherwise a few built-ins are
            // invoked through a command that is not the tool name (`vscode` runs
            // `code`, `bc` runs `bcomp`, ...), exactly as git translates them.
            let builtin_program = match tool_path {
                Some(_) => None,
                None => builtin_tool_program(&tool_name),
            };
            let tool_executable = tool_path
                .as_deref()
                .or(builtin_program.as_deref())
                .unwrap_or(&tool_name);
            let args = match builtin_merge_command(
                &tool_name,
                &MergetoolFiles {
                    base: base_path,
                    local: local_path,
                    remote: remote_path,
                    merged: &merged_path,
                    merged_label: path,
                    base_present: stage_paths.base_present,
                },
            ) {
                BuiltinMergeCommand::Args(args) => args,
                BuiltinMergeCommand::Unsupported(message) => {
                    return Err(Error::new(ErrorKind::Backend(message)));
                }
                // Not a git built-in: keep the generic convention, which is what
                // a tool configured only through `mergetool.<tool>.path` gets.
                BuiltinMergeCommand::Unknown => vec![
                    local_path.into(),
                    base_path.into(),
                    remote_path.into(),
                    merged_path.clone().into_os_string(),
                ],
            };

            no_window_command(tool_executable)
                .args(&args)
                .current_dir(workdir)
                .output()
                .map_err(|e| {
                    Error::new(ErrorKind::Backend(format!(
                        "Failed to launch mergetool '{tool_name}' ({tool_executable}): {e}"
                    )))
                })?
        };

        let stdout = bytes_to_text_preserving_utf8(&output.stdout);
        let stderr = bytes_to_text_preserving_utf8(&output.stderr);
        let exit_code = output.status.code();

        let cmd_output = CommandOutput {
            command: format!("mergetool ({tool_name})"),
            stdout,
            stderr,
            exit_code,
        };

        // 5. Determine success
        let post_merged_state = read_merged_file_state(&merged_path)?;
        let tool_success = if trust_exit_code {
            output.status.success()
        } else {
            // When trustExitCode is false (default), require an actual
            // merged-output delta (bytes change or file deletion/creation).
            pre_merged_state.as_ref() != Some(&post_merged_state)
        };

        if !tool_success {
            return Ok(MergetoolResult {
                tool_name,
                success: false,
                merged_contents: None,
                output: cmd_output,
            });
        }

        // 6. Stage tool output. For deleted output, stage deletion instead
        // of reading/staging file contents.
        let merged_contents = match post_merged_state {
            MergedFileState::Present(bytes) => {
                // Validate textual merged output and refuse staging if conflict
                // markers are still present.
                if let Ok(merged_text) = std::str::from_utf8(&bytes) {
                    let validation = validate_conflict_resolution_text(merged_text);
                    if validation.has_conflict_markers {
                        return Err(Error::new(ErrorKind::Backend(format!(
                            "Mergetool '{tool_name}' left unresolved conflict markers in {} ({} marker lines); refusing to stage",
                            path.display(),
                            validation.marker_lines
                        ))));
                    }
                }

                // Stage the file
                let mut add = self.git_workdir_cmd();
                add.arg("add").arg("--").arg(path);
                run_git_simple(add, "git add (after mergetool)")?;

                Some(bytes)
            }
            MergedFileState::Missing => {
                let mut rm = self.git_workdir_cmd();
                rm.arg("rm").arg("--").arg(path);
                run_git_simple(rm, "git rm (after mergetool)")?;
                None
            }
        };

        Ok(MergetoolResult {
            tool_name,
            success: true,
            merged_contents,
            output: cmd_output,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuiDefault {
    False,
    True,
    Auto,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MergetoolConfig {
    tool_name: String,
    tool_cmd: Option<String>,
    tool_path: Option<String>,
    trust_exit_code: bool,
    write_to_temp: bool,
    keep_temporaries: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitConfigScope {
    Any,
    Global,
    Local,
}

fn env_has_display() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(all(windows, test))]
fn shell_command(custom_cmd: &str) -> Command {
    let mut command = no_window_command("cmd");
    command.arg("/C").arg(custom_cmd);
    command
}

#[cfg(not(windows))]
fn shell_command(custom_cmd: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(custom_cmd);
    command
}

fn run_custom_mergetool_command(
    custom_cmd: &str,
    workdir: &Path,
    base_path: &Path,
    local_path: &Path,
    remote_path: &Path,
    merged_path: &Path,
) -> Result<std::process::Output> {
    #[cfg(windows)]
    {
        let script_dir = tempfile::Builder::new()
            .prefix("gitcomet-mergetool-shell-")
            .tempdir()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;
        let script_path = script_dir.path().join("run-mergetool.cmd");
        let mut script = String::from("@echo off\r\n");
        script.push_str(custom_cmd);
        script.push_str("\r\n");
        std::fs::write(&script_path, script).map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;

        let mut command = no_window_command("cmd");
        command.arg("/C").arg(&script_path);
        command
            .env("BASE", base_path)
            .env("LOCAL", local_path)
            .env("REMOTE", remote_path)
            .env("MERGED", merged_path)
            .current_dir(workdir);
        command
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))
    }
    #[cfg(not(windows))]
    {
        let mut command = shell_command(custom_cmd);
        command
            .env("BASE", base_path)
            .env("LOCAL", local_path)
            .env("REMOTE", remote_path)
            .env("MERGED", merged_path)
            .current_dir(workdir);
        command
            .output()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))
    }
}

fn parse_gui_default(value: Option<&str>) -> Result<GuiDefault> {
    let Some(value) = value else {
        return Ok(GuiDefault::False);
    };

    if value.eq_ignore_ascii_case("auto") {
        return Ok(GuiDefault::Auto);
    }

    match parse_git_bool(value) {
        Some(true) => Ok(GuiDefault::True),
        Some(false) => Ok(GuiDefault::False),
        None => Err(Error::new(ErrorKind::Backend(format!(
            "Invalid value for mergetool.guiDefault: {:?}. Expected true/false or auto.",
            value
        )))),
    }
}

fn choose_mergetool_name(
    merge_tool: Option<String>,
    merge_guitool: Option<String>,
    gui_default: GuiDefault,
    has_display: bool,
) -> Result<String> {
    let prefer_gui = match gui_default {
        GuiDefault::True => true,
        GuiDefault::False => false,
        GuiDefault::Auto => has_display,
    };

    if prefer_gui {
        if let Some(tool) = merge_guitool {
            return Ok(tool);
        }
        if let Some(tool) = merge_tool {
            return Ok(tool);
        }
    } else if let Some(tool) = merge_tool {
        return Ok(tool);
    }

    if let Some(tool) = merge_guitool {
        return Ok(tool);
    }

    Err(Error::new(ErrorKind::Backend(
        "No merge.tool or merge.guitool configured. Set one with: \
         git config merge.tool <toolname> or git config merge.guitool <toolname>"
            .to_string(),
    )))
}

fn resolve_mergetool_config(repo: &gix::Repository, has_display: bool) -> Result<MergetoolConfig> {
    let merge_tool = git_config_get(repo, "merge.tool")?;
    let merge_guitool = git_config_get(repo, "merge.guitool")?;
    let gui_default = parse_gui_default(git_config_get(repo, "mergetool.guiDefault")?.as_deref())?;

    let tool_name = choose_mergetool_name(merge_tool, merge_guitool, gui_default, has_display)?;
    let tool_cmd = resolve_mergetool_command_with_trust_mode(repo, &tool_name)?;
    let tool_path = resolve_mergetool_tool_path_with_trust_mode(repo, &tool_name)?;
    let trust_exit_code =
        match git_config_get_bool(repo, &format!("mergetool.{tool_name}.trustExitCode"))? {
            Some(value) => value,
            None => git_config_get_bool(repo, "mergetool.trustExitCode")?.unwrap_or(false),
        };
    let write_to_temp = git_config_get_bool(repo, "mergetool.writeToTemp")?.unwrap_or(false);
    let keep_temporaries = git_config_get_bool(repo, "mergetool.keepTemporaries")?.unwrap_or(false);

    Ok(MergetoolConfig {
        tool_name,
        tool_cmd,
        tool_path,
        trust_exit_code,
        write_to_temp,
        keep_temporaries,
    })
}

/// Resolve a mergetool string config key behind the repository-local consent
/// gate.
///
/// Used for `mergetool.<tool>.cmd` and `mergetool.<tool>.path`: a local-scope
/// value would let a hostile repository choose what gets executed, so it is
/// only honored when the user has explicitly trusted this repository/tool pair
/// (see `repo_local_mergetool_command_allowed`); otherwise a global-scope
/// value wins, and a missing global value is a refusal.
fn resolve_mergetool_string_value_with_trust_mode(
    repo: &gix::Repository,
    key: &str,
    value_kind: &str,
    tool_name: &str,
) -> Result<Option<String>> {
    let global_value = git_config_get_with_scope(repo, key, GitConfigScope::Global)?;
    let local_value = git_config_get_with_scope(repo, key, GitConfigScope::Local)?;

    let Some(local_value) = local_value else {
        return Ok(global_value);
    };

    if repo_local_mergetool_command_allowed(repo, tool_name)? {
        return Ok(Some(local_value));
    }

    if global_value.is_some() {
        return Ok(global_value);
    }

    let consent_key =
        repo_local_mergetool_command_consent_key(repo_workdir_for_mergetool(repo), tool_name);
    Err(Error::new(ErrorKind::Backend(format!(
        "Refusing to execute repository-local mergetool {value_kind} for '{tool_name}' without explicit consent.\n\
         Blocked {value_kind} from repository config:\n\
         {local_value}\n\
         To allow this {value_kind} for this repository and tool, run:\n\
         git config --global {} true",
        consent_key,
    ))))
}

fn resolve_mergetool_command_with_trust_mode(
    repo: &gix::Repository,
    tool_name: &str,
) -> Result<Option<String>> {
    resolve_mergetool_string_value_with_trust_mode(
        repo,
        &format!("mergetool.{tool_name}.cmd"),
        "command",
        tool_name,
    )
}

fn resolve_mergetool_tool_path_with_trust_mode(
    repo: &gix::Repository,
    tool_name: &str,
) -> Result<Option<String>> {
    resolve_mergetool_string_value_with_trust_mode(
        repo,
        &format!("mergetool.{tool_name}.path"),
        "path",
        tool_name,
    )
}

fn repo_local_mergetool_command_allowed(repo: &gix::Repository, tool_name: &str) -> Result<bool> {
    let consent_key =
        repo_local_mergetool_command_consent_key(repo_workdir_for_mergetool(repo), tool_name);
    if test_repo_local_mergetool_command_allowed(&consent_key) {
        return Ok(true);
    }
    Ok(
        git_config_get_bool_with_scope(repo, &consent_key, GitConfigScope::Global)?
            .unwrap_or(false),
    )
}

fn repo_workdir_for_mergetool(repo: &gix::Repository) -> &Path {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

fn repo_local_mergetool_command_consent_key(workdir: &Path, tool_name: &str) -> String {
    let repo_tool_fingerprint = stable_repo_tool_fingerprint(workdir, tool_name);
    format!("gitcomet.mergetool.allowrepolocalcmd-{repo_tool_fingerprint}")
}

static TEST_ALLOWED_REPO_LOCAL_MERGETOOL_COMMANDS: OnceLock<Mutex<HashSet<String>>> =
    OnceLock::new();

pub(crate) fn allow_test_repo_local_mergetool_command(workdir: &Path, tool_name: &str) {
    let consent_key = repo_local_mergetool_command_consent_key(workdir, tool_name);
    let allowed =
        TEST_ALLOWED_REPO_LOCAL_MERGETOOL_COMMANDS.get_or_init(|| Mutex::new(HashSet::new()));
    allowed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(consent_key);
}

fn test_repo_local_mergetool_command_allowed(consent_key: &str) -> bool {
    let Some(allowed) = TEST_ALLOWED_REPO_LOCAL_MERGETOOL_COMMANDS.get() else {
        return false;
    };
    allowed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(consent_key)
}

fn stable_path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[cfg(not(any(unix, windows)))]
    {
        path.to_str()
            .map(|text| text.as_bytes().to_vec())
            .unwrap_or_else(|| format!("{path:?}").into_bytes())
    }
}

fn stable_repo_tool_fingerprint(workdir: &Path, tool_name: &str) -> String {
    let repo_path = canonicalize_or_original(workdir.to_path_buf());
    let mut bytes = stable_path_bytes(&repo_path);
    bytes.push(0);
    bytes.extend_from_slice(tool_name.as_bytes());
    format!("{:016x}", fnv1a_64(&bytes))
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[derive(Debug)]
struct StagePaths {
    workdir: PathBuf,
    base: PathBuf,
    local: PathBuf,
    remote: PathBuf,
    /// Whether the index carried a stage 1 entry. Built-in merge tools take a
    /// different (two-way) command line when there is no merge base, so the
    /// empty placeholder file written for `base` must not be mistaken for one.
    base_present: bool,
    _temp_dir: Option<tempfile::TempDir>,
    cleanup_files: bool,
}

impl Drop for StagePaths {
    fn drop(&mut self) {
        if !self.cleanup_files {
            return;
        }
        for path in [&self.base, &self.local, &self.remote] {
            match stage_path_to_fs_path(&self.workdir, path) {
                Ok(path) => match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {}
                },
                // Stage paths are built from a conflict path already sanitized
                // by sanitize_conflict_path_for_worktree; an Err here is
                // unreachable, and skipping cleanup is the safe outcome.
                Err(_) => {}
            }
        }
    }
}

fn materialize_mergetool_stage_files(
    repo: &gix::Repository,
    workdir: &Path,
    conflict_path: &Path,
    write_to_temp: bool,
    keep_temporaries: bool,
) -> Result<StagePaths> {
    let mut stage_paths =
        build_stage_paths(workdir, conflict_path, write_to_temp, keep_temporaries)?;
    let base_bytes = gix_index_stage_blob_bytes_optional(repo, conflict_path, 1)?;
    stage_paths.base_present = base_bytes.is_some();
    write_stage_bytes(
        workdir,
        &stage_paths.base,
        base_bytes.as_deref().unwrap_or(b""),
    )?;
    write_stage_bytes(
        workdir,
        &stage_paths.local,
        gix_index_stage_blob_bytes_optional(repo, conflict_path, 2)?
            .as_deref()
            .unwrap_or(b""),
    )?;
    write_stage_bytes(
        workdir,
        &stage_paths.remote,
        gix_index_stage_blob_bytes_optional(repo, conflict_path, 3)?
            .as_deref()
            .unwrap_or(b""),
    )?;
    Ok(stage_paths)
}

fn build_stage_paths(
    workdir: &Path,
    conflict_path: &Path,
    write_to_temp: bool,
    keep_temporaries: bool,
) -> Result<StagePaths> {
    let normalized_conflict_path = normalize_path_for_platform(conflict_path);
    let (mut merge_base, ext) = split_merged_path_and_extension(&normalized_conflict_path);
    let pid = std::process::id();

    if write_to_temp {
        let tmp_dir = tempfile::Builder::new()
            .prefix("gitcomet-mergetool-")
            .tempdir()
            .map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;
        let (tmp_dir_path, temp_dir_guard) = if keep_temporaries {
            (tmp_dir.keep(), None)
        } else {
            (tmp_dir.path().to_path_buf(), Some(tmp_dir))
        };
        merge_base = PathBuf::from(merge_base.file_name().unwrap_or_default());

        let merge_base_name = merge_base.file_name().unwrap_or_default();
        let base = tmp_dir_path.join(build_stage_variant_file_name(
            merge_base_name,
            "BASE",
            pid,
            &ext,
        ));
        let local = tmp_dir_path.join(build_stage_variant_file_name(
            merge_base_name,
            "LOCAL",
            pid,
            &ext,
        ));
        let remote = tmp_dir_path.join(build_stage_variant_file_name(
            merge_base_name,
            "REMOTE",
            pid,
            &ext,
        ));

        return Ok(StagePaths {
            workdir: workdir.to_path_buf(),
            base,
            local,
            remote,
            base_present: false,
            _temp_dir: temp_dir_guard,
            cleanup_files: false,
        });
    }

    let merge_base = PathBuf::from(".").join(merge_base);
    let parent = merge_base.parent().unwrap_or(Path::new("."));
    let merge_base_name = merge_base.file_name().unwrap_or_default();
    let base = parent.join(build_stage_variant_file_name(
        merge_base_name,
        "BASE",
        pid,
        &ext,
    ));
    let local = parent.join(build_stage_variant_file_name(
        merge_base_name,
        "LOCAL",
        pid,
        &ext,
    ));
    let remote = parent.join(build_stage_variant_file_name(
        merge_base_name,
        "REMOTE",
        pid,
        &ext,
    ));

    Ok(StagePaths {
        workdir: workdir.to_path_buf(),
        base,
        local,
        remote,
        base_present: false,
        _temp_dir: None,
        cleanup_files: !keep_temporaries,
    })
}

fn split_merged_path_and_extension(path: &Path) -> (PathBuf, Option<OsString>) {
    let mut merge_base = path.to_path_buf();
    let ext = path.extension().map(OsStr::to_os_string);
    if ext.is_some() {
        merge_base.set_extension("");
    }
    (merge_base, ext)
}

fn build_stage_variant_file_name(
    base_name: &OsStr,
    role: &str,
    pid: u32,
    ext: &Option<OsString>,
) -> OsString {
    let mut name = base_name.to_os_string();
    name.push(format!("_{role}_{pid}"));
    if let Some(ext) = ext.as_deref() {
        name.push(".");
        name.push(ext);
    }
    name
}

fn normalize_path_for_platform(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        normalized.push(component.as_os_str());
    }
    normalized
}

/// Validate a conflict path from the index before it touches the filesystem.
///
/// The path originates from gix index entries (`path_buf_from_git_bytes`), so
/// a hostile repository can supply any byte sequence. Only plain relative
/// paths are accepted: absolute paths, paths with a Windows prefix (`C:\`,
/// `\\server\share`, `C:`), root-relative paths, and paths containing a
/// ParentDir (`..`) component are rejected so stage and merged files can never
/// be written outside the worktree. `normalize_path_for_platform` is applied
/// to the accepted result (its `components()` pass already normalizes
/// separators).
fn sanitize_conflict_path_for_worktree(conflict_path: &Path) -> Result<PathBuf> {
    if conflict_path.is_absolute() {
        return Err(Error::new(ErrorKind::Backend(format!(
            "Refusing absolute conflict path for mergetool: {}",
            conflict_path.display()
        ))));
    }
    if let Some(component) = conflict_path.components().find(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::ParentDir
        )
    }) {
        return Err(Error::new(ErrorKind::Backend(format!(
            "Refusing conflict path '{}' for mergetool: it must be a plain \
             relative path inside the worktree (unsafe component: {component:?})",
            conflict_path.display(),
        ))));
    }
    Ok(normalize_path_for_platform(conflict_path))
}

fn stage_path_to_fs_path(workdir: &Path, stage_path: &Path) -> Result<PathBuf> {
    if stage_path.is_absolute() {
        // Absolute stage files live in a tempdir created by us (writeToTemp);
        // their location never comes from the untrusted conflict path.
        return Ok(stage_path.to_path_buf());
    }
    // Defense in depth: relative stage paths derive from the conflict path;
    // re-check them so `..` can never escape the workdir here.
    let sanitized = sanitize_conflict_path_for_worktree(stage_path)?;
    Ok(workdir.join(sanitized))
}

fn write_stage_bytes(workdir: &Path, stage_path: &Path, bytes: &[u8]) -> Result<()> {
    let path = stage_path_to_fs_path(workdir, stage_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::new(ErrorKind::Io(e.kind())))?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::new(ErrorKind::Io(e.kind())))
}

/// Read a git config value. Returns `Ok(None)` if the key is not set.
fn git_config_get(repo: &gix::Repository, key: &str) -> Result<Option<String>> {
    git_config_get_with_scope(repo, key, GitConfigScope::Any)
}

/// Read a git config value from a specific scope. Returns `Ok(None)` if the key is not set.
fn git_config_get_with_scope(
    repo: &gix::Repository,
    key: &str,
    scope: GitConfigScope,
) -> Result<Option<String>> {
    let config = repo.config_snapshot();
    let value = match git_config_scope_filter(scope) {
        Some(filter) => config.plumbing().string_filter(key, filter),
        None => config.plumbing().string(key),
    };

    Ok(value.and_then(|value| {
        let value = bytes_to_text_preserving_utf8(&value);
        (!value.is_empty()).then_some(value)
    }))
}

/// Read a git config boolean value.
///
/// Supports git-style boolean literals: true/false, yes/no, on/off, 1/0.
fn git_config_get_bool(repo: &gix::Repository, key: &str) -> Result<Option<bool>> {
    git_config_get_bool_with_scope(repo, key, GitConfigScope::Any)
}

/// Read a git config boolean value from a specific scope.
///
/// Supports git-style boolean literals: true/false, yes/no, on/off, 1/0.
fn git_config_get_bool_with_scope(
    repo: &gix::Repository,
    key: &str,
    scope: GitConfigScope,
) -> Result<Option<bool>> {
    let config = repo.config_snapshot();
    let value = match git_config_scope_filter(scope) {
        Some(filter) => config.plumbing().boolean_filter(key, filter),
        None => config.plumbing().boolean(key),
    };

    match value.transpose() {
        Some(Ok(value)) => Ok(Some(value)),
        Some(Err(err)) => {
            let value = bytes_to_text_preserving_utf8(err.input.as_ref());
            Err(Error::new(ErrorKind::Backend(format!(
                "Invalid boolean value for git config {key}: {:?}. Expected true/false, yes/no, on/off, or 1/0.",
                value
            ))))
        }
        None => Ok(None),
    }
}

type GitConfigFilter = fn(&gix::config::file::Metadata) -> bool;

fn git_config_scope_filter(scope: GitConfigScope) -> Option<GitConfigFilter> {
    match scope {
        GitConfigScope::Any => None,
        GitConfigScope::Global => Some(config_value_is_global),
        GitConfigScope::Local => Some(config_value_is_local),
    }
}

fn config_value_is_global(meta: &gix::config::file::Metadata) -> bool {
    meta.source.kind() == gix::config::source::Kind::Global
}

fn config_value_is_local(meta: &gix::config::file::Metadata) -> bool {
    meta.source == gix::config::Source::Local
}

fn parse_git_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum MergedFileState {
    Present(Vec<u8>),
    Missing,
}

fn read_merged_file_state(path: &Path) -> Result<MergedFileState> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(MergedFileState::Present(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MergedFileState::Missing),
        Err(e) => Err(Error::new(ErrorKind::Io(e.kind()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_repo(workdir: &Path) -> gix::Repository {
        gix::open(workdir).unwrap()
    }

    #[test]
    fn test_git_config_get_nonexistent_key_returns_none() {
        // Create a temporary git repo
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let result = git_config_get(&repo, "nonexistent.key.xyz").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_git_config_get_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        // Set a config value
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("config")
            .arg("merge.tool")
            .arg("vimdiff")
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let result = git_config_get(&repo, "merge.tool").unwrap();
        assert_eq!(result, Some("vimdiff".to_string()));
    }

    #[test]
    fn test_git_config_get_with_scope_local_ignores_worktree_config() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "extensions.worktreeConfig", "true"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "--worktree", "mergetool.fake.cmd", "worktree-cmd"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        assert_eq!(
            git_config_get(&repo, "mergetool.fake.cmd").unwrap(),
            Some("worktree-cmd".to_string())
        );
        assert_eq!(
            git_config_get_with_scope(&repo, "mergetool.fake.cmd", GitConfigScope::Local).unwrap(),
            None
        );
    }

    #[test]
    fn test_read_index_stage_bytes_optional_no_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        // No conflict stages exist
        let repo = gix::open(workdir).unwrap();
        let result =
            gix_index_stage_blob_bytes_optional(&repo, Path::new("nonexistent.txt"), 1).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_build_stage_paths_write_to_temp_false_uses_workdir_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = build_stage_paths(tmp.path(), Path::new("dir/a.txt"), false, false).unwrap();

        assert!(paths._temp_dir.is_none());
        assert!(paths.cleanup_files);
        assert_eq!(paths.base.parent(), Some(Path::new("./dir")));
        assert_eq!(paths.local.parent(), Some(Path::new("./dir")));
        assert_eq!(paths.remote.parent(), Some(Path::new("./dir")));

        let base_name = paths
            .base
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        let local_name = paths
            .local
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        let remote_name = paths
            .remote
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        assert!(base_name.starts_with("a_BASE_"), "{base_name}");
        assert!(local_name.starts_with("a_LOCAL_"), "{local_name}");
        assert!(remote_name.starts_with("a_REMOTE_"), "{remote_name}");
        assert!(base_name.ends_with(".txt"));
        assert!(local_name.ends_with(".txt"));
        assert!(remote_name.ends_with(".txt"));
    }

    #[test]
    fn test_build_stage_paths_write_to_temp_true_uses_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = build_stage_paths(tmp.path(), Path::new("nested/a.txt"), true, false).unwrap();

        assert!(paths._temp_dir.is_some());
        assert!(!paths.cleanup_files);
        assert!(paths.base.is_absolute());
        assert!(paths.local.is_absolute());
        assert!(paths.remote.is_absolute());

        let base_name = paths
            .base
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        let local_name = paths
            .local
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        let remote_name = paths
            .remote
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated stage filename should be valid unicode")
            .to_string();
        assert!(base_name.starts_with("a_BASE_"), "{base_name}");
        assert!(local_name.starts_with("a_LOCAL_"), "{local_name}");
        assert!(remote_name.starts_with("a_REMOTE_"), "{remote_name}");
        assert!(base_name.ends_with(".txt"));
    }

    #[test]
    fn test_normalize_path_for_platform_preserves_components() {
        let normalized = normalize_path_for_platform(Path::new("docs/nested/file.txt"));
        let components: Vec<String> = normalized
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .expect("test path components should be unicode")
                    .to_string()
            })
            .collect();
        assert_eq!(
            components,
            vec![
                "docs".to_string(),
                "nested".to_string(),
                "file.txt".to_string()
            ]
        );
    }

    #[test]
    fn test_sanitize_conflict_path_for_worktree_accepts_plain_relative_paths() {
        for path in ["file.txt", "docs/nested/file.txt", "./file.txt"] {
            let sanitized = sanitize_conflict_path_for_worktree(Path::new(path))
                .unwrap_or_else(|_| panic!("expected {path:?} to be accepted"));
            assert_eq!(sanitized, PathBuf::from(path));
        }
    }

    #[test]
    fn test_sanitize_conflict_path_for_worktree_rejects_parent_dir() {
        for path in [
            "../escape.txt",
            "docs/../../escape.txt",
            "docs/..",
            "docs/../nested/file.txt",
        ] {
            let err = match sanitize_conflict_path_for_worktree(Path::new(path)) {
                Ok(_) => panic!("expected {path:?} to be rejected"),
                Err(err) => err,
            };
            assert!(
                matches!(err.kind(), ErrorKind::Backend(_)),
                "path={path:?} err={err}"
            );
        }
    }

    #[test]
    fn test_sanitize_conflict_path_for_worktree_rejects_absolute_paths() {
        for path in ["/etc/passwd", "/abs/dir/file.txt"] {
            let err = match sanitize_conflict_path_for_worktree(Path::new(path)) {
                Ok(_) => panic!("expected {path:?} to be rejected"),
                Err(err) => err,
            };
            assert!(
                matches!(err.kind(), ErrorKind::Backend(_)),
                "path={path:?} err={err}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_sanitize_conflict_path_for_worktree_rejects_windows_prefix() {
        for path in [
            r"C:\escape.txt",
            r"C:escape.txt",
            r"\\server\share\escape.txt",
        ] {
            let err = match sanitize_conflict_path_for_worktree(Path::new(path)) {
                Ok(_) => panic!("expected {path:?} to be rejected"),
                Err(err) => err,
            };
            assert!(
                matches!(err.kind(), ErrorKind::Backend(_)),
                "path={path:?} err={err}"
            );
        }
    }

    #[test]
    fn test_stage_path_to_fs_path_rejects_parent_dir_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();

        // A relative stage path carrying `..` must be refused rather than
        // silently resolved against the workdir.
        let err = stage_path_to_fs_path(workdir, Path::new("../../escape.txt")).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::Backend(_)), "err={err}");

        // Absolute stage paths (writeToTemp) come from a tempdir created by
        // us and still resolve unchanged.
        let absolute_stage = workdir.join("stages").join("a_BASE_123.txt");
        assert_eq!(
            stage_path_to_fs_path(workdir, &absolute_stage).unwrap(),
            absolute_stage
        );
    }

    #[test]
    fn test_resolve_mergetool_tool_path_with_trust_mode_blocks_repo_local_without_consent() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.fake.path", "/tmp/evil-tool"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let err = resolve_mergetool_tool_path_with_trust_mode(&repo, "fake").unwrap_err();
        assert!(
            matches!(
                err.kind(),
                ErrorKind::Backend(message)
                    if message.contains("repository-local mergetool path for 'fake'")
                    && message.contains("gitcomet.mergetool.allowrepolocalcmd")
            ),
            "err={err}"
        );
    }

    #[test]
    fn test_resolve_mergetool_tool_path_with_trust_mode_uses_local_path_after_consent() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        allow_test_repo_local_mergetool_command(workdir, "fake");
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.fake.path", "/opt/fake-tool"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let path = resolve_mergetool_tool_path_with_trust_mode(&repo, "fake").unwrap();
        assert_eq!(path.as_deref(), Some("/opt/fake-tool"));
    }

    #[test]
    fn test_resolve_mergetool_config_refuses_repo_local_path_without_consent() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.cli.path", "/tmp/evil-tool"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let err = resolve_mergetool_config(&repo, false).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                ErrorKind::Backend(message) if message.contains("repository-local mergetool path for 'cli'")
            ),
            "err={err}"
        );
    }

    #[test]
    fn test_resolve_mergetool_config_uses_local_path_with_consent() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        allow_test_repo_local_mergetool_command(workdir, "cli");
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.cli.path", "/opt/fake-tool"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert_eq!(cfg.tool_path.as_deref(), Some("/opt/fake-tool"));
    }

    #[cfg(windows)]
    #[test]
    fn test_build_stage_paths_write_to_temp_false_normalizes_windows_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let paths =
            build_stage_paths(tmp.path(), Path::new("docs/a space.txt"), false, false).unwrap();
        let base = paths.base.to_str().expect("path should be unicode");
        let local = paths.local.to_str().expect("path should be unicode");
        let remote = paths.remote.to_str().expect("path should be unicode");
        assert!(
            !base.contains('/'),
            "stage path should avoid mixed separators on Windows: {base}"
        );
        assert!(
            !local.contains('/'),
            "stage path should avoid mixed separators on Windows: {local}"
        );
        assert!(
            !remote.contains('/'),
            "stage path should avoid mixed separators on Windows: {remote}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_run_custom_mergetool_command_windows_executes_quoted_powershell_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        let remote = workdir.join("remote.txt");
        let merged = workdir.join("merged.txt");
        std::fs::write(&remote, b"theirs\n").unwrap();

        let output = run_custom_mergetool_command(
            r#"powershell -NoProfile -Command "[System.IO.File]::WriteAllBytes($env:MERGED, [System.IO.File]::ReadAllBytes($env:REMOTE))""#,
            workdir,
            Path::new("base.txt"),
            Path::new("local.txt"),
            Path::new("remote.txt"),
            &merged,
        )
        .unwrap();

        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8(output.stderr.clone())
                .unwrap_or_else(|_| "<non-utf8 stderr>".to_string())
        );
        assert_eq!(std::fs::read(&merged).unwrap(), b"theirs\n");
        let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
        assert!(
            !stdout.contains("[System.IO.File]"),
            "powershell payload should execute, not be echoed as a string expression"
        );
    }

    #[test]
    fn test_parse_git_bool_true_variants() {
        for value in ["true", "TRUE", "yes", "on", "1", "  YeS  "] {
            assert_eq!(parse_git_bool(value), Some(true), "value={value:?}");
        }
    }

    #[test]
    fn test_parse_git_bool_false_variants() {
        for value in ["false", "FALSE", "no", "off", "0", "  Off  "] {
            assert_eq!(parse_git_bool(value), Some(false), "value={value:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_shell_command_windows_uses_cmd_percent_expansion() {
        let output = shell_command("echo %GITCOMET_MERGETOOL_TEST_TOKEN%")
            .env("GITCOMET_MERGETOOL_TEST_TOKEN", "from-cmd")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
        assert!(
            stdout.contains("from-cmd"),
            "expected cmd percent expansion in stdout"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_shell_command_unix_uses_sh_dollar_expansion() {
        let output = shell_command("echo \"$GITCOMET_MERGETOOL_TEST_TOKEN\"")
            .env("GITCOMET_MERGETOOL_TEST_TOKEN", "from-sh")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("stdout should be utf-8");
        assert!(
            stdout.contains("from-sh"),
            "expected sh dollar expansion in stdout"
        );
    }

    #[test]
    fn test_parse_gui_default_variants() {
        assert_eq!(parse_gui_default(None).unwrap(), GuiDefault::False);
        assert_eq!(parse_gui_default(Some("auto")).unwrap(), GuiDefault::Auto);
        assert_eq!(parse_gui_default(Some("TRUE")).unwrap(), GuiDefault::True);
        assert_eq!(parse_gui_default(Some("off")).unwrap(), GuiDefault::False);
    }

    #[test]
    fn test_parse_gui_default_invalid_errors() {
        let err = parse_gui_default(Some("sometimes")).unwrap_err();
        assert!(matches!(
            err.kind(),
            ErrorKind::Backend(message) if message.contains("mergetool.guiDefault")
        ));
    }

    #[test]
    fn test_choose_mergetool_name_prefers_guitool_when_enabled() {
        let selected = choose_mergetool_name(
            Some("cli-tool".to_string()),
            Some("gui-tool".to_string()),
            GuiDefault::True,
            false,
        )
        .unwrap();
        assert_eq!(selected, "gui-tool");
    }

    #[test]
    fn test_choose_mergetool_name_auto_without_display_prefers_cli_tool() {
        let selected = choose_mergetool_name(
            Some("cli-tool".to_string()),
            Some("gui-tool".to_string()),
            GuiDefault::Auto,
            false,
        )
        .unwrap();
        assert_eq!(selected, "cli-tool");
    }

    #[test]
    fn test_choose_mergetool_name_auto_with_display_prefers_guitool() {
        let selected = choose_mergetool_name(
            Some("cli-tool".to_string()),
            Some("gui-tool".to_string()),
            GuiDefault::Auto,
            true,
        )
        .unwrap();
        assert_eq!(selected, "gui-tool");
    }

    #[test]
    fn test_choose_mergetool_name_falls_back_to_guitool_if_only_guitool_set() {
        let selected =
            choose_mergetool_name(None, Some("gui-tool".to_string()), GuiDefault::False, false)
                .unwrap();
        assert_eq!(selected, "gui-tool");
    }

    #[test]
    fn test_choose_mergetool_name_errors_when_no_tool_configured() {
        let err = choose_mergetool_name(None, None, GuiDefault::False, false).unwrap_err();
        assert!(matches!(
            err.kind(),
            ErrorKind::Backend(message) if message.contains("merge.tool or merge.guitool")
        ));
    }

    #[test]
    fn test_git_config_get_bool_nonexistent_key_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let result = git_config_get_bool(&repo, "nonexistent.bool.key").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_git_config_get_bool_parses_variants() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("config")
            .arg("mergetool.test.trustExitCode")
            .arg("yes")
            .output()
            .unwrap();
        let repo = open_repo(workdir);
        assert_eq!(
            git_config_get_bool(&repo, "mergetool.test.trustExitCode").unwrap(),
            Some(true)
        );

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("config")
            .arg("mergetool.test.trustExitCode")
            .arg("off")
            .output()
            .unwrap();
        let repo = open_repo(workdir);
        assert_eq!(
            git_config_get_bool(&repo, "mergetool.test.trustExitCode").unwrap(),
            Some(false)
        );
    }

    #[test]
    fn test_git_config_get_bool_invalid_value_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("config")
            .arg("mergetool.test.trustExitCode")
            .arg("sometimes")
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let err = git_config_get_bool(&repo, "mergetool.test.trustExitCode").unwrap_err();
        assert!(matches!(
            err.kind(),
            ErrorKind::Backend(message) if message.contains("Invalid boolean value")
        ));
    }

    #[test]
    fn test_git_config_get_bool_bare_key_is_true() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        let config_path = workdir.join(".git").join("config");
        let mut config = std::fs::read_to_string(&config_path).unwrap();
        config.push_str("\n[mergetool \"test\"]\n\ttrustExitCode\n");
        std::fs::write(config_path, config).unwrap();

        let repo = open_repo(workdir);
        assert_eq!(
            git_config_get_bool(&repo, "mergetool.test.trustExitCode").unwrap(),
            Some(true)
        );
    }

    #[test]
    fn test_resolve_mergetool_config_prefers_guitool_and_reads_path_override() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();
        // The path below is repository-local, so it only takes effect after
        // explicit consent for this repository/tool pair, like `.cmd`.
        allow_test_repo_local_mergetool_command(workdir, "gui");

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.guitool", "gui"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.guiDefault", "true"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.gui.path", "/opt/fake-gui-tool"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.gui.trustExitCode", "yes"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert_eq!(cfg.tool_name, "gui");
        assert_eq!(cfg.tool_cmd, None);
        assert_eq!(cfg.tool_path.as_deref(), Some("/opt/fake-gui-tool"));
        assert!(cfg.trust_exit_code);
        assert!(!cfg.write_to_temp);
        assert!(!cfg.keep_temporaries);
    }

    #[test]
    fn test_resolve_mergetool_config_auto_without_display_uses_merge_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.guitool", "gui"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.guiDefault", "auto"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert_eq!(cfg.tool_name, "cli");
        assert_eq!(cfg.tool_cmd, None);
        assert!(!cfg.write_to_temp);
        assert!(!cfg.keep_temporaries);
    }

    #[test]
    fn test_resolve_mergetool_config_trust_exit_code_falls_back_to_global_setting() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.trustExitCode", "true"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert!(cfg.trust_exit_code);
    }

    #[test]
    fn test_resolve_mergetool_config_tool_specific_trust_exit_overrides_global() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.trustExitCode", "true"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.cli.trustExitCode", "false"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert!(!cfg.trust_exit_code);
    }

    #[test]
    fn test_resolve_mergetool_config_reads_write_to_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.writeToTemp", "true"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert!(cfg.write_to_temp);
        assert!(!cfg.keep_temporaries);
    }

    #[test]
    fn test_resolve_mergetool_config_reads_keep_temporaries() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "merge.tool", "cli"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["config", "mergetool.keepTemporaries", "true"])
            .output()
            .unwrap();

        let repo = open_repo(workdir);
        let cfg = resolve_mergetool_config(&repo, false).unwrap();
        assert!(!cfg.write_to_temp);
        assert!(cfg.keep_temporaries);
    }
}
