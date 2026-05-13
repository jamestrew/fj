use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub id: u64,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub events: Vec<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub config: serde_json::Value,
}

pub async fn list(client: &Client, owner: &str, name: &str) -> Result<Vec<Hook>> {
    let path = format!("/api/v1/repos/{owner}/{name}/hooks");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Serialize)]
pub struct CreateHook<'a> {
    #[serde(rename = "type")]
    pub type_: &'a str,
    pub config: serde_json::Value,
    pub events: Vec<&'a str>,
    pub active: bool,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateHook<'_>,
) -> Result<Hook> {
    let path = format!("/api/v1/repos/{owner}/{name}/hooks");
    client.json(Method::POST, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/hooks/{id}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn test(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/hooks/{id}/tests");
    let res = client.request(Method::POST, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
