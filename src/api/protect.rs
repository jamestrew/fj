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
        "{}/branch_protections/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(branch)
    );
    client
        .json(Method::GET, &path, &[], None::<&()>)
        .await
        .map_err(|e| {
            crate::client::with_not_found_context(
                e,
                format!("branch protection for '{branch}' not found"),
            )
        })
}

/// The protection Forgejo applies to one branch, as `GET .../branches/{branch}`
/// reports it. Unlike the `branch_protections` endpoints this needs only read
/// access, so it is what a non-admin can learn about a merge gate. It carries
/// the merge permission, required contexts and required approvals; the rest of
/// the rule (rejected reviews, outdated branch, protected files, signed
/// commits) is not in it. Fields the server omitted stay `None` so absence is
/// never read as "not required".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectiveProtection {
    #[serde(default)]
    pub protected: Option<bool>,
    #[serde(default)]
    pub effective_branch_protection_name: Option<String>,
    #[serde(default)]
    pub required_approvals: Option<i64>,
    #[serde(default)]
    pub enable_status_check: Option<bool>,
    #[serde(default)]
    pub status_check_contexts: Option<Vec<String>>,
    #[serde(default)]
    pub user_can_merge: Option<bool>,
    #[serde(default)]
    pub user_can_push: Option<bool>,
}

pub async fn effective(
    client: &Client,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<EffectiveProtection> {
    let path = format!(
        "{}/branches/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(branch)
    );
    client
        .json(Method::GET, &path, &[], None::<&()>)
        .await
        .map_err(|e| {
            crate::client::with_not_found_context(e, format!("branch '{branch}' not found"))
        })
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
        "{}/branch_protections/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(branch)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
