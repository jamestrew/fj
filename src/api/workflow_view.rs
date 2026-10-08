//! Forgejo Actions run page, read from the web frontend routes as a fallback.

use anyhow::{Context, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::workflow_logs::{
    aggregate_job_status, find_run, list_run_jobs_api, WorkflowRunJob,
};
use crate::api::workflow_run::{
    actions_limitation, is_actions_unavailable, run_web_url, web_client, WorkflowRun,
};
use crate::client::{Capability, Client, Support};

// ── Run page ─────────────────────────────────────────────────────────────────
//
// THIS SECTION IS DELIBERATELY DIFFERENT FROM EVERY OTHER API MODULE.
//
// What `/api/v1` exposes for Actions runs depends on the server, and the gate
// table in `client::capability` records it per (flavor, version). Forgejo 16
// and Gitea 1.25 list a run's jobs to an API token; Forgejo 15 does not, and
// there the only place the run's jobs and live step output exist is the web
// frontend the browser Actions UI talks to, under `/{owner}/{repo}/actions/`.
//
// `fetch_run_overview` (for `fj run view N`) therefore asks the API first and
// only falls back to the web route where the API cannot answer. `fj run watch`
// does the same in `cli::workflow_run`.
//
// THE WEB ROUTES ARE NEVER SENT THE FORGE TOKEN. They authenticate a browser
// session (cookie plus CSRF) and ignore `Authorization: token`, so attaching
// the token bought nothing and put a credential on a route that has no use for
// it. Every request below goes through `web_client`, which is the same client
// with the token removed: still the one HTTP path (`Client::request`), no
// second client. A publicly readable run answers; a private one answers 404,
// which is reported as the limitation together with the run's URL.
//
// The web route:
//
//   * incremental per-step JSON (what the web fallback of `fj run watch` polls):
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
// that only proxies `/api/v1` will not forward them (`web_client` refuses that
// case with a clear message). Old runs whose logs have been pruned return the
// same "resource does not exist" 500; we translate that to a friendly notice.

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
    /// Modeled to document the wire shape; fj builds the run URL itself.
    #[serde(default)]
    #[allow(dead_code)]
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

fn private_log_auth_error(run: u64, run_url: &str) -> anyhow::Error {
    actions_limitation(
        &format!(
            "cannot read run #{run} over the web routes: they need a browser session \
             (cookie/CSRF) and do not accept an API token, so fj asks without one and only a \
             publicly readable run answers. Servers with the Actions API (Forgejo 16+, Gitea \
             1.25+) serve this over `/api/v1`, which `fj run view {run} --log` uses. See \
             rasterstate/fj#103"
        ),
        run_url,
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

/// `web` is the token-less client from `web_client`.
async fn web_summary_has_run(web: &Client, url: &str) -> Result<Option<RunView>> {
    let body = serde_json::json!({ "logCursors": [] });
    let res = web.request(Method::POST, url, &[], Some(&body)).await?;
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
    let run_url = run_web_url(client, owner, name, run);
    if status == StatusCode::NOT_FOUND {
        if job != 0 {
            if let (Ok(web), Ok(url)) = (
                web_client(client, "reading the run page", &run_url),
                web_run_url(client, owner, name, run, 0, 1),
            ) {
                if let Ok(Some(_summary)) = web_summary_has_run(&web, &url).await {
                    return missing_logs_error();
                }
            }
        }

        // The run list is `/api/v1`, so this one keeps the token.
        match token_list_contains_run(client, owner, name, run).await {
            Ok(true) => private_log_auth_error(run, &run_url),
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

/// POST the ViewPost route, without the forge token. With an empty `cursors`
/// slice this returns run/job state and no log lines; with cursors it returns
/// the requested steps' lines.
pub async fn fetch_run_view(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: usize,
    attempt: u32,
    cursors: &[LogCursor],
) -> Result<RunView> {
    let run_url = run_web_url(client, owner, name, run);
    let web = web_client(client, "reading the run page", &run_url)?;
    let url = web_run_url(client, owner, name, run, job, attempt)?;
    let body = serde_json::json!({ "logCursors": cursors });
    let res = web.request(Method::POST, &url, &[], Some(&body)).await?;
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

/// What `fj run view N` prints, whichever source answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunOverview {
    pub title: String,
    pub status: String,
    pub branch: String,
    pub sha: String,
    pub pushed_by: String,
    pub url: String,
    pub jobs: Vec<OverviewJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewJob {
    pub name: String,
    pub status: String,
    /// Only the web route reports one; empty from the API.
    pub duration: String,
}

/// One status for a run from the API. Gitea reports `completed` beside a
/// `conclusion`; Forgejo folds the outcome into `status`. With neither, the
/// jobs decide.
fn run_status(run: &WorkflowRun, jobs: &[WorkflowRunJob]) -> String {
    let conclusion = run.conclusion.as_deref().unwrap_or("");
    if !conclusion.is_empty() && matches!(run.status.as_str(), "" | "completed") {
        return conclusion.to_string();
    }
    if !run.status.is_empty() {
        return run.status.clone();
    }
    aggregate_job_status(jobs.iter().map(|j| j.status.as_str()))
        .unwrap_or("")
        .to_string()
}

pub fn overview_from_api(run: &WorkflowRun, jobs: &[WorkflowRunJob], url: &str) -> RunOverview {
    RunOverview {
        title: run.name.clone(),
        status: run_status(run, jobs),
        branch: run.head_branch.clone(),
        sha: run.head_sha.chars().take(10).collect(),
        pushed_by: String::new(),
        url: url.to_string(),
        jobs: jobs
            .iter()
            .enumerate()
            .map(|(index, job)| OverviewJob {
                name: job.display_name(index),
                status: job.status.clone(),
                duration: String::new(),
            })
            .collect(),
    }
}

pub fn overview_from_web(view: &RunView, url: &str) -> RunOverview {
    let run = &view.state.run;
    RunOverview {
        title: run.title.clone(),
        status: run.status.clone(),
        branch: run.commit.branch.name.clone(),
        sha: run.commit.short_sha.clone(),
        pushed_by: run.commit.pusher.display_name.clone(),
        url: url.to_string(),
        jobs: run
            .jobs
            .iter()
            .map(|job| OverviewJob {
                name: job.name.clone(),
                status: job.status.clone(),
                duration: job.duration.clone(),
            })
            .collect(),
    }
}

/// The run summary for `fj run view N`: from `/api/v1` where the server lists
/// a run's jobs, otherwise from the web route without the token. When neither
/// can answer, the error is the limitation and carries the run URL.
pub async fn fetch_run_overview(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<RunOverview> {
    let info = client.server_info().await;
    let url = run_web_url(client, owner, name, run);
    if info.supports(Capability::RunJobs) != Support::Unsupported {
        let found = find_run(client, owner, name, run).await?;
        match list_run_jobs_api(client, owner, name, run, found.id).await {
            Ok(jobs) => return Ok(overview_from_api(&found, &jobs, &url)),
            Err(err) if is_actions_unavailable(&err) => {}
            Err(err) => return Err(err),
        }
    }
    let view = fetch_run_summary(client, owner, name, run).await?;
    Ok(overview_from_web(&view, &url))
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
        let url = "https://example.com/octo/repo/actions/runs/395";
        let err = private_log_auth_error(395, url);
        assert!(is_actions_unavailable(&err));
        let msg = err.to_string();
        assert!(msg.contains("run #395"), "{msg}");
        assert!(msg.contains("do not accept an API token"), "{msg}");
        assert!(msg.contains("cookie/CSRF"), "{msg}");
        assert!(msg.contains("--log"), "{msg}");
        assert!(msg.contains("rasterstate/fj#103"), "{msg}");
        assert!(msg.contains(url), "{msg}");
    }

    #[test]
    fn overview_from_api_normalizes_status_and_names_jobs() {
        let run: WorkflowRun = serde_json::from_value(serde_json::json!({
            "id": 9, "run_number": 4, "name": "CI", "head_branch": "main",
            "head_sha": "0123456789abcdef", "status": "completed",
            "conclusion": "failure"
        }))
        .unwrap();
        let jobs = vec![
            WorkflowRunJob {
                id: 1,
                name: "build".into(),
                status: "success".into(),
                ..Default::default()
            },
            WorkflowRunJob {
                id: 2,
                status: "failure".into(),
                ..Default::default()
            },
        ];
        let o = overview_from_api(&run, &jobs, "https://h/o/r/actions/runs/4");
        assert_eq!(o.title, "CI");
        assert_eq!(o.status, "failure");
        assert_eq!(o.sha, "0123456789");
        assert_eq!(o.jobs[1].name, "job 1");
        assert_eq!(o.url, "https://h/o/r/actions/runs/4");

        let bare: WorkflowRun =
            serde_json::from_value(serde_json::json!({ "id": 9, "run_number": 4 })).unwrap();
        assert_eq!(overview_from_api(&bare, &jobs, "u").status, "failure");
    }

    #[test]
    fn overview_from_web_keeps_pusher_and_durations() {
        let view: RunView = serde_json::from_value(serde_json::json!({
            "state": {
                "run": {
                    "title": "fix", "status": "success",
                    "commit": { "shortSHA": "abc", "branch": { "name": "main" },
                                "pusher": { "displayName": "alice" } },
                    "jobs": [ { "name": "build", "status": "success", "duration": "3s" } ]
                },
                "currentJob": {}
            }
        }))
        .unwrap();
        let o = overview_from_web(&view, "u");
        assert_eq!((o.title.as_str(), o.pushed_by.as_str()), ("fix", "alice"));
        assert_eq!(o.jobs[0].duration, "3s");
    }
}
