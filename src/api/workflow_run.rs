//! Forgejo Actions runs, artifacts, and workflow-definition listing.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

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
    #[serde(default, alias = "event_name")]
    pub event: String,
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

#[derive(Debug, Default, Clone)]
pub struct RunListFilters {
    pub status: Option<String>,
    pub branch: Option<String>,
    pub event: Option<String>,
    pub workflow: Option<String>,
}

impl RunListFilters {
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.branch.is_none()
            && self.event.is_none()
            && self.workflow.is_none()
    }

    fn query_params(&self) -> Vec<(String, String)> {
        let mut q = Vec::new();
        if let Some(status) = &self.status {
            q.push(("status".into(), status.clone()));
        }
        if let Some(branch) = &self.branch {
            q.push(("branch".into(), branch.clone()));
        }
        if let Some(event) = &self.event {
            q.push(("event".into(), event.clone()));
        }
        if let Some(workflow) = &self.workflow {
            q.push(("workflow".into(), workflow.clone()));
        }
        q
    }

    fn matches(&self, run: &WorkflowRun) -> bool {
        self.status
            .as_deref()
            .is_none_or(|status| run.status == status || run.conclusion.as_deref() == Some(status))
            && self
                .branch
                .as_deref()
                .is_none_or(|branch| run.head_branch == branch)
            && self.event.as_deref().is_none_or(|event| run.event == event)
            && self
                .workflow
                .as_deref()
                .is_none_or(|workflow| workflow_matches(&run.workflow_id, workflow))
    }

    fn unknown_values(&self, runs: &[WorkflowRun]) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(status) = &self.status {
            let known = runs
                .iter()
                .any(|run| run.status == *status || run.conclusion.as_deref() == Some(status));
            if !known {
                out.push(format!("status `{status}`"));
            }
        }
        if let Some(branch) = &self.branch {
            let known = runs.iter().any(|run| run.head_branch == *branch);
            if !known {
                out.push(format!("branch `{branch}`"));
            }
        }
        if let Some(event) = &self.event {
            let known = runs.iter().any(|run| run.event == *event);
            if !known {
                out.push(format!("event `{event}`"));
            }
        }
        if let Some(workflow) = &self.workflow {
            let known = runs
                .iter()
                .any(|run| workflow_matches(&run.workflow_id, workflow));
            if !known {
                out.push(format!("workflow `{workflow}`"));
            }
        }
        out
    }
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
    list_runs_filtered(
        client,
        owner,
        name,
        limit,
        false,
        &RunListFilters::default(),
    )
    .await
}

pub async fn list_runs_filtered(
    client: &Client,
    owner: &str,
    name: &str,
    limit: u32,
    paginate: bool,
    filters: &RunListFilters,
) -> Result<Vec<WorkflowRun>> {
    let runs = fetch_runs(client, owner, name, limit, paginate, filters).await?;
    if filters.is_empty() {
        return Ok(runs);
    }

    let filtered: Vec<WorkflowRun> = runs
        .iter()
        .filter(|run| filters.matches(run))
        .cloned()
        .collect();
    if !filtered.is_empty() {
        return Ok(filtered);
    }

    let unfiltered = fetch_runs(
        client,
        owner,
        name,
        limit,
        paginate,
        &RunListFilters::default(),
    )
    .await?;
    if unfiltered.is_empty() {
        return Ok(Vec::new());
    }
    let unknown = filters.unknown_values(&unfiltered);
    if !unknown.is_empty() {
        return Err(anyhow!(
            "unknown workflow run filter value: {}",
            unknown.join(", ")
        ));
    }

    Ok(unfiltered
        .into_iter()
        .filter(|run| filters.matches(run))
        .collect())
}

async fn fetch_runs(
    client: &Client,
    owner: &str,
    name: &str,
    limit: u32,
    paginate: bool,
    filters: &RunListFilters,
) -> Result<Vec<WorkflowRun>> {
    let path = format!("{}/actions/runs", crate::api::repo_path(owner, name));
    if paginate {
        return fetch_all_run_pages(client, &path, filters).await;
    }

    // Forgejo's actions/runs endpoint ignores `limit` unless `page` is also
    // present; without it the server returns every run regardless of `-L`.
    let q = run_list_query(1, limit.clamp(1, 50), filters);
    let list: RunList = client.json(Method::GET, &path, &q, None::<&()>).await?;
    Ok(list.workflow_runs)
}

