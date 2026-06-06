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
