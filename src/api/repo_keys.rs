//! Repository deploy-key operations: list, add, and delete.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployKey {
    pub id: u64,
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub read_only: bool,
    pub created_at: DateTime<Utc>,
}

pub async fn list_keys(client: &Client, owner: &str, name: &str) -> Result<Vec<DeployKey>> {
    let path = format!("{}/keys", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Serialize)]
struct AddKeyBody<'a> {
    title: &'a str,
    key: &'a str,
    read_only: bool,
}

pub async fn add_key(
    client: &Client,
    owner: &str,
    name: &str,
    title: &str,
    key: &str,
    read_only: bool,
) -> Result<DeployKey> {
    let path = format!("{}/keys", crate::api::repo_path(owner, name));
    let body = AddKeyBody {
        title,
        key,
        read_only,
    };
    client.json(Method::POST, &path, &[], Some(&body)).await
}

pub async fn delete_key(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("{}/keys/{id}", crate::api::repo_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
