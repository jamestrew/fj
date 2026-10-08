//! Emoji reactions on an issue or pull request body and on a comment.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub content: String,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// What is being reacted to. A comment is addressed by its own id, not by the
/// number of the issue it sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The body of issue or pull request `index`.
    Issue(u64),
    Comment(u64),
}

impl Target {
    fn path(self, owner: &str, name: &str) -> String {
        let repo = crate::api::repo_path(owner, name);
        match self {
            Target::Issue(index) => format!("{repo}/issues/{index}/reactions"),
            Target::Comment(id) => format!("{repo}/issues/comments/{id}/reactions"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct EditReaction<'a> {
    content: &'a str,
}

/// Every reaction on the target. The issue endpoint paginates and the comment
/// endpoint does not; following `Link: rel=next` covers both.
pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    target: Target,
) -> Result<Vec<Reaction>> {
    let page = client
        .get_all_page::<Reaction>(&target.path(owner, name), &[], None)
        .await?;
    Ok(page.items)
}

pub async fn add(
    client: &Client,
    owner: &str,
    name: &str,
    target: Target,
    content: &str,
) -> Result<Reaction> {
    client
        .json(
            Method::POST,
            &target.path(owner, name),
            &[],
            Some(&EditReaction { content }),
        )
        .await
}

/// Remove the signed-in account's reaction. The DELETE must carry a body
/// naming the reaction; a bodiless DELETE names nothing and removes nothing.
pub async fn remove(
    client: &Client,
    owner: &str,
    name: &str,
    target: Target,
    content: &str,
) -> Result<()> {
    client
        .send(
            Method::DELETE,
            &target.path(owner, name),
            &[],
            Some(&EditReaction { content }),
        )
        .await?;
    Ok(())
}
