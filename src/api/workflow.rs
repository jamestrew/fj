//! Forgejo Actions workflow + run + secret APIs.

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

// ── Run logs ─────────────────────────────────────────────────────────────────
//
// THIS SECTION IS DELIBERATELY DIFFERENT FROM EVERY OTHER API MODULE.
//
// Forgejo's `/api/v1` exposes almost nothing for Actions runs. Verified
// against 15.0.2 / gitea-1.22: `/actions/runs/{id}` (single run),
// `/actions/runs/{id}/jobs`, `/actions/jobs/{id}/logs`, and
// `/actions/runs/{id}/logs` all 404. The only documented run surface is the
// `/actions/tasks` list plus `rerun` / `cancel`. Both the per-run SUMMARY and
// the LOGS live exclusively behind the web frontend that the browser Actions
// UI talks to. So, unlike `list_runs` above (an ordinary typed `/api/v1`
// wrapper), the functions below step OUTSIDE the API and call the
// human-facing web routes under `/{owner}/{repo}/actions/`.
//
// `fetch_run_summary` powers `fj run view N`; `fetch_job_logs` /
// `fetch_run_view` / `latest_attempt` power `--log` and `watch`.
//
// They still funnel through the one HTTP path (`Client::request`) and reuse
// the same token: `Client::url` passes absolute URLs through untouched, and
// the web origin is the same host as the API base, so `is_trusted_url` keeps
// sending the `Authorization` header. No parallel HTTP client, no second auth
// path.
//
// Two web routes, both confirmed working:
//
//   * whole-job plaintext download (one GET, entire job log, timestamped):
//       GET  /{owner}/{repo}/actions/runs/{run}/jobs/{job}/attempt/{attempt}/logs
//   * incremental per-step JSON (what `fj run watch` polls):
//       POST /{owner}/{repo}/actions/runs/{run}/jobs/{job}/attempt/{attempt}
//       body {"logCursors":[{"step":S,"cursor":C,"expanded":true}]}
//     An empty `{}` body returns run/job/step state with no log lines, which
//     is how we discover the step count cheaply before asking for every step.
//
// Parameter quirks that make this look impossible until you know them:
//   run     the PER-REPO run number (the `#` column of `fj run list`), NOT
//           the global id from `/actions/tasks`.
//   job     zero-based job position within the run.
//   attempt 1-based. Forgejo 15 added the `/attempt/{n}` path segment; the
//           attempt-less route silently resolves to "attempt 0" and 500s with
//           "resource does not exist". We default to the latest attempt.
//
// CAVEAT: these are unversioned web routes, not API. They are stable in
// practice but can shift between Forgejo releases, and a Fjord-Account gateway
// that only proxies `/api/v1` will not forward them (the CLI guards that case
// with a clear message). Old runs whose logs have been pruned return the same
// "resource does not exist" 500; we translate that to a friendly notice.

/// One entry in the `logCursors` array of a ViewPost request: "give me lines
/// for `step`, starting after byte/line `cursor`". `expanded` mirrors the UI
/// flag and must be true for the server to return lines.
#[derive(Debug, Serialize)]
pub struct LogCursor {
    pub step: usize,
    pub cursor: u64,
    pub expanded: bool,
}

/// The JSON returned by the web ViewPost route. Only the fields we render are
/// modeled; everything is defaulted so a schema drift in an unused corner
/// doesn't fail the whole decode.
#[derive(Debug, Deserialize)]
pub struct RunView {
    #[serde(default)]
    pub logs: LogsPayload,
    pub state: ViewState,
}

#[derive(Debug, Default, Deserialize)]
pub struct LogsPayload {
    #[serde(default, rename = "stepsLog")]
    pub steps_log: Vec<StepLog>,
}

#[derive(Debug, Deserialize)]
pub struct StepLog {
    #[serde(default)]
    pub step: usize,
    /// Opaque resume position to send back as the next request's `cursor`.
    #[serde(default)]
    pub cursor: u64,
    #[serde(default)]
    pub lines: Vec<LogLine>,
}

#[derive(Debug, Deserialize)]
pub struct LogLine {
    // index/timestamp are modeled to document the wire shape and to support a
    // future `--timestamps` flag; the current renderers only print `message`.
    #[serde(default)]
    #[allow(dead_code)]
    pub index: u64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub timestamp: f64,
}

#[derive(Debug, Deserialize)]
pub struct ViewState {
    pub run: RunStateView,
    #[serde(rename = "currentJob")]
    pub current_job: CurrentJob,
}

