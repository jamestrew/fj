//! Detect the Forgejo repo associated with the current working directory by
//! reading git remotes. Used so `fj pr list` (etc.) work without `-R` inside
//! a clone.

use std::process::Command;

use anyhow::{anyhow, Context, Result};

/// A parsed `owner/name` plus the hostname it lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RemoteRepo {
    #[allow(dead_code)]
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Find a Forgejo repo for the current directory.
///
/// Preference order: explicit `--repo`/`-R` (handled by the caller), then
/// `upstream` remote, then `origin`, then any other remote.
pub fn discover(preferred_host: Option<&str>) -> Result<RemoteRepo> {
    let remotes = list_remotes()?;
    if remotes.is_empty() {
        return Err(anyhow!(
            "no git remotes found; pass `-R <owner>/<name>` or run inside a clone"
        ));
    }

    // upstream first, then origin, then the rest in stable order.
    let mut ordered: Vec<&(String, String)> = remotes
        .iter()
        .filter(|(n, _)| n == "upstream")
        .chain(remotes.iter().filter(|(n, _)| n == "origin"))
        .chain(
            remotes
                .iter()
                .filter(|(n, _)| n != "upstream" && n != "origin"),
        )
        .collect();
    // Deduplicate while preserving order.
    ordered.dedup_by(|a, b| a.0 == b.0);

    let mut last_err: Option<anyhow::Error> = None;
    for (_, url) in ordered {
        match parse_remote(url) {
            Ok(r) => {
                if preferred_host.is_none_or(|h| h.eq_ignore_ascii_case(&r.host)) {
                    return Ok(r);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err
        .unwrap_or_else(|| anyhow!("no remote matched a known Forgejo host"))
        .context("could not infer the repository from git remotes"))
}

fn list_remotes() -> Result<Vec<(String, String)>> {
    let out = Command::new("git")
        .args(["remote", "-v"])
        .output()
        .context("running `git remote -v`")?;
    if !out.status.success() {
        // Not a git repo, or git is missing. Return an empty list; the caller
        // surfaces a friendlier message.
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        // Format: "<name>\t<url> (fetch|push)"
        let Some((name_url, _)) = line.rsplit_once(' ') else {
            continue;
        };
        let Some((name, url)) = name_url.split_once('\t') else {
            continue;
        };
        let entry = (name.to_string(), url.to_string());
        if !seen.iter().any(|(n, _)| n == &entry.0) {
            seen.push(entry);
        }
    }
    Ok(seen)
}

/// Parse `owner/name` and host out of a git remote URL.
///
/// Accepts:
/// - `https://host/owner/name(.git)?`
/// - `ssh://git@host(:port)?/owner/name(.git)?`
/// - `git@host:owner/name(.git)?`  (scp-like form)
pub fn parse_remote(url: &str) -> Result<RemoteRepo> {
    let url = url.trim();

    // scp-like: git@host:owner/name(.git)?
    if !url.contains("://") {
        if let Some((user_host, path)) = url.split_once(':') {
            let host = user_host
                .rsplit_once('@')
                .map(|(_, h)| h)
                .unwrap_or(user_host);
            return split_path(host, path);
        }
        return Err(anyhow!("can't parse remote URL: {url}"));
    }

    let parsed = url::Url::parse(url).with_context(|| format!("parsing remote URL '{url}'"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("remote URL has no host: {url}"))?;
    let path = parsed.path();
    split_path(host, path)
}

fn split_path(host: &str, path: &str) -> Result<RemoteRepo> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let Some((owner, name)) = path.split_once('/') else {
        return Err(anyhow!("remote path '{path}' is not 'owner/name'"));
    };
    // A colon in owner/name means the scp-form split left a stray segment in
    // the path (e.g. `git@host:2222:owner/name` splits on the first colon, so
    // `owner` would silently become `2222:owner`). Reject it instead of
    // targeting the wrong repository; a real owner or name never contains ':'.
    if owner.is_empty()
        || name.is_empty()
        || name.contains('/')
        || owner.contains(':')
        || name.contains(':')
    {
        return Err(anyhow!("remote path '{path}' is not 'owner/name'"));
    }
    Ok(RemoteRepo {
        host: host.to_string(),
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_with_dot_git() {
        let r = parse_remote("https://rasterhub.com/rasterstate/fj.git").unwrap();
        assert_eq!(r.host, "rasterhub.com");
        assert_eq!(r.owner, "rasterstate");
        assert_eq!(r.name, "fj");
    }

    #[test]
    fn https_no_dot_git() {
        let r = parse_remote("https://rasterhub.com/rasterstate/fj").unwrap();
        assert_eq!(r.slug(), "rasterstate/fj");
    }

    #[test]
    fn ssh_url_with_port() {
        let r = parse_remote("ssh://git@rasterhub.com:2222/rasterstate/fj.git").unwrap();
        assert_eq!(r.host, "rasterhub.com");
        assert_eq!(r.slug(), "rasterstate/fj");
    }

    #[test]
    fn scp_like() {
        let r = parse_remote("git@rasterhub.com:rasterstate/fj.git").unwrap();
        assert_eq!(r.host, "rasterhub.com");
        assert_eq!(r.slug(), "rasterstate/fj");
    }

    #[test]
    fn rejects_non_repo_path() {
        assert!(parse_remote("https://example.com/").is_err());
        assert!(parse_remote("https://example.com/just-one-segment").is_err());
    }

    #[test]
    fn rejects_deep_path() {
        assert!(parse_remote("https://example.com/a/b/c").is_err());
    }

    #[test]
    fn rejects_scp_with_stray_colon() {
        // The scp split is on the first colon, so a stray `:port:` would leave
        // a colon in the owner. Reject rather than target `2222:rasterstate/fj`.
        assert!(parse_remote("git@rasterhub.com:2222:rasterstate/fj.git").is_err());
    }
}