async fn fetch_all_run_pages(
    client: &Client,
    path: &str,
    filters: &RunListFilters,
) -> Result<Vec<WorkflowRun>> {
    const MAX_PAGES: u32 = 100;

    let mut out = Vec::new();
    let mut current_path = path.to_string();
    let mut current_query = run_list_query(1, 50, filters);
    for _ in 0..MAX_PAGES {
        let res = client
            .request(Method::GET, &current_path, &current_query, None)
            .await?;
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!(
                "could not list workflow runs (HTTP {}): {}",
                status.as_u16(),
                body.trim()
            ));
        }

        let headers = res.headers().clone();
        let list: RunList = res.json().await.context("decoding workflow runs JSON")?;
        if list.workflow_runs.is_empty() {
            break;
        }
        out.extend(list.workflow_runs);

        let Some(next) = headers
            .get(reqwest::header::LINK)
            .and_then(|link| link.to_str().ok())
            .map(crate::client::pagination::parse_link_header)
            .and_then(|links| {
                links
                    .into_iter()
                    .find_map(|(url, rel)| (rel == "next").then_some(url))
            })
        else {
            break;
        };
        let parsed = Url::parse(&next)?;
        current_path = parsed.path().to_string();
        current_query = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
    }
    Ok(out)
}

fn run_list_query(page: u32, limit: u32, filters: &RunListFilters) -> Vec<(String, String)> {
    let mut q = vec![
        ("page".into(), page.max(1).to_string()),
        ("limit".into(), limit.clamp(1, 50).to_string()),
    ];
    q.extend(filters.query_params());
    q
}

fn workflow_matches(actual: &str, wanted: &str) -> bool {
    actual == wanted || actual.rsplit('/').next() == Some(wanted)
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
    let path = format!(
        "{}/actions/workflows/{}/dispatches",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(workflow)
    );
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
    let url = format!(
        "{origin}{}/{}/actions/runs/{run}/artifacts",
        crate::api::path_segment(owner),
        crate::api::path_segment(name)
    );
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
    let url = format!(
        "{origin}{}/{}/actions/runs/{run}/artifacts/{}",
        crate::api::path_segment(owner),
        crate::api::path_segment(name),
        crate::api::path_segment(artifact)
    );
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
        let path = format!("{}/contents/{dir}", crate::api::repo_path(owner, name));
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
    post_run_action(client, owner, name, run, "rerun", "rerun").await
}

/// Rerun a single job by its 0-based index within the run (the index shown by
/// `fj run view`). Forgejo's run-level `/rerun` reruns every job; the per-job
/// `/jobs/{job}/rerun` route reruns just that one. Index 0 is a real job here,
/// not a "rerun all" sentinel: Forgejo keys "rerun all" off the absence of the
/// `{job}` path segment, not off `job == 0`.
pub async fn rerun_job(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
) -> Result<()> {
    post_run_action(
        client,
        owner,
        name,
        run,
        &format!("jobs/{job}/rerun"),
        "rerun",
    )
    .await
}

pub async fn cancel(client: &Client, owner: &str, name: &str, run: u64) -> Result<()> {
    post_run_action(client, owner, name, run, "cancel", "cancel").await
}

/// POST a web action under a run. `path` is the route suffix appended to the
/// run URL (`rerun`, `cancel`, `jobs/2/rerun`); `verb` is the bare word used in
/// error messages so a per-job path still reads "won't rerun run #N".
async fn post_run_action(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    path: &str,
    verb: &str,
) -> Result<()> {
    let origin = client.base().join("/")?;
    let run_url = format!(
        "{origin}{}/{}/actions/runs/{run}",
        crate::api::path_segment(owner),
        crate::api::path_segment(name)
    );
    let res = client
        .request(Method::POST, &format!("{run_url}/{path}"), &[], None)
        .await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(if status == StatusCode::NOT_FOUND {
            anyhow!(
                "this Forgejo won't {verb} run #{run} via an API token: Forgejo has no \
                 rerun/cancel API, and its web action needs a browser session. Do it here:\n  {run_url}"
            )
        } else {
            anyhow!("could not {verb} run #{run}: {}", body.trim())
        });
    }
    Ok(())
}
