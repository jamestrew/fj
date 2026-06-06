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
    client.json(Method::GET, &path, &[], None::<&()>).await
}
