//! Forgejo Actions runs, artifacts, and workflow-definition listing.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::client::capability::unsupported_reason;
use crate::client::{AuthScheme, Capability, Client, ServerInfo, Support};

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

    let mut out: Vec<WorkflowRun> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current_path = path.to_string();
    let mut current_query = run_list_query(1, 50, filters);
    for page in 1..=MAX_PAGES {
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
        let before = out.len();
        out.extend(
            list.workflow_runs
                .into_iter()
                .filter(|run| seen.insert(run.id)),
        );
        // A page that adds nothing new is the end, whatever the server says
        // about a next page or a total.
        if out.len() == before {
            break;
        }

        let next = headers
            .get(reqwest::header::LINK)
            .and_then(|link| link.to_str().ok())
            .map(crate::client::pagination::parse_link_header)
            .and_then(|links| {
                links
                    .into_iter()
                    .find_map(|(url, rel)| (rel == "next").then_some(url))
            });
        match next {
            Some(next) => {
                let parsed = Url::parse(&next)?;
                current_path = parsed.path().to_string();
                current_query = parsed
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
            }
            // Forgejo sends no Link header on this route, only `total_count`,
            // so the count is what says another page exists.
            None if (out.len() as u64) < list.total_count => {
                current_path = path.to_string();
                current_query = run_list_query(page + 1, 50, filters);
            }
            None => break,
        }
    }
    Ok(out)
}

