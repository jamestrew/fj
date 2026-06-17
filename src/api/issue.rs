use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{ensure_success, Client, Page};

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
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub labels: Vec<Label>,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub assignees: Vec<User>,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: u64,
    #[serde(default)]
    pub is_locked: bool,
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
    pub paginate: bool,
    pub labels: Option<&'a str>,
    pub assignee: Option<&'a str>,
    pub milestone: Option<&'a str>,
    pub mentioned: Option<&'a str>,
    pub query: Option<&'a str>,
}

impl Default for ListOptions<'_> {
    fn default() -> Self {
        Self {
            state: State::Open,
            limit: 30,
            page: 1,
            paginate: false,
            labels: None,
            assignee: None,
            milestone: None,
            mentioned: None,
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
    let path = format!("{}/issues", crate::api::repo_path(owner, name));
    let mut query: Vec<(String, String)> = vec![
        ("state".into(), opts.state.as_str().into()),
        ("type".into(), "issues".into()),
    ];
    if let Some(l) = opts.labels {
        query.push(("labels".into(), l.into()));
    }
    if let Some(a) = opts.assignee {
        query.push(("assigned_by".into(), a.into()));
    }
    if let Some(m) = opts.milestone {
        query.push(("milestones".into(), m.into()));
    }
    if let Some(u) = opts.mentioned {
        query.push(("mentioned_by".into(), u.into()));
    }
    if let Some(q) = opts.query {
        query.push(("q".into(), q.into()));
    }
    if opts.paginate || opts.limit > 50 {
        if opts.paginate {
            query.push(("page".into(), opts.page.max(1).to_string()));
            return client.get_all_page::<Issue>(&path, &query, None).await;
        }
        return client
            .get_all_page::<Issue>(&path, &query, Some(opts.limit as usize))
            .await;
    }
    query.push(("limit".into(), opts.limit.clamp(1, 50).to_string()));
    query.push(("page".into(), opts.page.max(1).to_string()));
    client.get_page::<Issue>(&path, &query).await
}

pub async fn get(client: &Client, owner: &str, name: &str, number: u64) -> Result<Issue> {
    let path = format!("{}/issues/{number}", crate::api::repo_path(owner, name));
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
    let path = format!("{}/issues", crate::api::repo_path(owner, name));
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
    let path = format!("{}/issues/{number}", crate::api::repo_path(owner, name));
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize)]
struct AssigneesBody {
    assignees: Vec<String>,
}

/// Forgejo has no `/issues/{n}/assignees` sub-resource (unlike labels); the
/// assignee list is set through the issue PATCH endpoint, which replaces it
/// wholesale. So `add`/`remove` read the current assignees, apply the delta,
/// and PATCH the merged set. Works for PRs too (they are issues here).
async fn set_assignees(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    assignees: Vec<String>,
) -> Result<Issue> {
    let path = format!("{}/issues/{number}", crate::api::repo_path(owner, name));
    client
        .json(
            Method::PATCH,
            &path,
            &[],
            Some(&AssigneesBody { assignees }),
        )
        .await
}

pub async fn add_assignees(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    assignees: Vec<String>,
) -> Result<Issue> {
    let current = get(client, owner, name, number).await?;
    let mut merged: Vec<String> = current.assignees.iter().map(|u| u.login.clone()).collect();
    for login in assignees {
        if !merged.iter().any(|a| a.eq_ignore_ascii_case(&login)) {
            merged.push(login);
        }
    }
    set_assignees(client, owner, name, number, merged).await
}

pub async fn remove_assignees(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    assignees: Vec<String>,
) -> Result<Issue> {
    let current = get(client, owner, name, number).await?;
    let merged: Vec<String> = current
        .assignees
        .iter()
        .map(|u| u.login.clone())
        .filter(|login| !assignees.iter().any(|r| r.eq_ignore_ascii_case(login)))
        .collect();
    set_assignees(client, owner, name, number, merged).await
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
    let path = format!(
        "{}/issues/{number}/comments",
        crate::api::repo_path(owner, name)
    );
    let payload = CreateComment { body };
    client.json(Method::POST, &path, &[], Some(&payload)).await
}

