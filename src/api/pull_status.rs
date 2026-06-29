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
    #[serde(default)]
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
    let path = format!(
        "{}/commits/{}/status",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(sha)
    );
    let value: serde_json::Value = client.json(Method::GET, &path, &[], None::<&()>).await?;
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
}
