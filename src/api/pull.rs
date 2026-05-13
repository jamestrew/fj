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
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullFile {
    pub filename: String,
    pub status: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changes: u64,
    #[serde(default)]
    pub previous_filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    #[serde(default)]
    pub commit: CommitMeta,
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitMeta {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author: Option<CommitAuthor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitAuthor {
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: u64,
    pub state: String,
    #[serde(default)]
    pub body: String,
    pub user: User,
    pub submitted_at: Option<DateTime<Utc>>,
    pub html_url: String,
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
    // When the caller wants more than the API's per-page cap, transparently
    // follow Link rel=next.
    if opts.limit > 50 {
        let query: Vec<(String, String)> = vec![("state".into(), opts.state.as_str().into())];
        let items = client
            .get_all::<Pull>(&path, &query, opts.limit as usize)
            .await?;
        return Ok(Page::single(items));
    }
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
    #[serde(default)]
    pub draft: bool,
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

/// Convert a draft PR to ready-for-review.
pub async fn ready(client: &Client, owner: &str, name: &str, number: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}");
    // Forgejo accepts `allow_maintainer_edit` and the toggle for `draft` via
    // PATCH; the more reliable form is the dedicated endpoint when present,
    // but PATCH with `{ "draft": false }` works on 7.x.
    #[derive(serde::Serialize)]
    struct ReadyBody {
        draft: bool,
    }
    let body = ReadyBody { draft: false };
    client
        .json::<Pull, _>(Method::PATCH, &path, &[], Some(&body))
        .await
        .map(|_| ())
}

pub async fn files(client: &Client, owner: &str, name: &str, number: u64) -> Result<Vec<PullFile>> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}/files");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn commits(client: &Client, owner: &str, name: &str, number: u64) -> Result<Vec<Commit>> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}/commits");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// Raw unified diff for a PR.
pub async fn diff_text(client: &Client, owner: &str, name: &str, number: u64) -> Result<String> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}.diff");
    let res = client.request(Method::GET, &path, &[], None).await?;
    let res = res.error_for_status()?;
    Ok(res.text().await?)
}

#[derive(Debug, Clone, Copy)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVED",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CreateReviewBody<'a> {
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

pub async fn submit_review(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    event: ReviewEvent,
    body: Option<&str>,
) -> Result<Review> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}/reviews");
    client
        .json(
            Method::POST,
            &path,
            &[],
            Some(&CreateReviewBody {
                event: event.as_str(),
                body,
            }),
        )
        .await
}

pub async fn list_reviews(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<Vec<Review>> {
    let path = format!("/api/v1/repos/{owner}/{name}/pulls/{number}/reviews");
    client.json(Method::GET, &path, &[], None::<&()>).await
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
        .request(
            Method::POST,
            &path,
            &[],
            Some(&serde_json::to_value(&body)?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedStatus {
    #[serde(default)]
    pub state: String,
    pub sha: String,
    #[serde(default)]
    pub statuses: Vec<CommitStatus>,
    #[serde(default)]
    pub total_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub target_url: String,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn combined_status(
    client: &Client,
    owner: &str,
    name: &str,
    sha: &str,
) -> Result<CombinedStatus> {
    let path = format!("/api/v1/repos/{owner}/{name}/commits/{sha}/status");
    client.json(Method::GET, &path, &[], None::<&()>).await
}
