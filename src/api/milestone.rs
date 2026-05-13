use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub state: String,
    #[serde(default)]
    pub open_issues: u64,
    #[serde(default)]
    pub closed_issues: u64,
    #[serde(default)]
    pub due_on: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
}

pub async fn list(client: &Client, owner: &str, name: &str, state: &str) -> Result<Vec<Milestone>> {
    let path = format!("/api/v1/repos/{owner}/{name}/milestones");
    let q = vec![("state".into(), state.into())];
    client.json(Method::GET, &path, &q, None::<&()>).await
}

pub async fn get(client: &Client, owner: &str, name: &str, id: u64) -> Result<Milestone> {
    let path = format!("/api/v1/repos/{owner}/{name}/milestones/{id}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateMilestone<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<&'a str>,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateMilestone<'_>,
) -> Result<Milestone> {
    let path = format!("/api/v1/repos/{owner}/{name}/milestones");
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditMilestone<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<&'a str>,
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    id: u64,
    body: &EditMilestone<'_>,
) -> Result<Milestone> {
    let path = format!("/api/v1/repos/{owner}/{name}/milestones/{id}");
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/milestones/{id}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

/// Assign an existing issue/PR to a milestone (or pass `None` to detach).
pub async fn assign(
    client: &Client,
    owner: &str,
    name: &str,
    issue_number: u64,
    milestone_id: Option<u64>,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/issues/{issue_number}");
    let body = match milestone_id {
        Some(id) => serde_json::json!({ "milestone": id }),
        None => serde_json::json!({ "milestone": null }),
    };
    let res = client
        .request(Method::PATCH, &path, &[], Some(&body))
        .await?;
    res.error_for_status()?;
    Ok(())
}
