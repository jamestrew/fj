//! Forgejo Actions runs, artifacts, and workflow-definition listing.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
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

#[derive(Debug, Serialize)]
struct DispatchBody<'a> {
    #[serde(rename = "ref")]
    ref_: &'a str,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    inputs: std::collections::BTreeMap<String, String>,
}

/// Trigger a `workflow_dispatch` for one workflow file on a given ref. This is
/// the one Actions-management write the `/api/v1` surface exposes (there is no
/// list/enable/disable). `workflow` is the file name, e.g. `ci.yml`.
pub async fn dispatch(
    client: &Client,
    owner: &str,
    name: &str,
    workflow: &str,
    ref_: &str,
    inputs: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let path = format!("/api/v1/repos/{owner}/{name}/actions/workflows/{workflow}/dispatches");
    let body = DispatchBody { ref_, inputs };
    let res = client
        .request(Method::POST, &path, &[], Some(&serde_json::to_value(body)?))
        .await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        // Forgejo answers a missing workflow file or bad ref with a 500 and a
        // terse body; surface it rather than a bare status line.
        let detail = body.trim();
        return Err(anyhow!(
            "could not dispatch `{workflow}` on `{ref_}` (HTTP {}){}",
            status.as_u16(),
            if detail.is_empty() {
                "; check the workflow file name and ref".to_string()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(())
}

// ── Artifacts ────────────────────────────────────────────────────────────────
//
// Like logs, artifacts have no `/api/v1` endpoint; the run page's web route
// returns them as JSON. LISTING is confirmed working (GET .../runs/{run}/
// artifacts -> {"artifacts":[...]}). DOWNLOAD uses the documented
// .../artifacts/{name} web route but could not be verified against a populated
// run in development (no run had artifacts); see docs/compatibility.md.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// `completed`, `expired`, or `pending`.
    #[serde(default)]
    pub status: String,
}

pub async fn list_artifacts(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<Vec<Artifact>> {
    let origin = client.base().join("/")?;
    let url = format!("{origin}{owner}/{name}/actions/runs/{run}/artifacts");
    let res = client.request(Method::GET, &url, &[], None).await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(if status == StatusCode::NOT_FOUND {
            anyhow!("run #{run} not found (check `fj run list` for the run number)")
        } else {
            anyhow!("could not list artifacts for run #{run}: {}", body.trim())
        });
    }
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        artifacts: Vec<Artifact>,
    }
    let env: Envelope = res.json().await.context("decoding artifacts JSON")?;
    Ok(env.artifacts)
}

/// Download one artifact (a zip) by name. Returns the raw bytes.
pub async fn download_artifact(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    artifact: &str,
) -> Result<Vec<u8>> {
    let origin = client.base().join("/")?;
    let enc = crate::api::urlencode_path_segment(artifact);
    let url = format!("{origin}{owner}/{name}/actions/runs/{run}/artifacts/{enc}");
    let res = client.request(Method::GET, &url, &[], None).await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(anyhow!(
            "could not download artifact '{artifact}' from run #{run} (HTTP {}): {}",
            status.as_u16(),
            body.trim()
        ));
    }
    crate::client::check_body_size(&res)?;
    Ok(res.bytes().await.context("reading artifact body")?.to_vec())
}

// ── Workflow definitions ─────────────────────────────────────────────────────
//
// There is no workflow-list API. Instead we read the workflow files straight
// from the repo via the contents API, covering both Forgejo's `.forgejo/
// workflows` and GitHub-compatible `.github/workflows` directories.

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowFile {
    pub name: String,
    pub path: String,
}

pub async fn list_workflows(client: &Client, owner: &str, name: &str) -> Result<Vec<WorkflowFile>> {
    #[derive(Deserialize)]
    struct ContentItem {
        #[serde(default)]
        name: String,
        #[serde(default)]
        path: String,
        #[serde(default, rename = "type")]
        kind: String,
    }
    let mut out: Vec<WorkflowFile> = Vec::new();
    for dir in [".forgejo/workflows", ".github/workflows"] {
        let path = format!("/api/v1/repos/{owner}/{name}/contents/{dir}");
        let res = client.request(Method::GET, &path, &[], None).await?;
        if !res.status().is_success() {
            // Missing directory -> 404; just skip it.
            continue;
        }
        let items: Vec<ContentItem> = match res.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for it in items {
            let is_yaml = it.name.ends_with(".yml") || it.name.ends_with(".yaml");
            if it.kind == "file" && is_yaml {
                out.push(WorkflowFile {
                    name: it.name,
                    path: it.path,
                });
            }
        }
    }
    Ok(out)
}

// `rerun` and `cancel` have NO `/api/v1` endpoint on any Forgejo (the API only
// lists/gets runs; see the run-logs section below). They exist only as web
// actions, and Forgejo answers a PAT-authenticated POST to them with a
// handler-level 404 even when the run resolves fine for reads, i.e. these
// actions require a browser session this CLI can't present. So rather than
// fake an `/api/v1/.../rerun` call (which 404s), we attempt the real web route
// and, on the expected rejection, return an actionable message pointing at the
// web UI. `run` is the per-repo run number, as in `run view` / `run list`.
pub async fn rerun(client: &Client, owner: &str, name: &str, run: u64) -> Result<()> {
    post_run_action(client, owner, name, run, "rerun").await
}

pub async fn cancel(client: &Client, owner: &str, name: &str, run: u64) -> Result<()> {
    post_run_action(client, owner, name, run, "cancel").await
}

async fn post_run_action(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    action: &str,
) -> Result<()> {
    let origin = client.base().join("/")?;
    let run_url = format!("{origin}{owner}/{name}/actions/runs/{run}");
    let res = client
        .request(Method::POST, &format!("{run_url}/{action}"), &[], None)
        .await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(if status == StatusCode::NOT_FOUND {
            anyhow!(
                "this Forgejo won't {action} run #{run} via an API token: Forgejo has no \
                 rerun/cancel API, and its web action needs a browser session. Do it here:\n  {run_url}"
            )
        } else {
            anyhow!("could not {action} run #{run}: {}", body.trim())
        });
    }
    Ok(())
}
