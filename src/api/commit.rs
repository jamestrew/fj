//! Repository commits (`/repos/{owner}/{repo}/commits` and
//! `/repos/{owner}/{repo}/git/commits/{sha}`), including the server's
//! signature `verification` object.

use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCommit {
    pub sha: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub commit: CommitDetail,
    #[serde(default)]
    pub parents: Vec<CommitParent>,
    /// Present only when the request asked for stats (`stat=true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<CommitStats>,
    /// Present only when the request asked for files (`files=true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<CommitFile>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitDetail {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author: Option<CommitIdentity>,
    #[serde(default)]
    pub committer: Option<CommitIdentity>,
    /// `None` when the server did not report verification (the request opted
    /// out with `verification=false`, or the forge predates the field). That
    /// is a different fact from an unsigned commit, so it stays an `Option`.
    #[serde(default)]
    pub verification: Option<Verification>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitIdentity {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verification {
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub signer: Option<Signer>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Signer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitParent {
    #[serde(default)]
    pub sha: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitStats {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitFile {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct ListOptions<'a> {
    /// Branch name, tag name, or commit SHA to start from. `None` lets the
    /// server use the default branch.
    pub start: Option<&'a str>,
    /// Only commits touching this file or directory.
    pub path: Option<&'a str>,
    pub limit: usize,
}

/// Query for the commit list. Diff stats and affected files are switched off
/// (the list does not show them, and the server documents both as slow);
/// `verification` is left at the server default so every row carries the
/// signature verdict.
fn list_query(opts: &ListOptions<'_>) -> Vec<(String, String)> {
    let mut q = vec![
        ("stat".to_string(), "false".to_string()),
        ("files".to_string(), "false".to_string()),
    ];
    if let Some(start) = opts.start {
        q.push(("sha".into(), start.into()));
    }
    if let Some(path) = opts.path {
        q.push(("path".into(), path.into()));
    }
    q
}

pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    opts: &ListOptions<'_>,
) -> Result<Vec<RepoCommit>> {
    let path = format!("{}/commits", crate::api::repo_path(owner, name));
    let mut q = list_query(opts);
    if opts.limit <= 50 {
        q.push(("limit".into(), opts.limit.to_string()));
        return Ok(client.get_page(&path, &q).await?.items);
    }
    Ok(client
        .get_all_page(&path, &q, Some(opts.limit))
        .await?
        .items)
}

/// One commit with its diff stats, affected files, and verification.
pub async fn get(client: &Client, owner: &str, name: &str, sha: &str) -> Result<RepoCommit> {
    let path = format!(
        "{}/git/commits/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(sha)
    );
    client
        .json(Method::GET, &path, &[], None::<&()>)
        .await
        .map_err(|e| crate::client::with_not_found_context(e, format!("commit '{sha}' not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_query_skips_stats_and_files_but_never_opts_out_of_verification() {
        let q = list_query(&ListOptions {
            start: Some("v1.0.0"),
            path: Some("src/main.rs"),
            limit: 30,
        });
        assert!(q.contains(&("stat".into(), "false".into())));
        assert!(q.contains(&("files".into(), "false".into())));
        assert!(q.contains(&("sha".into(), "v1.0.0".into())));
        assert!(q.contains(&("path".into(), "src/main.rs".into())));
        assert!(!q.iter().any(|(k, _)| k == "verification"));
    }

    #[test]
    fn list_query_omits_ref_and_path_when_unset() {
        let q = list_query(&ListOptions::default());
        assert!(!q.iter().any(|(k, _)| k == "sha" || k == "path"));
    }

    #[test]
    fn commit_decodes_without_a_verification_object() {
        let c: RepoCommit = serde_json::from_value(serde_json::json!({
            "sha": "abc", "commit": { "message": "m", "verification": null }
        }))
        .unwrap();
        assert!(c.commit.verification.is_none());
        let c: RepoCommit =
            serde_json::from_value(serde_json::json!({ "sha": "abc", "commit": {} })).unwrap();
        assert!(c.commit.verification.is_none());
    }
}
