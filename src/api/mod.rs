pub mod hook;
pub mod issue;
pub mod label;
pub mod milestone;
pub mod notification;
pub mod org;
pub mod protect;
pub mod pull;
mod pull_core;
mod pull_review;
mod pull_status;
pub mod release;
pub mod repo;
mod repo_core;
mod repo_keys;
mod repo_social;
pub mod search;
pub(crate) mod serde_util;
pub mod tag;
pub mod user;
pub mod workflow;
mod workflow_run;
mod workflow_secrets;
mod workflow_view;

use anyhow::{anyhow, Result};

/// Split an `owner/name` slug. Returns a helpful error if the form is wrong.
pub fn split_repo(repo: &str) -> Result<(&str, &str)> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(anyhow!("expected '<owner>/<name>', got '{repo}'"));
    }
    Ok((owner, name))
}

pub fn repo_path(owner: &str, name: &str) -> String {
    format!(
        "/api/v1/repos/{}/{}",
        path_segment(owner),
        path_segment(name)
    )
}

/// Percent-encode a string for use as a single URL path segment. Keeps the
/// RFC 3986 unreserved set; encodes everything else (including `/`) so a name
/// can't break out of its segment.
pub fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn urlencode_path_segment(s: &str) -> String {
    path_segment(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_slug() {
        let (o, n) = split_repo("rasterstate/fj").unwrap();
        assert_eq!(o, "rasterstate");
        assert_eq!(n, "fj");
    }

    #[test]
    fn rejects_missing_slash() {
        assert!(split_repo("fj").is_err());
    }

    #[test]
    fn rejects_empty_owner() {
        assert!(split_repo("/fj").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        assert!(split_repo("rasterstate/").is_err());
    }

    #[test]
    fn rejects_deep_repo_slug() {
        assert!(split_repo("rasterstate/fj/extra").is_err());
    }

    #[test]
    fn repo_path_encodes_owner_and_name_segments() {
        assert_eq!(
            repo_path("space owner", "repo/name"),
            "/api/v1/repos/space%20owner/repo%2Fname"
        );
    }

    #[test]
    fn path_segment_encodes_reserved_and_utf8_bytes() {
        assert_eq!(path_segment("feature/a b"), "feature%2Fa%20b");
        assert_eq!(path_segment("é"), "%C3%A9");
    }
}
