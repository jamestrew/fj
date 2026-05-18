//! Forgejo Actions workflow + run + secret APIs.

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

// Forgejo's actions/runs payload is inconsistent across versions and
// even across rows in one response (queued runs omit timestamps, some
// builds drop `name`/`html_url`, `workflow_id` is an int or a filename).
// Every field is defaulted so `fj run list` degrades gracefully instead
// of failing the whole list on one odd row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    #[serde(default)]
    pub id: u64,
    // Newer Forgejo names the run `title`; older builds use `name`.
    #[serde(default, alias = "title")]
    pub name: String,
    // Newer: `prettyref`; older: `head_branch`.
    #[serde(default, alias = "prettyref")]
    pub head_branch: String,
    // Newer: `commit_sha`; older: `head_sha`.
    #[serde(default, alias = "commit_sha")]
    pub head_sha: String,
    #[serde(default)]
    pub status: String,
    // Newer schema folds the outcome into `status` (no separate field).
    #[serde(default)]
    pub conclusion: Option<String>,
    // Int row id on older builds, workflow file path ("ci.yml") on newer.
    #[serde(default, deserialize_with = "crate::api::serde_util::string_or_number")]
    pub workflow_id: String,
    #[serde(default)]
    pub html_url: String,
    // Newer: `created`/`updated`; older: `created_at`/`updated_at`.
    #[serde(default, alias = "created")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "updated")]
    pub updated_at: Option<DateTime<Utc>>,
    // Newer: `index_in_repo` (per-repo run number); older: `run_number`.
    #[serde(default, alias = "index_in_repo")]
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
