//! Commit status: the combined CI status for a SHA (used to render PR checks).

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub sha: String,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub statuses: Vec<CommitStatus>,
    #[serde(default)]
    pub total_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub target_url: String,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn combined_status(
    client: &Client,
    owner: &str,
    name: &str,
    sha: &str,
) -> Result<CombinedStatus> {
    combined_status_page(client, owner, name, sha, &[]).await
}

/// Page cap for the complete scans a merge gate is judged on.
pub(crate) const SCAN_MAX_PAGES: u32 = 10;

pub(crate) fn page_query(page: u32) -> Vec<(String, String)> {
    vec![
        ("page".to_string(), page.to_string()),
        ("limit".to_string(), "50".to_string()),
    ]
}

/// The statuses on a commit gathered by [`combined_status_all`]. `complete`
/// is false when the page cap was reached first, in which case a context that
/// was not seen may still exist on a later page.
#[derive(Debug, Clone)]
pub struct StatusScan {
    pub combined: CombinedStatus,
    pub complete: bool,
}

/// The combined status with every page of `statuses` folded in. The endpoint
/// pages its statuses and reports no reliable total, so the walk ends only on
/// an empty page; a short page proves nothing when the server caps the page
/// size below what was asked for.
pub async fn combined_status_all(
    client: &Client,
    owner: &str,
    name: &str,
    sha: &str,
) -> Result<StatusScan> {
    let mut combined = empty_combined_status(sha);
    let mut complete = false;
    for page in 1..=SCAN_MAX_PAGES {
        let mut batch = combined_status_page(client, owner, name, sha, &page_query(page)).await?;
        if page == 1 {
            combined.state = std::mem::take(&mut batch.state);
        }
        if batch.statuses.is_empty() {
            complete = true;
            break;
        }
        let before = combined.statuses.len();
        for s in batch.statuses {
            if !combined
                .statuses
                .iter()
                .any(|seen| seen.context == s.context)
            {
                combined.statuses.push(s);
            }
        }
        // A page that repeats what was already seen means the server ignored
        // `page`; stop rather than loop, and do not claim completeness.
        if combined.statuses.len() == before {
            break;
        }
    }
    combined.total_count = combined.statuses.len() as u64;
    Ok(StatusScan { combined, complete })
}

async fn combined_status_page(
    client: &Client,
    owner: &str,
    name: &str,
    sha: &str,
    query: &[(String, String)],
) -> Result<CombinedStatus> {
    let path = format!(
        "{}/commits/{}/status",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(sha)
    );
    let value: serde_json::Value = client.json(Method::GET, &path, query, None::<&()>).await?;
    if value.is_null() {
        return Ok(empty_combined_status(sha));
    }
    let mut status: CombinedStatus = serde_json::from_value(value)?;
    if status.sha.is_empty() {
        status.sha = sha.to_string();
    }
    Ok(status)
}

fn empty_combined_status(sha: &str) -> CombinedStatus {
    CombinedStatus {
        state: String::new(),
        sha: sha.to_string(),
        statuses: Vec::new(),
        total_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_status_decodes_empty_no_ci_payload() {
        let mut status: CombinedStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        if status.sha.is_empty() {
            status.sha = "abc123".to_string();
        }
        assert_eq!(status.sha, "abc123");
        assert_eq!(status.state, "");
        assert!(status.statuses.is_empty());
        assert_eq!(status.total_count, 0);
    }

    #[test]
    fn combined_status_decodes_null_statuses_as_empty() {
        let status: CombinedStatus = serde_json::from_value(serde_json::json!({
            "state": "",
            "sha": "abc123",
            "total_count": 0,
            "statuses": null
        }))
        .unwrap();
        assert_eq!(status.sha, "abc123");
        assert!(status.statuses.is_empty());
    }
}
