//! Core repository operations: list, search, get, create, edit, delete,
//! fork, rename, sync, migrate, mirror, branches, and topics.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Client, Page};

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: User,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub mirror: bool,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub stars_count: u64,
    #[serde(default)]
    pub forks_count: u64,
    #[serde(default)]
    pub open_issues_count: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ListOptions<'a> {
    pub limit: u32,
    pub page: u32,
    pub paginate: bool,
    pub query: Option<&'a str>,
}

pub async fn list_for_user(client: &Client, opts: ListOptions<'_>) -> Result<Page<Repo>> {
    if opts.paginate || opts.limit > 50 {
        if opts.paginate {
            let query = vec![("page".into(), opts.page.max(1).to_string())];
            return client
                .get_all_page::<Repo>("/api/v1/user/repos", &query, None)
                .await;
        }
        return client
            .get_all_page::<Repo>("/api/v1/user/repos", &[], Some(opts.limit as usize))
            .await;
    }
    let limit = opts.limit.clamp(1, 50);
    let page = opts.page.max(1);
    let query: Vec<(String, String)> = vec![
        ("limit".into(), limit.to_string()),
        ("page".into(), page.to_string()),
    ];
    client.get_page::<Repo>("/api/v1/user/repos", &query).await
}

pub async fn search(client: &Client, opts: ListOptions<'_>) -> Result<Page<SearchHit>> {
    let limit = opts.limit.clamp(1, 50);
    let page = opts.page.max(1);
    let mut query: Vec<(String, String)> = vec![
        ("limit".into(), limit.to_string()),
        ("page".into(), page.to_string()),
    ];
    if let Some(q) = opts.query {
        query.push(("q".into(), q.into()));
    }
    let res = client
        .request(Method::GET, "/api/v1/repos/search", &query, None)
        .await?;
    let headers = res.headers().clone();
    let body: SearchResponse = res.error_for_status()?.json().await?;
    Ok(Page::from_headers(body.data, &headers))
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    data: Vec<SearchHit>,
    #[serde(default)]
    #[allow(dead_code)]
    ok: bool,
}

pub type SearchHit = Repo;

pub async fn get(client: &Client, owner: &str, name: &str) -> Result<Repo> {
    let path = crate::api::repo_path(owner, name);
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateRepo<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(default)]
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<&'a str>,
    #[serde(default)]
    pub auto_init: bool,
}

pub async fn create_for_current_user(client: &Client, body: &CreateRepo<'_>) -> Result<Repo> {
    client
        .json(Method::POST, "/api/v1/user/repos", &[], Some(body))
        .await
}

pub async fn create_for_org(client: &Client, org: &str, body: &CreateRepo<'_>) -> Result<Repo> {
    let path = format!("/api/v1/orgs/{}/repos", crate::api::path_segment(org));
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditRepo<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

pub async fn edit(client: &Client, owner: &str, name: &str, body: &EditRepo<'_>) -> Result<Repo> {
    let path = crate::api::repo_path(owner, name);
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str) -> Result<()> {
    let path = crate::api::repo_path(owner, name);
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct ForkBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    organization: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

pub async fn fork(
    client: &Client,
    owner: &str,
    name: &str,
    organization: Option<&str>,
    new_name: Option<&str>,
) -> Result<Repo> {
    let path = format!("{}/forks", crate::api::repo_path(owner, name));
    client
        .json(
            Method::POST,
            &path,
            &[],
            Some(&ForkBody {
                organization,
                name: new_name,
            }),
        )
        .await
}

pub async fn rename(client: &Client, owner: &str, name: &str, new_name: &str) -> Result<()> {
    // Forgejo: PATCH /repos/{owner}/{name} with `name` field.
    let path = crate::api::repo_path(owner, name);
    #[derive(Serialize)]
    struct Body<'a> {
        name: &'a str,
    }
    let res = client
        .request(
            Method::PATCH,
            &path,
            &[],
            Some(&serde_json::to_value(Body { name: new_name })?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeUpstream<'a> {
    pub branch: &'a str,
}

pub async fn sync_with_upstream(
    client: &Client,
    owner: &str,
    name: &str,
    branch: &str,
) -> Result<()> {
    let path = format!("{}/merge-upstream", crate::api::repo_path(owner, name));
    let res = client
        .request(
            Method::POST,
            &path,
            &[],
            Some(&serde_json::to_value(MergeUpstream { branch })?),
        )
        .await?;
    // `merge-upstream` is a Gitea endpoint that some Forgejo builds (e.g.
    // 15.0.x) don't expose. Translate the 404 into an actionable message
    // rather than a bare "404 Not Found".
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow::anyhow!(
            "this Forgejo build has no `merge-upstream` endpoint, so `fj repo sync` \
             can't run server-side. Pull the upstream branch and push it manually, \
             or upgrade the server."
        ));
    }
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    #[serde(default)]
    pub protected: bool,
    pub commit: BranchCommit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchCommit {
    pub id: String,
    #[serde(default)]
    pub message: String,
}

pub async fn list_branches(client: &Client, owner: &str, name: &str) -> Result<Vec<Branch>> {
    let path = format!("{}/branches", crate::api::repo_path(owner, name));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    #[serde(default)]
    pub topics: Vec<String>,
}

pub async fn list_topics(client: &Client, owner: &str, name: &str) -> Result<Vec<String>> {
    let path = format!("{}/topics", crate::api::repo_path(owner, name));
    let t: Topic = client.json(Method::GET, &path, &[], None::<&()>).await?;
    Ok(t.topics)
}

pub async fn set_topics(client: &Client, owner: &str, name: &str, topics: &[String]) -> Result<()> {
    let path = format!("{}/topics", crate::api::repo_path(owner, name));
    let res = client
        .request(
            Method::PUT,
            &path,
            &[],
            Some(&serde_json::json!({ "topics": topics })),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Migrate<'a> {
    pub clone_addr: &'a str,
    pub repo_name: &'a str,
    /// User or org login that will own the migrated repo.
    pub repo_owner: &'a str,
    #[serde(default)]
    pub mirror: bool,
    #[serde(default)]
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_password: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<&'a str>,
    /// Service: "git", "github", "gitea", "gitlab", "gogs", "onedev", ...
    pub service: &'a str,
    /// For pull-mirror: how often to refresh from the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_interval: Option<&'a str>,
}

pub async fn migrate(client: &Client, body: &Migrate<'_>) -> Result<Repo> {
    client
        .json(Method::POST, "/api/v1/repos/migrate", &[], Some(body))
        .await
}

pub async fn mirror_sync(client: &Client, owner: &str, name: &str) -> Result<()> {
    let path = format!("{}/mirror-sync", crate::api::repo_path(owner, name));
    let res = client.request(Method::POST, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