/// Find one run by its per-repo run number. The server filters on
/// `run_number`, so this is a single request however old the run is. A server
/// that ignores the filter answers with its newest page instead; when the run
/// is not on it, walk the rest of the list.
pub(crate) async fn find_run_by_number(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
) -> Result<Option<WorkflowRun>> {
    let path = format!("{}/actions/runs", crate::api::repo_path(owner, name));
    let mut q = run_list_query(1, 50, &RunListFilters::default());
    q.push(("run_number".into(), run_number.to_string()));
    let list: RunList = client.json(Method::GET, &path, &q, None::<&()>).await?;

    let filter_ignored = list
        .workflow_runs
        .iter()
        .any(|run| run.run_number != run_number);
    if let Some(run) = list
        .workflow_runs
        .into_iter()
        .find(|run| run.run_number == run_number)
    {
        return Ok(Some(run));
    }
    if !filter_ignored {
        return Ok(None);
    }
    let runs = fetch_all_run_pages(client, &path, &RunListFilters::default()).await?;
    Ok(runs.into_iter().find(|run| run.run_number == run_number))
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

// ── Capability-gated Actions surface ────────────────────────────────────────
//
// Run jobs, job logs, artifacts, cancel and rerun are optional: which of them
// a server serves to an API token depends on the forge AND its release, on two
// version axes that cannot be compared. Every command below asks
// `client.server_info()` and the gate table in `client::capability`:
//
//   * Unsupported: no request is sent. The command reports the limitation and
//     the run's web URL.
//   * Supported or Unknown: the API route is tried, and a 404 is reported as
//     the same limitation.
//
// The unversioned web routes (`/{owner}/{repo}/actions/...`) authenticate with
// a browser session cookie plus CSRF token. A personal access token does
// nothing there, so fj never sends one: the few web reads that remain as a
// fallback for older servers go through `web_client`, which strips the token.
// There are no web POSTs left.

/// An optional Actions route this server does not serve to an API token.
/// Typed so a caller can tell "not offered here" from a real failure and pick
/// another source without parsing message text.
#[derive(Debug)]
pub struct ActionsUnavailable(String);

impl std::fmt::Display for ActionsUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ActionsUnavailable {}

/// The limitation error: why the feature is not offered here, and a safe way
/// to reach the run in a browser.
pub fn actions_limitation(reason: &str, run_url: &str) -> anyhow::Error {
    anyhow::Error::new(ActionsUnavailable(format!(
        "{}. Open the run in the web UI instead:\n  {run_url}",
        reason.trim_end_matches('.')
    )))
}

pub fn is_actions_unavailable(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ActionsUnavailable>().is_some()
}

/// Browser URL of a run, by its per-repo run number.
pub fn run_web_url(client: &Client, owner: &str, name: &str, run_number: u64) -> String {
    let origin = client
        .base()
        .join("/")
        .map(|u| u.to_string())
        .unwrap_or_else(|_| "/".into());
    format!(
        "{origin}{}/{}/actions/runs/{run_number}",
        crate::api::path_segment(owner),
        crate::api::path_segment(name)
    )
}

/// The client for an unversioned web route: the same host, with no forge
/// token attached. Refused outright for a Fjord Account gateway, which does
/// not forward web routes and must never see a request shaped like one.
pub(crate) fn web_client(client: &Client, what: &str, run_url: &str) -> Result<Client> {
    if matches!(client.auth_scheme(), AuthScheme::FjordBearer) {
        return Err(actions_limitation(
            &format!(
                "{what} needs a web route on this server, which a Fjord Account gateway does \
                 not forward"
            ),
            run_url,
        ));
    }
    Ok(client.without_credentials())
}

fn detail_suffix(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}

/// Classify a failed run action. A 404 is the limitation (with the run URL), a
/// 403 is a permission denial, and the two are never worded alike.
fn run_action_error(
    verb: &str,
    run: u64,
    status: StatusCode,
    body: &str,
    run_url: &str,
) -> anyhow::Error {
    match status {
        StatusCode::NOT_FOUND => actions_limitation(
            &format!(
                "this server has no API route to {verb} run #{run}, or the token cannot see \
                 the run (HTTP 404)"
            ),
            run_url,
        ),
        StatusCode::FORBIDDEN => anyhow!(
            "permission denied: this token may not {verb} run #{run} (HTTP 403){}. It needs \
             write access to the repository.",
            detail_suffix(body)
        ),
        _ => anyhow!(
            "could not {verb} run #{run} (HTTP {}){}",
            status.as_u16(),
            detail_suffix(body)
        ),
    }
}

// ── Artifacts ────────────────────────────────────────────────────────────────
//
// Forgejo 16 and Gitea 1.24 serve artifacts over `/api/v1`: a list per run
// (`{run_id}` is the internal run id) and a zip per artifact id. Older servers
// only have the run page's web route, which fj reads WITHOUT the token (see
// the section comment above): that works for a public repository and reports
// the limitation for a private one.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// API artifact id. Zero when the entry came from the web route, which
    /// addresses artifacts by name.
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "size_in_bytes")]
    pub size: u64,
    /// `completed`, `expired`, or `pending`.
    #[serde(default)]
    pub status: String,
}

/// Decode an artifact list. Forgejo's API answers a bare array; Gitea's API
/// and the web route wrap it in `{"artifacts": [...]}`. The API reports
/// `expired` as a boolean where the web route has a `status` string.
pub fn parse_artifacts(value: serde_json::Value) -> Result<Vec<Artifact>> {
    let items = match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut map) => match map.remove("artifacts") {
            Some(serde_json::Value::Array(items)) => items,
            _ => return Err(anyhow!("unexpected artifact list response shape")),
        },
        _ => return Err(anyhow!("unexpected artifact list response shape")),
    };
    items
        .into_iter()
        .map(|item| {
            let expired = item
                .get("expired")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let mut artifact: Artifact =
                serde_json::from_value(item).context("decoding artifact")?;
            if artifact.status.is_empty() {
                artifact.status = if expired { "expired" } else { "completed" }.into();
            }
            Ok(artifact)
        })
        .collect()
}

