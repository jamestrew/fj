use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::path_segment;
use crate::client::{ensure_success, Client};

/// Forgejo's per-page cap for list endpoints.
const PER_PAGE_MAX: usize = 50;
/// Upper bound on pages one inbox fetch will walk. Only reachable when a
/// local filter hides nearly everything; it keeps a fully muted inbox from
/// turning into an unbounded crawl.
const MAX_PAGES: u32 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: u64,
    pub unread: bool,
    pub pinned: bool,
    pub updated_at: DateTime<Utc>,
    pub url: String,
    #[serde(default)]
    pub html_url: String,
    pub subject: Subject,
    pub repository: Option<NotificationRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub latest_comment_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRepo {
    pub full_name: String,
    #[serde(default)]
    pub html_url: String,
}

/// One fetch of the inbox after a local filter has been applied.
#[derive(Debug)]
pub struct Inbox {
    pub items: Vec<Notification>,
    /// Threads the filter dropped from the pages that were fetched.
    pub hidden: usize,
    /// The page cap was hit before `limit` visible threads were found.
    pub truncated: bool,
}

pub async fn list(client: &Client, all: bool, limit: u32) -> Result<Vec<Notification>> {
    Ok(list_filtered(client, all, limit, |_| true).await?.items)
}

/// List the inbox, dropping every thread `keep` rejects from each fetched
/// page, and keep paging until `limit` kept threads are collected or the
/// server runs out. Server order (newest first) is preserved.
pub async fn list_filtered(
    client: &Client,
    all: bool,
    limit: u32,
    keep: impl Fn(&Notification) -> bool,
) -> Result<Inbox> {
    let path = "/api/v1/notifications";
    let limit = limit.max(1) as usize;
    let per_page = limit.min(PER_PAGE_MAX);
    let mut inbox = Inbox {
        items: Vec::new(),
        hidden: 0,
        truncated: false,
    };
    let mut page = 1u32;
    loop {
        let mut q = vec![("limit".into(), per_page.to_string())];
        if all {
            q.push(("all".into(), "true".into()));
        }
        if page > 1 {
            q.push(("page".into(), page.to_string()));
        }
        let batch = client.get_page::<Notification>(path, &q).await?.items;
        let fetched = batch.len();
        for thread in batch {
            if !keep(&thread) {
                inbox.hidden += 1;
            } else if inbox.items.len() < limit {
                inbox.items.push(thread);
            }
        }
        if inbox.items.len() >= limit || fetched < per_page {
            break;
        }
        if page >= MAX_PAGES {
            inbox.truncated = true;
            break;
        }
        page += 1;
    }
    Ok(inbox)
}

