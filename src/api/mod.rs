pub mod hook;
pub mod issue;
pub mod label;
pub mod milestone;
pub mod notification;
pub mod org;
pub mod protect;
pub mod pull;
pub mod release;
pub mod repo;
pub mod search;
pub(crate) mod serde_util;
pub mod user;
pub mod workflow;

use anyhow::{anyhow, Result};

/// Split an `owner/name` slug. Returns a helpful error if the form is wrong.
pub fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty())
        .ok_or_else(|| anyhow!("expected '<owner>/<name>', got '{repo}'"))
}

/// Percent-encode a string for use as a single URL path segment. Keeps the
/// RFC 3986 unreserved set; encodes everything else (including `/`) so a name
/// can't break out of its segment.
pub fn urlencode_path_segment(s: &str) -> String {
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
}