pub async fn list_artifacts(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<Vec<Artifact>> {
    let info = client.server_info().await;
    let run_url = run_web_url(client, owner, name, run);
    if info.supports(Capability::Artifacts) != Support::Unsupported {
        match list_artifacts_api(client, owner, name, run, &run_url).await {
            Err(err) if is_actions_unavailable(&err) => {}
            other => return other,
        }
    }
    list_artifacts_web(client, run, &info, &run_url).await
}

async fn list_artifacts_api(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    run_url: &str,
) -> Result<Vec<Artifact>> {
    let run_id = crate::api::workflow_logs::resolve_run_api_id(client, owner, name, run).await?;
    let path = format!(
        "{}/actions/runs/{run_id}/artifacts",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::GET, &path, &[], None).await?;
    let status = res.status();
    if status == StatusCode::NOT_FOUND {
        return Err(actions_limitation(
            "this server has no API route for run artifacts",
            run_url,
        ));
    }
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(anyhow!(
            "could not list artifacts for run #{run} (HTTP {}){}",
            status.as_u16(),
            detail_suffix(&body)
        ));
    }
    parse_artifacts(res.json().await.context("decoding artifacts JSON")?)
}

fn web_artifact_limitation(info: &ServerInfo, run: u64, run_url: &str) -> anyhow::Error {
    actions_limitation(
        &format!(
            "cannot read artifacts for run #{run} with an API token: {} serves them only from \
             a web route that needs a browser session, and the run is not publicly readable \
             (or does not exist; check `fj run list`)",
            info.describe()
        ),
        run_url,
    )
}

async fn list_artifacts_web(
    client: &Client,
    run: u64,
    info: &ServerInfo,
    run_url: &str,
) -> Result<Vec<Artifact>> {
    let web = web_client(client, "listing artifacts", run_url)?;
    let res = web
        .request(Method::GET, &format!("{run_url}/artifacts"), &[], None)
        .await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(if status == StatusCode::NOT_FOUND {
            web_artifact_limitation(info, run, run_url)
        } else {
            anyhow!(
                "could not list artifacts for run #{run} (HTTP {}){}",
                status.as_u16(),
                detail_suffix(&body)
            )
        });
    }
    parse_artifacts(res.json().await.context("decoding artifacts JSON")?)
}

/// Download one artifact (a zip). An entry that carries an API id is fetched
/// from `/api/v1`; one listed from the web route is fetched from the web route
/// by name, without the token.
pub async fn download_artifact(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    artifact: &Artifact,
) -> Result<Vec<u8>> {
    let run_url = run_web_url(client, owner, name, run);
    crate::download::verified_body(
        || async {
            let res = if artifact.id != 0 {
                let path = format!(
                    "{}/actions/artifacts/{}/zip",
                    crate::api::repo_path(owner, name),
                    artifact.id
                );
                client.request(Method::GET, &path, &[], None).await?
            } else {
                let web = web_client(client, "downloading an artifact", &run_url)?;
                let url = format!(
                    "{run_url}/artifacts/{}",
                    crate::api::path_segment(&artifact.name)
                );
                web.request(Method::GET, &url, &[], None).await?
            };
            let status = res.status();
            if !status.is_success() {
                let body = res.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "could not download artifact '{}' from run #{run} (HTTP {}){}",
                    artifact.name,
                    status.as_u16(),
                    detail_suffix(&body)
                ));
            }
            Ok(res)
        },
        None,
        true,
        "artifact",
    )
    .await
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    #[serde(deserialize_with = "crate::api::serde_util::string_or_number")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, rename = "html_url")]
    pub html_url: String,
    #[serde(default)]
    pub badge_url: String,
    #[serde(default, alias = "created")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "updated")]
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn get_workflow(
    client: &Client,
    owner: &str,
    name: &str,
    workflow: &str,
) -> Result<Workflow> {
    let path = workflow_path(owner, name, workflow, None);
    client.json(Method::GET, &path, &[], None::<&()>).await
}

pub async fn enable_workflow(
    client: &Client,
    owner: &str,
    name: &str,
    workflow: &str,
) -> Result<()> {
    set_workflow_state(client, owner, name, workflow, "enable").await
}

pub async fn disable_workflow(
    client: &Client,
    owner: &str,
    name: &str,
    workflow: &str,
) -> Result<()> {
    set_workflow_state(client, owner, name, workflow, "disable").await
}