pub async fn mark_all_read(client: &Client) -> Result<()> {
    let path = "/api/v1/notifications";
    let res = client.request(Method::PUT, path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn get_thread(client: &Client, id: u64) -> Result<Notification> {
    let path = format!("/api/v1/notifications/threads/{id}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// Mark one thread read. Forgejo answers 205 Reset Content, with or without a
/// body depending on what sits in front of it, so the body is never decoded;
/// callers confirm the result with [`get_thread`].
pub async fn mark_thread_read(client: &Client, id: u64) -> Result<()> {
    let path = format!("/api/v1/notifications/threads/{id}");
    client.send(Method::PATCH, &path, &[], None::<&()>).await?;
    Ok(())
}

/// The issue or pull request a thread is about, which is what a thread
/// subscription is actually attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionTarget {
    pub owner: String,
    pub repo: String,
    pub index: u64,
}

/// Pull the issue or pull request number out of a subject URL, accepting both
/// the web form (`/o/r/issues/5`) and the API form (`/repos/o/r/pulls/5`).
fn subject_index(url: &str) -> Option<u64> {
    let path = url.split(['#', '?']).next()?.trim_end_matches('/');
    let mut segments = path.rsplit('/');
    let index = segments.next()?.parse::<u64>().ok().filter(|n| *n > 0)?;
    let kind = segments.next()?.to_ascii_lowercase();
    matches!(kind.as_str(), "issues" | "pulls").then_some(index)
}

/// Resolve what a thread subscribes the user to, or explain why it has no
/// subscription to remove. Only issue and pull request threads do; a
/// repository or commit thread follows the repository watch instead.
pub fn subscription_target(thread: &Notification) -> Result<SubscriptionTarget> {
    let id = thread.id;
    let kind = thread.subject.type_.to_ascii_lowercase();
    if !matches!(kind.as_str(), "issue" | "pull") {
        let kind = if kind.is_empty() { "unknown" } else { &kind };
        bail!(
            "thread {id} is a {kind} notification; only issue and pull request threads \
             have a subscription to remove (repository watching is `fj repo unwatch`)"
        );
    }
    let full_name = thread
        .repository
        .as_ref()
        .map(|r| r.full_name.as_str())
        .unwrap_or_default();
    let (owner, repo) = crate::api::split_repo(full_name)
        .map_err(|_| anyhow!("thread {id} has no resolvable repository; cannot unsubscribe"))?;
    let index = subject_index(&thread.subject.html_url)
        .or_else(|| subject_index(&thread.subject.url))
        .ok_or_else(|| {
            anyhow!(
                "thread {id} does not resolve to an issue or pull request number; \
                 cannot unsubscribe"
            )
        })?;
    Ok(SubscriptionTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        index,
    })
}

/// What the server said about a subscription delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsubscribed {
    /// 201: a subscription existed and was removed.
    Removed,
    /// 200: the user was not subscribed in the first place.
    AlreadyUnsubscribed,
}

pub async fn delete_subscription(
    client: &Client,
    target: &SubscriptionTarget,
    user: &str,
) -> Result<Unsubscribed> {
    let path = format!(
        "/api/v1/repos/{}/{}/issues/{}/subscriptions/{}",
        path_segment(&target.owner),
        path_segment(&target.repo),
        target.index,
        path_segment(user)
    );
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    // Forgejo answers 304 (no body) when the caller may not change this
    // user's subscription, which would otherwise surface as a bare status.
    if res.status() == StatusCode::NOT_MODIFIED {
        bail!("the server refused: {user} can only change their own subscription");
    }
    let res = ensure_success(res).await?;
    Ok(if res.status() == StatusCode::CREATED {
        Unsubscribed::Removed
    } else {
        Unsubscribed::AlreadyUnsubscribed
    })
}

/// The server-confirmed result of unsubscribing from a thread.
#[derive(Debug)]
pub struct UnsubscribeReport {
    pub target: SubscriptionTarget,
    pub user: String,
    pub outcome: Unsubscribed,
    /// The thread as the server reports it after the whole operation.
    pub thread: Notification,
}

/// Unsubscribe the current user from the issue or pull request behind a
/// thread, and mark the thread read if it was unread.
///
/// Nothing is deleted unless the thread resolves to an issue or pull request
/// and the current user's login is known. Whatever happens after that, the
/// thread is re-read so the caller reports server truth, not intent.
pub async fn unsubscribe_thread(client: &Client, id: u64) -> Result<UnsubscribeReport> {
    let before = get_thread(client, id).await?;
    let target = subscription_target(&before)?;
    let user = crate::api::user::current(client)
        .await
        .map(|u| u.login)
        .context("could not resolve the current user; not unsubscribing")?;
    if user.trim().is_empty() {
        bail!("could not resolve the current user; not unsubscribing");
    }

    let outcome = match delete_subscription(client, &target, &user).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let state = match get_thread(client, id).await {
                Ok(t) if t.unread => "the thread is still unread on the server",
                Ok(_) => "the thread is read on the server",
                Err(_) => "the thread could not be re-read",
            };
            return Err(err.context(format!(
                "unsubscribing from {}/{}#{} failed; {state}",
                target.owner, target.repo, target.index
            )));
        }
    };
    if before.unread {
        mark_thread_read(client, id).await.with_context(|| {
            format!(
                "unsubscribed from {}/{}#{}, but marking thread {id} read failed",
                target.owner, target.repo, target.index
            )
        })?;
    }
    let thread = get_thread(client, id).await?;
    Ok(UnsubscribeReport {
        target,
        user,
        outcome,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_deserializes_when_html_urls_are_missing() {
        let n: Notification = serde_json::from_value(serde_json::json!({
            "id": 1,
            "unread": true,
            "pinned": false,
            "updated_at": "2026-01-01T00:00:00Z",
            "url": "https://forge.example/api/v1/notifications/1",
            "subject": {
                "title": "Build failed",
                "type": "Issue",
                "state": "open"
            },
            "repository": {
                "full_name": "o/r"
            }
        }))
        .expect("notification without html_url fields should deserialize");

        assert_eq!(n.html_url, "");
        assert_eq!(n.subject.html_url, "");
        assert_eq!(n.repository.unwrap().html_url, "");
    }

    fn thread(kind: &str, repo: Option<&str>, html_url: &str, url: &str) -> Notification {
        serde_json::from_value(serde_json::json!({
            "id": 7,
            "unread": true,
            "pinned": false,
            "updated_at": "2026-01-01T00:00:00Z",
            "url": "https://forge.example/api/v1/notifications/threads/7",
            "subject": {
                "title": "t", "type": kind, "state": "open",
                "html_url": html_url, "url": url
            },
            "repository": repo.map(|r| serde_json::json!({ "full_name": r }))
        }))
        .unwrap()
    }

    #[test]
    fn subject_index_reads_web_and_api_urls() {
        assert_eq!(subject_index("https://f.example/o/r/issues/5"), Some(5));
        assert_eq!(
            subject_index("https://f.example/api/v1/repos/o/r/pulls/12/"),
            Some(12)
        );
        assert_eq!(
            subject_index("https://f.example/o/r/pulls/3#issuecomment-9"),
            Some(3)
        );
        assert_eq!(subject_index("https://f.example/o/r/issues/8?x=1"), Some(8));
        assert_eq!(subject_index("https://f.example/o/r/commit/5"), None);
        assert_eq!(subject_index("https://f.example/o/r/issues/0"), None);
        assert_eq!(subject_index("https://f.example/o/r"), None);
        assert_eq!(subject_index(""), None);
    }

    #[test]
    fn subscription_target_resolves_issue_and_pull_threads() {
        let issue = thread("Issue", Some("o/r"), "https://f.example/o/r/issues/5", "");
        assert_eq!(
            subscription_target(&issue).unwrap(),
            SubscriptionTarget {
                owner: "o".into(),
                repo: "r".into(),
                index: 5
            }
        );
        // No web URL: fall back to the API URL.
        let pull = thread(
            "Pull",
            Some("o/r"),
            "",
            "https://f.example/api/v1/repos/o/r/pulls/9",
        );
        assert_eq!(subscription_target(&pull).unwrap().index, 9);
    }

    #[test]
    fn subscription_target_refuses_repository_and_commit_threads() {
        for kind in ["Repository", "Commit", ""] {
            let t = thread(kind, Some("o/r"), "https://f.example/o/r/issues/5", "");
            let err = subscription_target(&t).unwrap_err().to_string();
            assert!(err.contains("only issue and pull request"), "{kind}: {err}");
        }
    }

    #[test]
    fn subscription_target_refuses_unresolvable_threads() {
        let no_repo = thread("Issue", None, "https://f.example/o/r/issues/5", "");
        let err = subscription_target(&no_repo).unwrap_err().to_string();
        assert!(err.contains("no resolvable repository"), "{err}");

        let no_index = thread("Issue", Some("o/r"), "https://f.example/o/r", "");
        let err = subscription_target(&no_index).unwrap_err().to_string();
        assert!(err.contains("does not resolve"), "{err}");
    }
}
