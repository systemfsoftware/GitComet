//! Shared filesystem hygiene primitives for security-critical short-lived
//! files: crash logs, clipboard diagnostics, image-diff caches, staged
//! patches. Consolidated here so the symlink-safety invariants live in one
//! place rather than drifting across crates.

use std::fs::{File, OpenOptions};
use std::path::Path;

/// Opens `path` for writing with O_EXCL semantics so a hostile symlink at
/// `path` can never be followed. A stale entry left by an earlier process is
/// replaced (only when it is a regular file or a symlink) and creation is
/// retried once; a directory at `path` is never deleted and produces an
/// `AlreadyExists` error.
pub fn create_new_file(path: &Path) -> std::io::Result<File> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => make_private(file),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::symlink_metadata(path) {
                Ok(metadata)
                    if metadata.file_type().is_file() || metadata.file_type().is_symlink() =>
                {
                    std::fs::remove_file(path)?;
                }
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("refusing to replace non-file entry at {}", path.display()),
                    ));
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            make_private(OpenOptions::new().write(true).create_new(true).open(path)?)
        }
        Err(err) => Err(err),
    }
}

/// Restricts a freshly created diagnostic file to the owning user on unix:
/// the O_EXCL-open above creates with umask-derived permissions, so an
/// explicit `0o600` keeps backtraces, env values, and repo paths unreadable to
/// other local accounts even under a permissive umask.
#[cfg(unix)]
fn make_private(file: File) -> std::io::Result<File> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn make_private(file: File) -> std::io::Result<File> {
    Ok(file)
}

/// Removes `path` only when `symlink_metadata` reports it as a symlink, never
/// following the link itself. Anything else (regular file, directory) is left
/// alone.
pub fn remove_symlink_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        },
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Opens `path` for append while refusing to write through a hostile symlink:
/// a symlink at `path` is removed first, so the append creates a fresh file.
/// Regular files and directories are left untouched.
pub fn open_append(path: &Path) -> std::io::Result<File> {
    remove_symlink_entry(path)?;
    OpenOptions::new().create(true).append(true).open(path)
}

/// Restricts a directory to the owning user on unix: crash reports, session
/// markers, and diagnostics can embed backtraces, environment values and
/// repository paths that other local accounts must not be able to read.
///
/// Stat-first: only issues a `chmod` when the current mode is not already
/// `0700`, so hot paths (a runtime-error log written on every `log::error!`)
/// do not pay a redundant syscall per record. Failures are returned, not
/// hidden; callers that must keep reporting (crash logs) treat this as
/// best-effort and continue, because per-file `0600` (see `create_new_file`)
/// still protects contents even when the directory cannot be tightened.
pub fn enforce_directory_is_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let is_private = std::fs::metadata(dir)
            .map(|meta| meta.permissions().mode() & 0o777 == 0o700)
            .unwrap_or(false);
        if !is_private {
            std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}