fn workflow_path(owner: &str, name: &str, workflow: &str, action: Option<&str>) -> String {
    let base = format!(
        "{}/actions/workflows/{}",
        crate::api::repo_path(owner, name),
        crate::api::path_segment(workflow)
    );
    match action {
        Some(action) => format!("{base}/{action}"),
        None => base,
    }
}

async fn set_workflow_state(
    client: &Client,
    owner: &str,
    name: &str,
    workflow: &str,
    action: &str,
) -> Result<()> {
    let path = workflow_path(owner, name, workflow, Some(action));
    let res = client.request(Method::PUT, &path, &[], None).await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        let detail = body.trim();
        return Err(anyhow!(
            "could not {action} `{workflow}` (HTTP {}){}",
            status.as_u16(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(())
}

// ── Rerun and cancel ─────────────────────────────────────────────────────────
//
// Gitea 1.26 added three rerun routes under `/api/v1`; no Forgejo has any.
// Forgejo 16 added a cancel route; Gitea has none before 28.0. Main used to
// POST the web actions with the token attached, which the web router ignores
// (it wants a session cookie and a CSRF token), so that path is gone: a server
// without the API route gets the limitation and the run URL, and no request.
// `run` is the per-repo run number, as in `run view` / `run list`.

/// What `fj run rerun` should restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunTarget {
    /// Every job in the run.
    Run,
    /// Only the jobs that did not succeed.
    Failed,
    /// One job, by its 0-based index in the run (the row order of
    /// `fj run view`). The index is mapped to the job id the route wants.
    Job(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunOutcome {
    Started,
    /// `Failed` was asked for and the server reports nothing failed.
    NothingFailed,
}

pub async fn rerun(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    target: RerunTarget,
) -> Result<RerunOutcome> {
    let info = client.server_info().await;
    let run_url = run_web_url(client, owner, name, run);
    if info.supports(Capability::RunRerun) == Support::Unsupported {
        return Err(actions_limitation(
            &unsupported_reason(&info, Capability::RunRerun, "rerun a workflow run"),
            &run_url,
        ));
    }
    let run_id = crate::api::workflow_logs::resolve_run_api_id(client, owner, name, run).await?;
    let repo = crate::api::repo_path(owner, name);
    let path = match target {
        RerunTarget::Run => format!("{repo}/actions/runs/{run_id}/rerun"),
        RerunTarget::Failed => format!("{repo}/actions/runs/{run_id}/rerun-failed-jobs"),
        RerunTarget::Job(index) => {
            let jobs =
                crate::api::workflow_logs::list_run_jobs_api(client, owner, name, run, run_id)
                    .await?;
            let job = jobs.get(index).ok_or_else(|| {
                anyhow!(
                    "job index {index} is out of range: this run has {} job(s)",
                    jobs.len()
                )
            })?;
            format!("{repo}/actions/runs/{run_id}/jobs/{}/rerun", job.id)
        }
    };
    let res = client.request(Method::POST, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return Ok(RerunOutcome::Started);
    }
    let body = res.text().await.unwrap_or_default();
    if target == RerunTarget::Failed
        && status == StatusCode::BAD_REQUEST
        && body.to_ascii_lowercase().contains("no failed jobs")
    {
        return Ok(RerunOutcome::NothingFailed);
    }
    Err(run_action_error("rerun", run, status, &body, &run_url))
}

pub async fn cancel(client: &Client, owner: &str, name: &str, run: u64) -> Result<()> {
    let info = client.server_info().await;
    let run_url = run_web_url(client, owner, name, run);
    if info.supports(Capability::RunCancel) == Support::Unsupported {
        return Err(actions_limitation(
            &unsupported_reason(&info, Capability::RunCancel, "cancel a workflow run"),
            &run_url,
        ));
    }
    let run_id = crate::api::workflow_logs::resolve_run_api_id(client, owner, name, run).await?;
    let path = format!(
        "{}/actions/runs/{run_id}/cancel",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::POST, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(run_action_error("cancel", run, status, &body, &run_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> Client {
        Client::for_base_url(
            Url::parse("https://forge.example/api/v1/").unwrap(),
            "t".into(),
        )
        .unwrap()
    }

    #[test]
    fn run_web_url_is_anchored_at_the_origin() {
        assert_eq!(
            run_web_url(&test_client(), "octo", "my repo", 56),
            "https://forge.example/octo/my%20repo/actions/runs/56"
        );
    }

    #[test]
    fn limitation_carries_the_run_url_and_is_typed() {
        let err = actions_limitation("Forgejo has no API route to rerun a workflow run.", "U");
        assert!(is_actions_unavailable(&err));
        let msg = err.to_string();
        assert!(msg.contains("rerun a workflow run. Open the run"), "{msg}");
        assert!(msg.ends_with("\n  U"), "{msg}");
        assert!(!is_actions_unavailable(&anyhow!("boom")));
    }

    #[test]
    fn run_action_404_is_a_limitation_and_403_is_a_permission_denial() {
        let missing = run_action_error("cancel", 7, StatusCode::NOT_FOUND, "", "U");
        assert!(is_actions_unavailable(&missing));
        assert!(missing.to_string().contains("\n  U"));

        let denied = run_action_error("cancel", 7, StatusCode::FORBIDDEN, "nope", "U");
        assert!(!is_actions_unavailable(&denied));
        let msg = denied.to_string();
        assert!(msg.starts_with("permission denied"), "{msg}");
        assert!(msg.contains("nope"), "{msg}");

        let other = run_action_error("cancel", 7, StatusCode::CONFLICT, "", "U");
        assert!(other.to_string().contains("HTTP 409"));
    }

    #[test]
    fn web_client_drops_the_token_and_refuses_a_gateway() {
        let web = web_client(&test_client(), "listing artifacts", "U").unwrap();
        assert!(web.auth_header_value().unwrap().is_none());

        let gateway = Client::for_base_url_with_scheme(
            Url::parse("https://gw.example/api/v1/").unwrap(),
            Some("jwt".into()),
            AuthScheme::FjordBearer,
        )
        .unwrap();
        let err = web_client(&gateway, "listing artifacts", "U")
            .err()
            .expect("a gateway client is refused");
        assert!(is_actions_unavailable(&err));
        assert!(err.to_string().contains("Fjord Account gateway"));
    }

    #[test]
    fn parse_artifacts_accepts_the_forgejo_array_and_the_gitea_envelope() {
        let forgejo = serde_json::json!([
            { "id": 4, "name": "dist", "size_in_bytes": 10, "expired": false },
            { "id": 5, "name": "old", "size_in_bytes": 2, "expired": true }
        ]);
        let got = parse_artifacts(forgejo).unwrap();
        assert_eq!(
            (got[0].id, got[0].size, got[0].status.as_str()),
            (4, 10, "completed")
        );
        assert_eq!(got[1].status, "expired");

        let envelope = serde_json::json!({
            "artifacts": [{ "name": "web", "size": 3, "status": "completed" }]
        });
        let got = parse_artifacts(envelope).unwrap();
        assert_eq!((got[0].id, got[0].size), (0, 3));

        assert!(parse_artifacts(serde_json::json!({ "nope": 1 })).is_err());
    }

    #[test]
    fn workflow_path_without_action_encodes_segments() {
        assert_eq!(
            workflow_path("owner name", "repo/name", "ci.yml", None),
            "/api/v1/repos/owner%20name/repo%2Fname/actions/workflows/ci.yml"
        );
    }

    #[test]
    fn workflow_path_with_action() {
        assert_eq!(
            workflow_path("owner", "repo", "sub/ci.yml", Some("enable")),
            "/api/v1/repos/owner/repo/actions/workflows/sub%2Fci.yml/enable"
        );
    }
}
