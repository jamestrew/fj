use anyhow::{anyhow, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub commit: TagCommit,
    #[serde(default)]
    pub tarball_url: String,
    #[serde(default)]
    pub zipball_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagCommit {
    #[serde(default, alias = "id")]
    pub sha: String,
    #[serde(default)]
    pub url: String,
}

pub async fn list(client: &Client, owner: &str, name: &str) -> Result<Vec<Tag>> {
    let path = format!("{}/tags", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn get(client: &Client, owner: &str, name: &str, tag: &str) -> Result<Tag> {
    let path = format!(
        "{}/tags/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(tag)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateTag<'a> {
    pub tag_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<&'a str>,
}

pub async fn create(client: &Client, owner: &str, name: &str, body: &CreateTag<'_>) -> Result<Tag> {
    let path = format!("{}/tags", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, tag: &str) -> Result<()> {
    let path = format!(
        "{}/tags/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(tag)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let message = api_error_message(res).await;
    match status {
        StatusCode::NOT_FOUND => Err(anyhow!("tag '{tag}' not found")),
        StatusCode::CONFLICT => Err(anyhow!("could not delete tag '{tag}': {message}")),
        _ => Err(anyhow!("HTTP {status}: {message}")),
    }
}

async fn api_error_message(res: reqwest::Response) -> String {
    let text = res.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str())
                .map(ToString::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(text)
}
