use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Client, Page};

use super::issue::{Label, State};
use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pull {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    pub user: User,
    #[serde(default)]
    pub labels: Vec<Label>,
    pub html_url: String,
    pub head: Branch,
    pub base: Branch,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub mergeable: bool,
    #[serde(default)]
    pub merged: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "ref")]
    pub ref_: String,
    #[serde(default)]
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub state: State,
    pub limit: u32,
    pub page: u32,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            state: State::Open,
            limit: 30,
            page: 1,
        }
    }
}

pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    opts: ListOptions,
) -> Result<Page<Pull>> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls");
    let query: Vec<(String, String)> = vec![
        ("state".into(), opts.state.as_str().into()),
        ("limit".into(), opts.limit.clamp(1, 50).to_string()),
        ("page".into(), opts.page.max(1).to_string()),
    ];
    client.get_page::<Pull>(&path, &query).await
}

pub async fn get(client: &Client, owner: &str, name: &str, number: u64) -> Result<Pull> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePull<'a> {
    pub title: &'a str,
    pub head: &'a str,
    pub base: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreatePull<'_>,
) -> Result<Pull> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls");
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditPull<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a str>,
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    body: &EditPull<'_>,
) -> Result<Pull> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}");
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Copy)]
pub enum MergeStyle {
    Merge,
    Rebase,
    RebaseMerge,
    Squash,
}

impl MergeStyle {
    fn as_str(self) -> &'static str {
        match self {
            MergeStyle::Merge => "merge",
            MergeStyle::Rebase => "rebase",
            MergeStyle::RebaseMerge => "rebase-merge",
            MergeStyle::Squash => "squash",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct MergeBody<'a> {
    #[serde(rename = "Do")]
    do_: &'a str,
    #[serde(skip_serializing_if = "Option::is_none", rename = "MergeTitleField")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "MergeMessageField")]
    message: Option<&'a str>,
}

pub async fn merge(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    style: MergeStyle,
    title: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}/merge");
    let body = MergeBody {
        do_: style.as_str(),
        title,
        message,
    };
    let res = client
        .request(Method::POST, &path, &[], Some(&serde_json::to_value(&body)?))
        .await?;
    res.error_for_status()?;
    Ok(())
}
