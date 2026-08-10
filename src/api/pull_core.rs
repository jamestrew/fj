//! Pull request core: model types, list/get, create/edit, ready, update
//! branch, files, commits, diff, and merge.

use anyhow::{anyhow, Result};
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
    /// Users with a pending review request. Populated by GET pull; empty on
    /// list responses that omit it.
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub requested_reviewers: Vec<User>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: u64,
    #[serde(default)]
    pub is_locked: bool,
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

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub state: State,
    pub limit: u32,
    pub page: u32,
    pub paginate: bool,
    /// Forgejo's `/pulls` endpoint filters labels by ID, not name; the CLI
    /// resolves names up front. Empty/None adds no filter.
    pub labels: Option<Vec<u64>>,
    /// Author (`poster`) login. Forgejo 400s on an unknown user.
    pub author: Option<String>,
    /// Client-side filter: this Forgejo host does not honor `base` on `/pulls`.
    pub base: Option<String>,
    /// Client-side filter: this Forgejo host does not honor `head` on `/pulls`.
    pub head: Option<String>,
    /// Client-side filter: this Forgejo host does not honor `draft` on `/pulls`.
    pub draft: Option<bool>,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            state: State::Open,
            limit: 30,
            page: 1,
            paginate: false,
            labels: None,
            author: None,
            base: None,
            head: None,
            draft: None,
        }
    }
}

pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    opts: ListOptions,
) -> Result<Page<Pull>> {
    let path = format!("{}/pulls", crate::api::repo_path(owner, name));
    let mut query: Vec<(String, String)> = vec![("state".into(), opts.state.as_str().into())];
    if let Some(ids) = opts.labels.as_deref() {
        if !ids.is_empty() {
            let joined = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            query.push(("labels".into(), joined));
        }
    }
    if let Some(author) = opts.author.as_deref() {
        query.push(("poster".into(), author.into()));
    }

    let needs_local_filter = opts.base.is_some() || opts.head.is_some() || opts.draft.is_some();
    if needs_local_filter {
        let mut page = client.get_all_page::<Pull>(&path, &query, None).await?;
        let limit = usize::try_from(opts.limit).map_err(|_| anyhow!("--limit is too large"))?;
        page.items = filter_pulls(page.items, &opts);
        if !opts.paginate && page.items.len() > limit {
            page.items.truncate(limit);
        }
        page.total = Some(page.items.len() as u64);
        page.next = None;
        page.prev = None;
        page.last = None;
        page.first = None;
        return Ok(page);
    }

    if opts.paginate || opts.limit > 50 {
        if opts.paginate {
            query.push(("page".into(), opts.page.max(1).to_string()));
            return client.get_all_page::<Pull>(&path, &query, None).await;
        }
        return client
            .get_all_page::<Pull>(&path, &query, Some(opts.limit as usize))
            .await;
    }
    query.push(("limit".into(), opts.limit.clamp(1, 50).to_string()));
    query.push(("page".into(), opts.page.max(1).to_string()));
    client.get_page::<Pull>(&path, &query).await
}

fn filter_pulls(mut pulls: Vec<Pull>, opts: &ListOptions) -> Vec<Pull> {
    pulls.retain(|p| {
        opts.base
            .as_deref()
            .is_none_or(|base| branch_matches(&p.base, base))
            && opts
                .head
                .as_deref()
                .is_none_or(|head| branch_matches(&p.head, head))
            && opts.draft.is_none_or(|draft| p.draft == draft)
    });
    pulls
}

fn branch_matches(branch: &Branch, value: &str) -> bool {
    branch.label == value || branch.ref_ == value
}