pub async fn list_comments(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<Vec<Comment>> {
    let path = format!(
        "{}/issues/{number}/comments",
        crate::api::repo_path(owner, name)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
struct EditComment<'a> {
    body: &'a str,
}

/// Edit an issue or PR comment by its id (NOT by the issue number).
/// Comment ids come from `list_comments` or from the URL of a comment
/// (e.g. `#issuecomment-7`).
pub async fn edit_comment(
    client: &Client,
    owner: &str,
    name: &str,
    comment_id: u64,
    body: &str,
) -> Result<Comment> {
    let path = format!(
        "{}/issues/comments/{comment_id}",
        crate::api::repo_path(owner, name)
    );
    client
        .json(Method::PATCH, &path, &[], Some(&EditComment { body }))
        .await
}

pub async fn delete_comment(
    client: &Client,
    owner: &str,
    name: &str,
    comment_id: u64,
) -> Result<()> {
    let path = format!(
        "{}/issues/comments/{comment_id}",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    ensure_success(res).await?;
    Ok(())
}

/// Permanently delete an issue. Requires admin on the repo.
pub async fn delete(client: &Client, owner: &str, name: &str, number: u64) -> Result<()> {
    let path = format!("{}/issues/{number}", crate::api::repo_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

/// A file attached to an issue or PR. Forgejo stores attachments under the
/// shared issue index, so this is also the upload path for PR-description
/// files: a PR's number IS its issue index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub created_at: DateTime<Utc>,
}

/// Upload a file as an attachment on issue/PR `index`. Returns the stored
/// attachment, whose `browser_download_url` can be spliced into the body
/// markdown to render the file inline. Mirrors `release::upload_asset`: a
/// single multipart `attachment` part, with auth pulled straight off the
/// client so the right scheme (PAT vs Fjord bearer) is used. The display name
/// rides as a `name` query param (reqwest percent-encodes it).
pub async fn upload_asset(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
    asset_name: &str,
    bytes: Vec<u8>,
) -> Result<Attachment> {
    let path = format!(
        "{}/issues/{index}/assets",
        crate::api::repo_path(owner, name)
    );
    let url = client.url(&path)?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(asset_name.to_string())
        .mime_str("application/octet-stream")?;
    let form = reqwest::multipart::Form::new().part("attachment", part);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, client.auth_header_value()?);
    let res = client
        .http()
        .post(url)
        .query(&[("name", asset_name)])
        .headers(headers)
        .multipart(form)
        .send()
        .await?;
    let res = res.error_for_status()?;
    Ok(res.json().await?)
}

/// Pin (`pinned = true`) or unpin an issue. Works for PRs too, since they
/// share the issue index space.
pub async fn set_pinned(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    pinned: bool,
) -> Result<()> {
    let path = format!("{}/issues/{number}/pin", crate::api::repo_path(owner, name));
    let method = if pinned { Method::POST } else { Method::DELETE };
    let res = client.request(method, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct LockIssueOption<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_reason: Option<&'a str>,
}

pub async fn lock(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    reason: Option<&str>,
) -> Result<()> {
    let path = format!(
        "{}/issues/{number}/lock",
        crate::api::repo_path(owner, name)
    );
    let body = LockIssueOption {
        lock_reason: reason,
    };
    client.send(Method::PUT, &path, &[], Some(&body)).await?;
    Ok(())
}

pub async fn unlock(client: &Client, owner: &str, name: &str, number: u64) -> Result<()> {
    let path = format!(
        "{}/issues/{number}/lock",
        crate::api::repo_path(owner, name)
    );
    client.send(Method::DELETE, &path, &[], None::<&()>).await?;
    Ok(())
}
