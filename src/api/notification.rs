use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub unread: bool,
    pub pinned: bool,
    pub updated_at: DateTime<Utc>,
    pub url: String,
    pub html_url: String,
    pub subject: Subject,
    pub repository: Option<NotificationRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub latest_comment_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRepo {
    pub full_name: String,
    pub html_url: String,
}

pub async fn list(client: &Client, all: bool, limit: u32) -> Result<Vec<Notification>> {
    let path = "/api/v1/notifications";
    let mut q = vec![("limit".into(), limit.clamp(1, 50).to_string())];
    if all {
        q.push(("all".into(), "true".into()));
    }
    client.json(Method::GET, path, &q, None::<&()>).await
}

pub async fn mark_all_read(client: &Client) -> Result<()> {
    let path = "/api/v1/notifications";
    let res = client.request(Method::PUT, path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
