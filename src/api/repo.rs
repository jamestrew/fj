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
    pub query: Option<&'a str>,
}

pub async fn list_for_user(client: &Client, opts: ListOptions<'_>) -> Result<Page<Repo>> {
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
        .request(
            Method::GET,
            "/api/v1/repos/search",
            &query,
            None,
        )
        .await?;
    let headers = res.headers().clone();
    let body: SearchResponse = res.error_for_status()?.json().await?;
    Ok(Page::from_headers(body.data, &headers).also_total(body.ok))
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SearchHit>,
    #[serde(default)]
    ok: bool,
}

impl<T> Page<T> {
    fn also_total(self, _ok: bool) -> Self {
        self
    }
}

pub type SearchHit = Repo;

pub async fn get(client: &Client, owner: &str, name: &str) -> Result<Repo> {
    let path = format!("/api/v1/repos/{owner}/{name}");
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

pub async fn create_for_org(
    client: &Client,
    org: &str,
    body: &CreateRepo<'_>,
) -> Result<Repo> {
    let path = format!("/api/v1/orgs/{org}/repos");
    client.json(Method::POST, &path, &[], Some(body)).await
}
