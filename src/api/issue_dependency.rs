//! Issue and pull request dependencies. Forgejo keeps both directions on the
//! issue index: `dependencies` is what the item waits on, `blocks` is what
//! waits on it. Pull requests share the index, so every call here takes a PR
//! number as readily as an issue number.

use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

/// Which way a relationship points, seen from the work item in the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The work item in the path waits on the other one.
    DependsOn,
    /// The other one waits on the work item in the path.
    Blocks,
}

impl Direction {
    fn segment(self) -> &'static str {
        match self {
            Direction::DependsOn => "dependencies",
            Direction::Blocks => "blocks",
        }
    }
}

/// `IssueMeta`: the request body that names the other work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueMeta {
    pub owner: String,
    pub repo: String,
    pub index: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryMeta {
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub name: String,
}

/// A work item on the other end of a relationship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Related {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub html_url: String,
    /// Present only when the work item is a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
    /// The repository it lives in; a relationship may cross repositories.
    #[serde(default)]
    pub repository: Option<RepositoryMeta>,
}

fn path(owner: &str, name: &str, index: u64, direction: Direction) -> String {
    format!(
        "{}/issues/{index}/{}",
        crate::api::repo_path(owner, name),
        direction.segment()
    )
}

/// List one direction. `limit` of `None` reads every page.
pub async fn list(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
    direction: Direction,
    limit: Option<usize>,
) -> Result<Vec<Related>> {
    let page = client
        .get_all_page::<Related>(&path(owner, name, index, direction), &[], limit)
        .await?;
    Ok(page.items)
}

pub async fn add(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
    direction: Direction,
    other: &IssueMeta,
) -> Result<()> {
    client
        .send(
            Method::POST,
            &path(owner, name, index, direction),
            &[],
            Some(other),
        )
        .await?;
    Ok(())
}

/// Remove a relationship. The DELETE carries the same `IssueMeta` body as the
/// POST: without it the forge has no way to know which link to drop.
pub async fn remove(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
    direction: Direction,
    other: &IssueMeta,
) -> Result<()> {
    client
        .send(
            Method::DELETE,
            &path(owner, name, index, direction),
            &[],
            Some(other),
        )
        .await?;
    Ok(())
}
