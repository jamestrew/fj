//! Repository social operations: subscriptions (watch) and stars.

use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

use super::repo_core::Repo;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Subscription {
    #[serde(default)]
    pub subscribed: bool,
    #[serde(default)]
    pub ignored: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub url: String,
}

#[allow(dead_code)]
pub async fn get_subscription(client: &Client, owner: &str, name: &str) -> Result<Subscription> {
    let path = format!("{}/subscription", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn watch(client: &Client, owner: &str, name: &str) -> Result<Subscription> {
    let path = format!("{}/subscription", crate::api::repo_path(owner, name));
    client
        .json(
            Method::PUT,
            &path,
            &[],
            Some(&serde_json::json!({ "subscribed": true, "ignored": false })),
        )
        .await
}

pub async fn unwatch(client: &Client, owner: &str, name: &str) -> Result<()> {
    let path = format!("{}/subscription", crate::api::repo_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn star(client: &Client, owner: &str, name: &str) -> Result<()> {
    let path = format!("/api/v1/user/starred/{owner}/{name}");
    let res = client.request(Method::PUT, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn unstar(client: &Client, owner: &str, name: &str) -> Result<()> {
    let path = format!("/api/v1/user/starred/{owner}/{name}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn list_starred(client: &Client, limit: u32) -> Result<Vec<Repo>> {
    let q = vec![("limit".into(), limit.clamp(1, 50).to_string())];
    client
        .json(Method::GET, "/api/v1/user/starred", &q, None::<&()>)
        .await
}
