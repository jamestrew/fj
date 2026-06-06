use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub visibility: String,
    pub avatar_url: String,
}

pub async fn list_for_user(client: &Client) -> Result<Vec<Organization>> {
    client
        .json(Method::GET, "/api/v1/user/orgs", &[], None::<&()>)
        .await
}

pub async fn get(client: &Client, org: &str) -> Result<Organization> {
    let path = format!("/api/v1/orgs/{}", crate::api::path_segment(org));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permission: String,
}

pub async fn list_teams(client: &Client, org: &str) -> Result<Vec<Team>> {
    let path = format!("/api/v1/orgs/{}/teams", crate::api::path_segment(org));
    client.json(Method::GET, &path, &[], None::<&()>).await
}
