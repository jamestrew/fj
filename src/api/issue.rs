use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Client, Page};

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    pub user: User,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub assignees: Vec<User>,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: u64,
    /// Present only when the issue is actually a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: u64,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Closed,
    All,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Open => "open",
            State::Closed => "closed",
            State::All => "all",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListOptions<'a> {
    pub state: State,
    pub limit: u32,
    pub page: u32,
    pub labels: Option<&'a str>,
    pub assignee: Option<&'a str>,
    pub query: Option<&'a str>,
}

impl Default for ListOptions<'_> {
    fn default() -> Self {
        Self {
            state: State::Open,
            limit: 30,
            page: 1,
            labels: None,
            assignee: None,
            query: None,
        }
    }
}

pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    opts: ListOptions<'_>,
) -> Result<Page<Issue>> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues");
    let mut query: Vec<(String, String)> = vec![
        ("state".into(), opts.state.as_str().into()),
        ("limit".into(), opts.limit.clamp(1, 50).to_string()),
        ("page".into(), opts.page.max(1).to_string()),
        ("type".into(), "issues".into()),
    ];
    if let Some(l) = opts.labels {
        query.push(("labels".into(), l.into()));
    }
    if let Some(a) = opts.assignee {
        query.push(("assigned_by".into(), a.into()));
    }
    if let Some(q) = opts.query {
        query.push(("q".into(), q.into()));
    }
    client.get_page::<Issue>(&path, &query).await
}

pub async fn get(client: &Client, owner: &str, name: &str, number: u64) -> Result<Issue> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues/{number}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateIssue<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<u64>>,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateIssue<'_>,
) -> Result<Issue> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues");
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditIssue<'a> {
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
    body: &EditIssue<'_>,
) -> Result<Issue> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues/{number}");
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub body: String,
    pub user: User,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct CreateComment<'a> {
    body: &'a str,
}

pub async fn comment(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    body: &str,
) -> Result<Comment> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues/{number}/comments");
    let payload = CreateComment { body };
    client.json(Method::POST, &path, &[], Some(&payload)).await
}

pub async fn list_comments(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<Vec<Comment>> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues/{number}/comments");
    client.json(Method::GET, &path, &[], None::<&()>).await
}
