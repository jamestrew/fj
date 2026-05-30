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

// Note: no `code` search here. Forgejo/Gitea expose no code-search API
// endpoint (the UI code search is a web route gated on the repo indexer), so
// `fj search code` was removed rather than left to 404. If a web-route-backed
// implementation is ever added, it belongs in the run-logs-style web layer.
