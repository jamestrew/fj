//! Pull request reviews: submit a review, request/unrequest reviewers, and
//! list reviews.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: u64,
    pub state: String,
    #[serde(default)]
    pub body: String,
    pub user: User,
    pub submitted_at: Option<DateTime<Utc>>,
    pub html_url: String,
    /// The review counts toward the base branch's required approvals.
    #[serde(default)]
    pub official: bool,
    #[serde(default)]
    pub dismissed: bool,
    /// Submitted against a commit that is no longer the head.
    #[serde(default)]
    pub stale: bool,
    /// Inline (diff line) comments the server recorded on this review.
    #[serde(default)]
    pub comments_count: u64,
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

/// Which version of the file an inline comment's line number counts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSide {
    /// The base version: a removed (or unchanged) line, numbered as in the
    /// file before the change.
    Old,
    /// The pull request's version: an added (or unchanged) line, numbered as
    /// in the file after the change.
    New,
}

impl DiffSide {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffSide::Old => "old",
            DiffSide::New => "new",
        }
    }
}

/// One inline comment anchored to a single line of a changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineComment {
    pub path: String,
    pub side: DiffSide,
    /// 1-based file line number on `side` (not an offset into the diff).
    pub line: u64,
    pub body: String,
}

/// Wire shape of `CreatePullReviewComment`. Forgejo reads `old_position` and
/// `new_position` as file line numbers and treats a non-zero `old_position`
/// as an old-side anchor, so exactly one of the two is ever non-zero here.
/// `extra_lines_count` (a multi-line range) is deliberately never sent.
#[derive(Debug, Clone, Serialize)]
struct CreateReviewComment<'a> {
    path: &'a str,
    body: &'a str,
    old_position: u64,
    new_position: u64,
}

impl<'a> From<&'a LineComment> for CreateReviewComment<'a> {
    fn from(c: &'a LineComment) -> Self {
        let (old_position, new_position) = match c.side {
            DiffSide::Old => (c.line, 0),
            DiffSide::New => (0, c.line),
        };
        CreateReviewComment {
            path: &c.path,
            body: &c.body,
            old_position,
            new_position,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CreateReviewBody<'a> {
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    comments: Vec<CreateReviewComment<'a>>,
}

pub async fn submit_review(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    event: ReviewEvent,
    body: Option<&str>,
) -> Result<Review> {
    submit_review_with_comments(client, owner, name, number, event, body, &[]).await
}

/// Submit a review whose inline comments travel in the same request as the
/// verdict, so the batch lands as one review or not at all.
pub async fn submit_review_with_comments(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    event: ReviewEvent,
    body: Option<&str>,
    comments: &[LineComment],
) -> Result<Review> {
    let path = format!(
        "{}/pulls/{number}/reviews",
        crate::api::repo_path(owner, name)
    );
    client
        .json(
            Method::POST,
            &path,
            &[],
            Some(&CreateReviewBody {
                event: event.as_str(),
                body,
                comments: comments.iter().map(CreateReviewComment::from).collect(),
            }),
        )
        .await
}

#[derive(Debug, Clone, Serialize)]
struct ReviewerBody {
    reviewers: Vec<String>,
}

/// Request reviews from specific users on a PR. Distinct from
/// `submit_review`, which is your own approval/changes/comment.
pub async fn request_reviewers(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    reviewers: Vec<String>,
) -> Result<()> {
    let path = format!(
        "{}/pulls/{number}/requested_reviewers",
        crate::api::repo_path(owner, name)
    );
    let res = client
        .request(
            Method::POST,
            &path,
            &[],
            Some(&serde_json::to_value(ReviewerBody { reviewers })?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn unrequest_reviewers(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    reviewers: Vec<String>,
) -> Result<()> {
    let path = format!(
        "{}/pulls/{number}/requested_reviewers",
        crate::api::repo_path(owner, name)
    );
    let res = client
        .request(
            Method::DELETE,
            &path,
            &[],
            Some(&serde_json::to_value(ReviewerBody { reviewers })?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn list_reviews(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<Vec<Review>> {
    let path = format!(
        "{}/pulls/{number}/reviews",
        crate::api::repo_path(owner, name)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// Reviews gathered by [`list_reviews_all`]. `complete` is false when the
/// page cap was reached before the server ran out of reviews, so a caller
/// counting approvals must not treat the list as the whole story.
#[derive(Debug, Clone)]
pub struct ReviewScan {
    pub reviews: Vec<Review>,
    pub complete: bool,
}

/// Every review on a PR, walking pages until an empty one. An empty page is
/// the only end marker trusted: a short page proves nothing when the server
/// caps the page size below what was asked for.
pub async fn list_reviews_all(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<ReviewScan> {
    let path = format!(
        "{}/pulls/{number}/reviews",
        crate::api::repo_path(owner, name)
    );
    let mut reviews: Vec<Review> = Vec::new();
    for page in 1..=crate::api::pull_status::SCAN_MAX_PAGES {
        let query = crate::api::pull_status::page_query(page);
        let batch = client.get_page::<Review>(&path, &query).await?.items;
        if batch.is_empty() {
            return Ok(ReviewScan {
                reviews,
                complete: true,
            });
        }
        let before = reviews.len();
        for r in batch {
            if !reviews.iter().any(|seen| seen.id == r.id) {
                reviews.push(r);
            }
        }
        // A page that repeats what was already seen means the server ignored
        // `page`; stop rather than loop, and do not claim completeness.
        if reviews.len() == before {
            break;
        }
    }
    Ok(ReviewScan {
        reviews,
        complete: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_event_as_str_maps_each_variant() {
        assert_eq!(ReviewEvent::Approve.as_str(), "APPROVED");
        assert_eq!(ReviewEvent::RequestChanges.as_str(), "REQUEST_CHANGES");
        assert_eq!(ReviewEvent::Comment.as_str(), "COMMENT");
    }
}
