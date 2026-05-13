use anyhow::Result;
use reqwest::Method;
use serde::Deserialize;

use crate::client::Client;

use super::issue::Issue;

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: Vec<T>,
    #[serde(default)]
    #[allow(dead_code)]
    ok: bool,
}

pub async fn issues(client: &Client, query: &str, type_: &str, limit: u32) -> Result<Vec<Issue>> {
    let path = "/api/v1/repos/issues/search";
    let q = vec![
        ("q".into(), query.into()),
        ("type".into(), type_.into()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    client.json(Method::GET, path, &q, None::<&()>).await
}

pub async fn repos(client: &Client, query: &str, limit: u32) -> Result<Vec<super::repo::Repo>> {
    let path = "/api/v1/repos/search";
    let q = vec![
        ("q".into(), query.into()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    let body: Envelope<super::repo::Repo> = client.json(Method::GET, path, &q, None::<&()>).await?;
    Ok(body.data)
}

pub async fn users(client: &Client, query: &str, limit: u32) -> Result<Vec<super::user::User>> {
    let path = "/api/v1/users/search";
    let q = vec![
        ("q".into(), query.into()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    let body: Envelope<super::user::User> = client.json(Method::GET, path, &q, None::<&()>).await?;
    Ok(body.data)
}

/// A single hit in a code search. Forgejo's search-code endpoint returns
/// matches grouped by file with `formatted_lines` containing a highlighted
/// snippet. We deserialize what we need and stay forward-compatible by
/// allowing unknown fields.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct CodeHit {
    #[serde(default)]
    pub repo_id: u64,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub formatted_lines: String,
    #[serde(default)]
    pub raw_lines: String,
    #[serde(default)]
    pub matches: u64,
    #[serde(default)]
    pub language: String,
}

#[derive(Debug, Deserialize)]
struct CodeSearchResponse {
    #[serde(default)]
    results: Vec<CodeHit>,
    #[serde(default)]
    #[allow(dead_code)]
    total: u64,
}

/// Search code across all repos visible to the authenticated user.
pub async fn code(client: &Client, query: &str, limit: u32) -> Result<Vec<CodeHit>> {
    let path = "/api/v1/repos/search/code";
    let q = vec![
        ("q".into(), query.into()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    let body: CodeSearchResponse = client.json(Method::GET, path, &q, None::<&()>).await?;
    Ok(body.results)
}

/// Search code restricted to one repository.
pub async fn code_in_repo(
    client: &Client,
    owner: &str,
    name: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<CodeHit>> {
    let path = format!("/api/v1/repos/{owner}/{name}/search/code");
    let q = vec![
        ("q".into(), query.into()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    let body: CodeSearchResponse = client.json(Method::GET, &path, &q, None::<&()>).await?;
    Ok(body.results)
}