#[derive(Debug, Deserialize)]
pub struct RunStateView {
    /// The run's display title (the triggering commit/PR title).
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub status: String,
    /// Web path to the run, e.g. `/owner/repo/actions/runs/56`. Relative;
    /// `cli::workflow` absolutizes it against the host origin.
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub commit: Commit,
    #[serde(default)]
    pub jobs: Vec<JobBrief>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Commit {
    #[serde(rename = "shortSHA", default)]
    pub short_sha: String,
    #[serde(default)]
    pub branch: CommitBranch,
    #[serde(default)]
    pub pusher: Pusher,
}

#[derive(Debug, Default, Deserialize)]
pub struct CommitBranch {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Pusher {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct JobBrief {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration: String,
}

#[derive(Debug, Deserialize)]
pub struct CurrentJob {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub steps: Vec<StepBrief>,
    #[serde(default, rename = "allAttempts")]
    pub all_attempts: Vec<AttemptBrief>,
}

#[derive(Debug, Deserialize)]
pub struct StepBrief {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration: String,
}

#[derive(Debug, Deserialize)]
pub struct AttemptBrief {
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    #[allow(dead_code)]
    pub status: String,
}

/// Base URL for a run+job+attempt's web routes, anchored at the host ORIGIN
/// (not the `/api/v1/` base). `Client::base()` ends in `/api/v1/`; joining `/`
/// drops back to `https://host/`.
fn web_run_url(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
    attempt: u32,
) -> Result<String> {
    let origin = client.base().join("/")?;
    Ok(format!(
        "{origin}{owner}/{name}/actions/runs/{run}/jobs/{job}/attempt/{attempt}"
    ))
}

/// Map a failed web-route response to a message that explains itself, instead
/// of leaking the raw "Not found." / "resource does not exist" web body.
fn log_route_error(status: StatusCode, body: &str) -> anyhow::Error {
    if status == StatusCode::NOT_FOUND {
        anyhow!(
            "no logs for that run/job/attempt. Check the run number against \
             `fj run list`, and that the job index (`--job`) exists in the run."
        )
    } else if body.contains("resource does not exist") {
        anyhow!(
            "logs are no longer available for this run. Forgejo prunes Actions \
             logs after its retention period, or they were never recorded."
        )
    } else {
        anyhow!("could not fetch logs (HTTP {status})")
    }
}

/// POST the ViewPost route. With an empty `cursors` slice this returns run/job
/// state and no log lines; with cursors it returns the requested steps' lines.
pub async fn fetch_run_view(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
    attempt: u32,
    cursors: &[LogCursor],
) -> Result<RunView> {
    let url = web_run_url(client, owner, name, run, job, attempt)?;
    let body = serde_json::json!({ "logCursors": cursors });
    let res = client.request(Method::POST, &url, &[], Some(&body)).await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(log_route_error(status, &body));
    }
    res.json().await.context("decoding run view JSON")
}

/// Latest attempt number for a run+job. Probes attempt 1 (the original always
/// exists) and reads the server's own attempt list, so reruns are followed
/// without the caller guessing.
pub async fn latest_attempt(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
) -> Result<u32> {
    let view = fetch_run_view(client, owner, name, run, job, 1, &[]).await?;
    let max = view
        .state
        .current_job
        .all_attempts
        .iter()
        .map(|a| a.number)
        .max()
        .unwrap_or(1);
    Ok(max.max(1))
}

/// Fetch every step's log lines for a run+job+attempt in one shot. Does a
/// cheap empty-body call first to learn the step count, then requests cursors
/// 0..n. The returned `RunView` carries both the lines and the step metadata.
pub async fn fetch_job_logs(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
    attempt: u32,
) -> Result<RunView> {
    let meta = fetch_run_view(client, owner, name, run, job, attempt, &[]).await?;
    let n = meta.state.current_job.steps.len();
    if n == 0 {
        return Ok(meta);
    }
    let cursors: Vec<LogCursor> = (0..n)
        .map(|step| LogCursor {
            step,
            cursor: 0,
            expanded: true,
        })
        .collect();
    fetch_run_view(client, owner, name, run, job, attempt, &cursors).await
}

/// Run-level state for `fj run view N` (title, status, jobs, commit), with no
/// log lines fetched. Uses attempt 1, which always exists; the run-level state
/// in `state.run` is the same regardless of which attempt's URL we ask for.
pub async fn fetch_run_summary(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<RunView> {
    fetch_run_view(client, owner, name, run, 0, 1, &[]).await
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
