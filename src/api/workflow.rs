//! Forgejo Actions workflow + run + secret APIs.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub workflow_id: u64,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub run_number: u64,
}

#[derive(Debug, Deserialize)]
struct RunList {
    #[serde(default)]
    workflow_runs: Vec<WorkflowRun>,
    #[serde(default)]
    #[allow(dead_code)]
    total_count: u64,
}

pub async fn list_runs(
    client: &Client,
    owner: &str,
    name: &str,
    limit: u32,
) -> Result<Vec<WorkflowRun>> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/runs");
    let q = vec![("limit".into(), limit.clamp(1, 50).to_string())];
    let list: RunList = client.json(Method::GET, &path, &q, None::<&()>).await?;
    Ok(list.workflow_runs)
}

pub async fn get_run(client: &Client, owner: &str, name: &str, run_id: u64) -> Result<WorkflowRun> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/runs/{run_id}");
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn rerun(client: &Client, owner: &str, name: &str, run_id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/runs/{run_id}/rerun");
    let res = client.request(Method::POST, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn cancel(client: &Client, owner: &str, name: &str, run_id: u64) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/runs/{run_id}/cancel");
    let res = client.request(Method::POST, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSecret {
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list_secrets(client: &Client, owner: &str, name: &str) -> Result<Vec<ActionSecret>> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/secrets");
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        secrets: Vec<ActionSecret>,
    }
    let env: Envelope = client.json(Method::GET, &path, &[], None::<&()>).await?;
    Ok(env.secrets)
}

#[derive(Debug, Serialize)]
struct SecretBody<'a> {
    data: &'a str,
}

pub async fn set_secret(
    client: &Client,
    owner: &str,
    name: &str,
    secret_name: &str,
    value: &str,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}");
    let res = client
        .request(
            Method::PUT,
            &path,
            &[],
            Some(&serde_json::to_value(SecretBody { data: value })?),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn delete_secret(
    client: &Client,
    owner: &str,
    name: &str,
    secret_name: &str,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionVariable {
    pub name: String,
    pub data: String,
}

pub async fn list_variables(
    client: &Client,
    owner: &str,
    name: &str,
) -> Result<Vec<ActionVariable>> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/variables");
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        variables: Vec<ActionVariable>,
    }
    let env: Envelope = client.json(Method::GET, &path, &[], None::<&()>).await?;
    Ok(env.variables)
}

pub async fn set_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
    value: &str,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/variables/{var_name}");
    let res = client
        .request(
            Method::PUT,
            &path,
            &[],
            Some(&serde_json::json!({ "value": value })),
        )
        .await?;
    res.error_for_status()?;
    Ok(())
}

pub async fn delete_variable(
    client: &Client,
    owner: &str,
    name: &str,
    var_name: &str,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/variables/{var_name}");
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}