pub async fn get(client: &Client, owner: &str, name: &str, number: u64) -> Result<Pull> {
    let path = format!("{}/pulls/{number}", crate::api::repo_path(owner, name));
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
    let path = format!("{}/pulls", crate::api::repo_path(owner, name));
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
    /// Re-point the PR's base branch (used by `stack ship` to retarget the
    /// next PR onto the trunk after the one below it merges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<&'a str>,
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    body: &EditPull<'_>,
) -> Result<Pull> {
    let path = format!("{}/pulls/{number}", crate::api::repo_path(owner, name));
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

/// Convert a draft PR to ready-for-review.
pub async fn ready(client: &Client, owner: &str, name: &str, number: u64) -> Result<()> {
    let path = format!("{}/pulls/{number}", crate::api::repo_path(owner, name));
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

/// Merge the pull request's base branch into its head (the "update branch"
/// button). `style` is `merge` (default) or `rebase`.
pub async fn update_branch(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    style: &str,
) -> Result<()> {
    let path = format!(
        "{}/pulls/{number}/update",
        crate::api::repo_path(owner, name)
    );
    let q = vec![("style".to_string(), style.to_string())];
    client.send(Method::POST, &path, &q, None::<&()>).await?;
    Ok(())
}

pub async fn files(client: &Client, owner: &str, name: &str, number: u64) -> Result<Vec<PullFile>> {
    let path = format!(
        "{}/pulls/{number}/files",
        crate::api::repo_path(owner, name)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn commits(client: &Client, owner: &str, name: &str, number: u64) -> Result<Vec<Commit>> {
    let path = format!(
        "{}/pulls/{number}/commits",
        crate::api::repo_path(owner, name)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// Raw unified diff for a PR.
pub async fn diff_text(client: &Client, owner: &str, name: &str, number: u64) -> Result<String> {
    let path = format!("{}/pulls/{number}.diff", crate::api::repo_path(owner, name));
    let res = client.request(Method::GET, &path, &[], None).await?;
    let res = res.error_for_status()?;
    Ok(res.text().await?)
}

#[derive(Debug, Clone, Copy)]
pub enum MergeStyle {
    Merge,
    Rebase,
    RebaseMerge,
    Squash,
}

impl MergeStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeStyle::Merge => "merge",
            MergeStyle::Rebase => "rebase",
            MergeStyle::RebaseMerge => "rebase-merge",
            MergeStyle::Squash => "squash",
        }
    }

    pub fn from_str(style: &str) -> Option<Self> {
        match style {
            "merge" => Some(MergeStyle::Merge),
            "rebase" => Some(MergeStyle::Rebase),
            "rebase-merge" => Some(MergeStyle::RebaseMerge),
            "squash" => Some(MergeStyle::Squash),
            _ => None,
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
    /// Delete the head branch after a successful merge (Forgejo-side).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    delete_branch_after_merge: bool,
    /// Schedule the merge to run server-side once all required checks pass.
    /// When the PR is already green the server merges immediately instead.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    merge_when_checks_succeed: bool,
}

/// Optional knobs for [`merge`]. Defaults: auto title/message, keep branch,
/// merge now.
#[derive(Debug, Clone, Default)]
pub struct MergeOptions<'a> {
    pub title: Option<&'a str>,
    pub message: Option<&'a str>,
    pub delete_branch: bool,
    /// Schedule the merge for when required checks pass rather than merging
    /// now (the `--auto` flag). An already-green PR still merges immediately.
    pub auto: bool,
}

pub async fn merge(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    style: MergeStyle,
    opts: &MergeOptions<'_>,
) -> Result<()> {
    let path = format!(
        "{}/pulls/{number}/merge",
        crate::api::repo_path(owner, name)
    );
    let body = MergeBody {
        do_: style.as_str(),
        title: opts.title,
        message: opts.message,
        delete_branch_after_merge: opts.delete_branch,
        merge_when_checks_succeed: opts.auto,
    };
    client.send(Method::POST, &path, &[], Some(&body)).await?;
    Ok(())
}

/// Cancel a pending scheduled auto-merge (the `DELETE …/merge` endpoint).
/// Returns the API's 404 as-is when nothing is scheduled, so the caller can
/// turn it into a friendly "no scheduled auto-merge" message.
pub async fn cancel_auto_merge(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<()> {
    let path = format!(
        "{}/pulls/{number}/merge",
        crate::api::repo_path(owner, name)
    );
    client.send(Method::DELETE, &path, &[], None::<&()>).await?;
    Ok(())
}

/// A single issue/PR timeline entry. We only decode the discriminant `type`
/// so a scheduled auto-merge can be detected; everything else is ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct TimelineEvent {
    #[serde(default, rename = "type")]
    pub type_: String,
}

/// Forgejo timeline `type` for a scheduled auto-merge.
const TL_SCHEDULED: &str = "pull_scheduled_merge";
/// Forgejo timeline `type` for a cancelled scheduled auto-merge.
const TL_CANCELLED: &str = "pull_cancel_scheduled_merge";
/// Forgejo timeline `type` for a completed merge.
const TL_MERGED: &str = "merge_pull";

/// True when the most recent auto-merge-related timeline event is a schedule
/// with no later cancel or merge, i.e. an auto-merge is still pending. Events
/// arrive oldest-first, so the last relevant one wins. Pure for unit testing.
pub fn has_pending_auto_merge(events: &[TimelineEvent]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|e| match e.type_.as_str() {
            TL_SCHEDULED => Some(true),
            TL_CANCELLED | TL_MERGED => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether PR `number` currently has a scheduled auto-merge pending.
///
/// Forgejo does not expose the scheduled state on the PR object, so this reads
/// the issue timeline. A scheduled merge that is still pending is, by
/// definition, not followed by a cancel or merge, so it lands among the most
/// recent events; we scan the last timeline page to keep this to one or two
/// GETs rather than walking the whole history on every `pr view`.
pub async fn auto_merge_pending(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<bool> {
    let path = format!(
        "{}/issues/{number}/timeline",
        crate::api::repo_path(owner, name)
    );
    let q = vec![("limit".to_string(), "50".to_string())];
    let first = client.get_page::<TimelineEvent>(&path, &q).await?;
    let events = match &first.last {
        // Multi-page: the tail (where a pending schedule must be) is the last
        // page. The `last` URL already carries its own query string.
        Some(last_url) => client.get_page::<TimelineEvent>(last_url, &[]).await?.items,
        None => first.items,
    };
    Ok(has_pending_auto_merge(&events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_style_as_str_maps_each_variant() {
        assert_eq!(MergeStyle::Merge.as_str(), "merge");
        assert_eq!(MergeStyle::Rebase.as_str(), "rebase");
        assert_eq!(MergeStyle::RebaseMerge.as_str(), "rebase-merge");
        assert_eq!(MergeStyle::Squash.as_str(), "squash");
    }

    fn ev(type_: &str) -> TimelineEvent {
        TimelineEvent {
            type_: type_.to_string(),
        }
    }

    #[test]
    fn merge_body_omits_auto_field_when_not_scheduling() {
        let body = MergeBody {
            do_: "merge",
            title: None,
            message: None,
            delete_branch_after_merge: false,
            merge_when_checks_succeed: false,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v, serde_json::json!({ "Do": "merge" }));
    }

    #[test]
    fn merge_body_sets_auto_field_when_scheduling() {
        let body = MergeBody {
            do_: "squash",
            title: None,
            message: None,
            delete_branch_after_merge: false,
            merge_when_checks_succeed: true,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["merge_when_checks_succeed"], serde_json::json!(true));
    }

    #[test]
    fn pending_when_last_event_is_a_schedule() {
        let events = vec![ev("pull_push"), ev("pull_scheduled_merge")];
        assert!(has_pending_auto_merge(&events));
    }

    #[test]
    fn not_pending_after_cancel() {
        let events = vec![
            ev("pull_scheduled_merge"),
            ev("pull_cancel_scheduled_merge"),
        ];
        assert!(!has_pending_auto_merge(&events));
    }

    #[test]
    fn not_pending_after_merge() {
        let events = vec![ev("pull_scheduled_merge"), ev("merge_pull")];
        assert!(!has_pending_auto_merge(&events));
    }

    #[test]
    fn reschedule_after_cancel_is_pending() {
        let events = vec![
            ev("pull_scheduled_merge"),
            ev("pull_cancel_scheduled_merge"),
            ev("pull_scheduled_merge"),
        ];
        assert!(has_pending_auto_merge(&events));
    }

    #[test]
    fn no_auto_merge_events_is_not_pending() {
        let events = vec![ev("comment"), ev("pull_push"), ev("label")];
        assert!(!has_pending_auto_merge(&events));
    }
}
