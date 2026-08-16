//! Remote URL display sanitization and scheme validation.
//!
//! Provides:
//! - Userinfo redaction for URLs shown in logs/errors
//! - Scheme allowlist gate for URLs passed to `git` as clone/add/set-url
//!   arguments (blocks `ext::` protocol helpers and plain `http`).

use crate::error::{Error, ErrorKind};

const ALLOWED_REMOTE_URL_SCHEMES: [&str; 4] = ["https", "ssh", "git", "file"];

/// Redacts userinfo (user[:password]) from a remote URL for display/logging.
/// git itself strips userinfo in its error output; mirror that: `https://user:tok@host/x` -> `https://host/x`, `ssh://git@host/x` -> `ssh://host/x`.
pub fn redact_remote_url_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };

    // Everything after the scheme separator up to the first literal `/` is
    // the authority; a literal `@` in it is userinfo (`user[:password]@`).
    let after_scheme = &url[scheme_end + 3..];
    let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    // The host is everything after the last `@` (RFC 3986 userinfo may not
    // contain an unencoded `@`, so the last occurrence is the separator).
    let Some(at) = authority.rfind('@') else {
        return url.to_owned();
    };

    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..scheme_end + 3]);
    redacted.push_str(&authority[at + 1..]);
    redacted.push_str(&after_scheme[authority_end..]);
    redacted
}

/// Reject remote URLs that are absolute or otherwise not safe to hand to git.
///
/// Accepts the allowlisted schemes (`https`, `ssh`, `git`, `file`), plus
/// schemeless inputs that git itself understands: local paths, scp-style
/// `user@host:path` strings, and Windows drive paths. Rejects other explicit
/// schemes (notably `http` and git's `ext::` protocol helper) and malformed
/// allowlisted-scheme URLs such as `ssh:git@example.com/org/repo.git`.
pub fn validate_remote_url(url: &str) -> Result<(), Error> {
    let url = url.trim();
    if url.is_empty() {
        return Err(Error::new(ErrorKind::Backend(
            "remote URL cannot be empty".to_string(),
        )));
    }

    let Some(scheme_end) = explicit_url_scheme_end(url) else {
        return Ok(());
    };

    let scheme = url[..scheme_end].to_ascii_lowercase();
    if !ALLOWED_REMOTE_URL_SCHEMES.contains(&scheme.as_str()) {
        return Err(Error::new(ErrorKind::Backend(format!(
            "unsupported remote URL scheme `{scheme}` (allowed: https, ssh, git, file)"
        ))));
    }

    if !url[scheme_end..].starts_with("://") {
        return Err(Error::new(ErrorKind::Backend(format!(
            "invalid remote URL format for `{scheme}`; expected `{scheme}://...`"
        ))));
    }

    Ok(())
}

fn is_windows_drive_path(url: &str) -> bool {
    let bytes = url.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn explicit_url_scheme_end(url: &str) -> Option<usize> {
    if is_windows_drive_path(url) {
        return None;
    }

    let mut chars = url.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }

    for (idx, ch) in chars {
        if ch == ':' {
            return Some(idx);
        }
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')) {
            return None;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{redact_remote_url_userinfo, validate_remote_url};

    #[test]
    fn redact_remote_url_userinfo_strips_user_and_password() {
        assert_eq!(
            redact_remote_url_userinfo("https://user:tok@host/x"),
            "https://host/x"
        );
        assert_eq!(
            redact_remote_url_userinfo("ssh://git@example.com/org/repo.git"),
            "ssh://example.com/org/repo.git"
        );
        assert_eq!(
            redact_remote_url_userinfo("https://user@host:8080/org/repo.git"),
            "https://host:8080/org/repo.git"
        );
    }

    #[test]
    fn redact_remote_url_userinfo_leaves_urls_without_userinfo() {
        assert_eq!(
            redact_remote_url_userinfo("https://example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
        assert_eq!(
            redact_remote_url_userinfo("ssh://host:2222/org/repo.git"),
            "ssh://host:2222/org/repo.git"
        );
        assert_eq!(
            redact_remote_url_userinfo("https://host/x@y"),
            "https://host/x@y"
        );
    }

    #[test]
    fn redact_remote_url_userinfo_keeps_scp_style_and_local_paths() {
        assert_eq!(
            redact_remote_url_userinfo("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        assert_eq!(redact_remote_url_userinfo("/tmp/repo.git"), "/tmp/repo.git");
        assert_eq!(
            redact_remote_url_userinfo("C:\\repos\\repo.git"),
            "C:\\repos\\repo.git"
        );
    }

    #[test]
    fn redact_remote_url_userinfo_handles_file_urls_and_percent_encoded_at() {
        assert_eq!(
            redact_remote_url_userinfo("file:///tmp/repo.git"),
            "file:///tmp/repo.git"
        );
        // `%40` is not a literal `@`; only the real `@` splits userinfo.
        assert_eq!(
            redact_remote_url_userinfo("https://user%40example.com@host/x"),
            "https://host/x"
        );
    }

    #[test]
    fn validate_remote_url_accepts_allowlisted_schemes() {
        assert!(validate_remote_url("https://example.com/org/repo.git").is_ok());
        assert!(validate_remote_url("ssh://git@example.com/org/repo.git").is_ok());
        assert!(validate_remote_url("git://example.com/org/repo.git").is_ok());
        assert!(validate_remote_url("file:///tmp/repo.git").is_ok());
    }

    #[test]
    fn validate_remote_url_rejects_unallowlisted_schemes() {
        assert!(validate_remote_url("ext::sh -c touch /tmp/pwned").is_err());
        assert!(validate_remote_url("http://example.com/org/repo.git").is_err());
    }

    #[test]
    fn validate_remote_url_keeps_schemeless_inputs_working() {
        assert!(validate_remote_url("/tmp/repo.git").is_ok());
        assert!(validate_remote_url("git@github.com:org/repo.git").is_ok());
        assert!(validate_remote_url("C:\\repos\\repo.git").is_ok());
        assert!(validate_remote_url("").is_err());
    }

    #[test]
    fn validate_remote_url_rejects_malformed_allowlisted_schemes() {
        assert!(validate_remote_url("ssh:git@example.com/org/repo.git").is_err());
    }
}
