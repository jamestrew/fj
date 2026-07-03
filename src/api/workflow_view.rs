//! Forgejo Actions run view + logs, served from the web frontend routes.

use anyhow::{Context, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::Client;

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
// UI talks to. So, unlike `list_runs` (an ordinary typed `/api/v1`
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
    #[serde(
        default,
        rename = "stepsLog",
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub steps_log: Vec<StepLog>,
}

#[derive(Debug, Deserialize)]
pub struct StepLog {
    #[serde(default)]
    pub step: usize,
    /// Opaque resume position to send back as the next request's `cursor`.
    #[serde(default)]
    pub cursor: u64,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
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
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
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
    #[allow(dead_code)]
    pub title: String,
    #[serde(
        default,
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub steps: Vec<StepBrief>,
    #[serde(
        default,
        rename = "allAttempts",
        deserialize_with = "crate::api::serde_util::deserialize_null_to_default"
    )]
    pub all_attempts: Vec<AttemptBrief>,
}

#[derive(Debug, Deserialize)]
pub struct StepBrief {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub status: String,
    #[serde(default)]
    #[allow(dead_code)]
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
        "{origin}{}/{}/actions/runs/{run}/jobs/{job}/attempt/{attempt}",
        crate::api::path_segment(owner),
        crate::api::path_segment(name)
    ))
}

fn missing_logs_error() -> anyhow::Error {
    anyhow::anyhow!(
        "no logs for that run/job/attempt. Check the run number against \
         `fj run list`, and that the job index (`--job`) exists in the run."
    )
}

fn private_log_auth_error(run: u64) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot read logs for run #{run}: the Forgejo web log route rejected API token auth. \
         Log retrieval on private repos needs a browser session (cookie/CSRF); an API token \
         is not accepted by this route. Open the run in the Forgejo web UI for logs; see \
         rasterstate/fj#103."
    )
}

async fn token_list_contains_run(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<bool> {
    let runs = crate::api::workflow_run::list_runs(client, owner, name, 50).await?;
    Ok(runs.iter().any(|r| r.run_number == run))
}

async fn web_summary_has_run(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<Option<RunView>> {
    let url = web_run_url(client, owner, name, run, 0, 1)?;
    let body = serde_json::json!({ "logCursors": [] });
    let res = client.request(Method::POST, &url, &[], Some(&body)).await?;
    if res.status().is_success() {
        return res.json().await.context("decoding run view JSON").map(Some);
    }
    Ok(None)
}

/// Map a failed web-route response to a message that explains itself, instead
/// of leaking the raw "Not found." / "resource does not exist" web body.
async fn log_route_error(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
    status: StatusCode,
    body: &str,
) -> anyhow::Error {
    if status == StatusCode::NOT_FOUND {
        if job != 0 {
            if let Ok(Some(_summary)) = web_summary_has_run(client, owner, name, run).await {
                return missing_logs_error();
            }
        }

        match token_list_contains_run(client, owner, name, run).await {
            Ok(true) => private_log_auth_error(run),
            Ok(false) | Err(_) => missing_logs_error(),
        }
    } else if body.contains("resource does not exist") {
        anyhow::anyhow!(
            "logs are no longer available for this run. Forgejo prunes Actions \
             logs after its retention period, or they were never recorded."
        )
    } else {
        anyhow::anyhow!("could not fetch logs (HTTP {status})")
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
        return Err(log_route_error(client, owner, name, run, job, status, &body).await);
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
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        let base = url::Url::parse("https://example.com/api/v1/").unwrap();
        Client::for_base_url(base, "t".into()).unwrap()
    }

    #[test]
    fn web_run_url_builds_origin_anchored_path() {
        let c = client();
        let url = web_run_url(&c, "octo", "repo", 56, 0, 1).unwrap();
        assert_eq!(
            url,
            "https://example.com/octo/repo/actions/runs/56/jobs/0/attempt/1"
        );
    }

    #[test]
    fn web_run_url_includes_job_and_attempt() {
        let c = client();
        let url = web_run_url(&c, "octo", "repo", 7, 3, 2).unwrap();
        assert_eq!(
            url,
            "https://example.com/octo/repo/actions/runs/7/jobs/3/attempt/2"
        );
    }

    #[test]
    fn run_view_tolerates_null_arrays() {
        // A running / blocked job can return `null` where the UI sends arrays;
        // `#[serde(default)]` alone rejects an explicit null, so this used to
        // fail with "invalid type: null, expected a sequence".
        let json = serde_json::json!({
            "logs": { "stepsLog": [ { "step": 0, "cursor": 0, "lines": null } ] },
            "state": {
                "run": { "jobs": null },
                "currentJob": { "steps": null, "allAttempts": null }
            }
        });
        let view: RunView = serde_json::from_value(json).expect("null arrays decode to empty");
        assert!(view.state.run.jobs.is_empty());
        assert!(view.state.current_job.steps.is_empty());
        assert!(view.state.current_job.all_attempts.is_empty());
        assert_eq!(view.logs.steps_log.len(), 1);
        assert!(view.logs.steps_log[0].lines.is_empty());
    }

    #[test]
    fn missing_logs_error_is_friendly() {
        let err = missing_logs_error();
        let msg = err.to_string();
        assert!(msg.contains("no logs for that run/job/attempt"), "{msg}");
        assert!(msg.contains("fj run list"), "{msg}");
    }

    #[test]
    fn private_log_auth_error_is_actionable() {
        let err = private_log_auth_error(395);
        let msg = err.to_string();
        assert!(msg.contains("run #395"), "{msg}");
        assert!(msg.contains("rejected API token auth"), "{msg}");
        assert!(msg.contains("cookie/CSRF"), "{msg}");
        assert!(msg.contains("rasterstate/fj#103"), "{msg}");
    }
}
