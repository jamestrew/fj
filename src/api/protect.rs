use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchProtection {
    pub branch_name: String,
    #[serde(default)]
    pub rule_name: String,
    #[serde(default)]
    pub enable_push: bool,
    #[serde(default)]
    pub enable_push_whitelist: bool,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub push_whitelist_usernames: Vec<String>,
    #[serde(default)]
    pub require_signed_commits: bool,
    #[serde(default)]
    pub block_on_outdated_branch: bool,
    #[serde(default)]
    pub required_approvals: i64,
    #[serde(default)]
    pub enable_status_check: bool,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub status_check_contexts: Vec<String>,
}

pub async fn list(client: &Client, owner: &str, name: &str) -> Result<Vec<BranchProtection>> {
    let path = format!("{}/branch_protections", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn get(
    client: &Client,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<BranchProtection> {
    let path = format!(
        "{}/branch_protections/{branch}",
        crate::api::repo_path(owner, name)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateBranchProtection<'a> {
    pub branch_name: &'a str,
    #[serde(default)]
    pub enable_push: bool,
    #[serde(default)]
    pub require_signed_commits: bool,
    #[serde(default)]
    pub block_on_outdated_branch: bool,
    #[serde(default)]
    pub required_approvals: i64,
    #[serde(default)]
    pub enable_status_check: bool,
    #[serde(default)]
    pub status_check_contexts: &'a [String],
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateBranchProtection<'_>,
) -> Result<BranchProtection> {
    let path = format!("{}/branch_protections", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, branch: &str) -> Result<()> {
    let path = format!(
        "{}/branch_protections/{branch}",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
