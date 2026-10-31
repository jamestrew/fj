//! Workflow run logs through the token-authenticated Actions API, with a Loki
//! fallback.
//!
//! Which servers have the job and job-log routes is decided by the gate table
//! in `client::capability`, keyed on (flavor, version). A server known not to
//! have them is never asked: fj reads from an explicitly configured Loki, or
//! reports the limitation with the run's web URL. Job logs are read in bounded
//! byte ranges and continued until the server says the log is complete.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_RANGE, RANGE};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::api::workflow_run::{
    actions_limitation, is_actions_unavailable, run_web_url, WorkflowRun,
};
use crate::client::capability::unsupported_reason;
use crate::client::{Capability, Client, ServerInfo, Support};

const DEFAULT_LOKI_URL: &str = "http://100.105.235.123:3100";
const LOKI_LIMIT: &str = "5000";

/// Bytes asked for per job-log request.
pub(crate) const LOG_CHUNK_BYTES: u64 = 1024 * 1024;
/// Ceiling on one job's log held in memory. A longer log is cut here with a
/// visible note instead of growing without bound.
pub(crate) const LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunLogs {
    pub source: String,
    pub jobs: Vec<JobLogs>,
    /// Jobs whose log could not be retrieved (as opposed to confirmed missing).
    /// The logs that did load are still in `jobs`; the caller prints them and
    /// then fails, so a partial read never exits as a complete one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl RunLogs {
    pub fn status(&self) -> Option<&str> {
        aggregate_job_status(self.jobs.iter().map(|j| j.status.as_str()))
    }
}

/// A job status that will not change again.
pub fn is_terminal_job_status(status: &str) -> bool {
    matches!(
        status,
        "success" | "failure" | "cancelled" | "skipped" | "timed_out"
    )
}

/// One status for a set of jobs: the worst outcome wins, all green is
/// `success`, otherwise the first status reported.
pub fn aggregate_job_status<'a>(statuses: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let statuses: Vec<&str> = statuses.into_iter().collect();
    for worst in ["failure", "cancelled", "timed_out"] {
        if statuses.contains(&worst) {
            return Some(worst);
        }
    }
    if statuses.iter().all(|s| matches!(*s, "success" | "skipped")) {
        return Some("success");
    }
    statuses.into_iter().find(|s| !s.is_empty())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct JobLogs {
    pub job: usize,
    pub task_id: Option<u64>,
    pub job_id: Option<u64>,
    pub name: String,
    pub status: String,
    pub lines: Vec<LogLine>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogLine {
    pub timestamp: Option<String>,
    pub stream: Option<String>,
    pub message: String,
}

#[derive(Clone)]
pub struct LogRequest<'a> {
    pub client: &'a Client,
    pub owner: &'a str,
    pub name: &'a str,
    pub job: usize,
    pub attempt: Option<u32>,
    pub task: Option<&'a WorkflowTask>,
    pub api_job: Option<&'a WorkflowRunJob>,
}

pub trait LogSource {
    fn available(&self) -> bool;

    fn fetch<'a>(
        &'a self,
        req: LogRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = LogSourceResult<JobLogs>> + Send + 'a>>;
}

#[derive(Debug)]
pub enum LogSourceError {
    Unavailable(String),
    Other(anyhow::Error),
}

type LogSourceResult<T> = std::result::Result<T, LogSourceError>;

impl fmt::Display for LogSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogSourceError::Unavailable(msg) => write!(f, "{msg}"),
            LogSourceError::Other(err) => write!(f, "{err}"),
        }
    }
}

impl Error for LogSourceError {}

impl From<anyhow::Error> for LogSourceError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct WorkflowTask {
    #[serde(default, deserialize_with = "crate::api::serde_util::string_or_number")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "display_title")]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, alias = "conclusion")]
    pub result: String,
    #[serde(default, alias = "started", alias = "started_at")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(
        default,
        alias = "stopped",
        alias = "stopped_at",
        alias = "completed_at"
    )]
    pub stopped_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "created", alias = "created_at")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "updated", alias = "updated_at")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl WorkflowTask {
    fn id_u64(&self) -> Result<u64> {
        self.id
            .parse::<u64>()
            .with_context(|| format!("workflow task has non-numeric id `{}`", self.id))
    }

    fn display_name(&self, index: usize) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if !self.title.is_empty() {
            return self.title.clone();
        }
        format!("job {index}")
    }

    fn display_status(&self) -> String {
        if !self.status.is_empty() {
            return self.status.clone();
        }
        self.result.clone()
    }
}

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct WorkflowRunJob {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    /// Runner task id. Forgejo 16 reports it inline on the jobs route, which
    /// is the only cheap way to name the task without listing every task in
    /// the repository.
    #[serde(default)]
    pub task_id: Option<u64>,
}

impl WorkflowRunJob {
    pub fn display_name(&self, index: usize) -> String {
        if self.name.is_empty() {
            format!("job {index}")
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ForgejoApiLogSource;

impl LogSource for ForgejoApiLogSource {
    fn available(&self) -> bool {
        true
    }

    fn fetch<'a>(
        &'a self,
        req: LogRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = LogSourceResult<JobLogs>> + Send + 'a>> {
        Box::pin(async move {
            let api_job = req.api_job.ok_or_else(|| {
                LogSourceError::Unavailable(
                    "Forgejo native workflow logs endpoint is unavailable".into(),
                )
            })?;
            let attempt = req
                .attempt
                .or_else(|| (api_job.attempt > 0).then_some(api_job.attempt));
            let Some(log) = read_job_log(
                req.client,
                req.owner,
                req.name,
                api_job.id,
                attempt,
                0,
                LOG_CHUNK_BYTES,
                LOG_MAX_BYTES,
            )
            .await?
            else {
                return Err(LogSourceError::Unavailable(
                    "Forgejo native workflow logs endpoint is unavailable".into(),
                ));
            };
            let text = String::from_utf8_lossy(&log.bytes);
            let mut lines: Vec<LogLine> = text
                .lines()
                .map(|message| LogLine {
                    timestamp: None,
                    stream: None,
                    message: message.to_string(),
                })
                .collect();
            if log.truncated {
                lines.push(LogLine {
                    timestamp: None,
                    stream: None,
                    message: format!(
                        "[fj] log truncated after {} MiB; open the run in the web UI for the rest",
                        LOG_MAX_BYTES / (1024 * 1024)
                    ),
                });
            }
            Ok(JobLogs {
                job: req.job,
                task_id: api_job
                    .task_id
                    .or_else(|| req.task.and_then(|task| task.id_u64().ok())),
                job_id: Some(api_job.id),
                name: api_job.display_name(req.job),
                status: api_job.status.clone(),
                lines,
            })
        })
    }
}

/// Bytes of one job's log, read from `start`.
#[derive(Debug, Default)]
pub(crate) struct JobLogBytes {
    pub bytes: Vec<u8>,
    /// The read stopped at the `max` ceiling with more log left on the server.
    pub truncated: bool,
}

/// Parse `Content-Range: bytes <start>-<end>/<total|*>`.
pub(crate) fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    Some((start, end, total.trim().parse().ok()))
}

/// Read one job's log from the token-authenticated API route, in `chunk`-sized
/// byte ranges starting at `start`, following each `206 Partial Content` with
/// the next range until the server reports the end (`Content-Range` total, a
/// short chunk, or `416`). At most `max` bytes are kept.
///
/// `Ok(None)` means the route answered 404 for the first range: either the
/// server has no such route or the job has no recorded log. A server that
/// ignores `Range` answers `200` with the whole log, which is taken as is.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_job_log(
    client: &Client,
    owner: &str,
    name: &str,
    job_id: u64,
    attempt: Option<u32>,
    start: u64,
    chunk: u64,
    max: u64,
) -> Result<Option<JobLogBytes>> {
    let path = format!(
        "{}/actions/jobs/{job_id}/logs",
        crate::api::repo_path(owner, name)
    );
    let query = attempt
        .map(|attempt| vec![("attempt".to_string(), attempt.to_string())])
        .unwrap_or_default();
    let chunk = chunk.max(1);
    let mut out = JobLogBytes::default();
    let mut offset = start;
    loop {
        if out.bytes.len() as u64 >= max {
            out.truncated = true;
            break;
        }
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/plain"));
        headers.insert(
            RANGE,
            HeaderValue::from_str(&format!("bytes={offset}-{}", offset + chunk - 1))
                .context("building Range header")?,
        );
        let res = client
            .request_with_headers(Method::GET, &path, &query, None, &headers)
            .await?;
        let status = res.status();
        if status == StatusCode::NOT_FOUND {
            if offset == start {
                return Ok(None);
            }
            break;
        }
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            // Nothing at or after `offset`: the log ends here (for now).
            break;
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let range = res
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            let body = res
                .bytes()
                .await
                .context("reading Forgejo workflow log response")?;
            if let Some((got, _, _)) = range {
                if got != offset {
                    return Err(anyhow!(
                        "workflow log server answered byte {got} when asked for byte {offset}"
                    ));
                }
            }
            if body.is_empty() {
                break;
            }
            offset += body.len() as u64;
            out.bytes.extend_from_slice(&body);
            let done = match range {
                Some((_, _, Some(total))) => offset >= total,
                _ => (body.len() as u64) < chunk,
            };
            if done {
                break;
            }
            continue;
        }
        if status.is_success() {
            // `Range` was ignored: this is the whole log from byte 0.
            crate::client::check_body_size(&res)?;
            let body = res
                .bytes()
                .await
                .context("reading Forgejo workflow log response")?;
            let skip = usize::try_from(start).unwrap_or(usize::MAX).min(body.len());
            out.bytes = body[skip..].to_vec();
            break;
        }
        let body = res.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Forgejo workflow log fetch failed ({status}): {}",
            body.trim()
        ));
    }
    Ok(Some(out))
}

/// Append `new` to `pending` and take every complete line out of it, leaving
/// a trailing partial line for the next chunk. Used by `fj run watch`.
pub fn split_complete_lines(pending: &mut Vec<u8>, new: &[u8]) -> Vec<String> {
    pending.extend_from_slice(new);
    let Some(last) = pending.iter().rposition(|b| *b == b'\n') else {
        return Vec::new();
    };
    let complete: Vec<u8> = pending.drain(..=last).collect();
    String::from_utf8_lossy(&complete)
        .lines()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone)]
pub struct LokiLogSource {
    base_url: Url,
}

impl LokiLogSource {
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("FJ_RUN_LOG_LOKI_URL")
            .or_else(|_| std::env::var("FJ_LOKI_URL"))
            .unwrap_or_else(|_| DEFAULT_LOKI_URL.to_string());
        Self::new(&raw)
    }

    /// The Loki the user pointed fj at, if any. Unlike `from_env` there is no
    /// built-in default: a server without the log API is only read through
    /// Loki when someone configured one on purpose.
    pub fn configured() -> Result<Option<Self>> {
        match std::env::var("FJ_RUN_LOG_LOKI_URL").or_else(|_| std::env::var("FJ_LOKI_URL")) {
            Ok(raw) if !raw.trim().is_empty() => Self::new(raw.trim()).map(Some),
            _ => Ok(None),
        }
    }

    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            base_url: Url::parse(base_url).context("parsing Loki URL")?,
        })
    }
}

impl LogSource for LokiLogSource {
    fn available(&self) -> bool {
        true
    }

    fn fetch<'a>(
        &'a self,
        req: LogRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = LogSourceResult<JobLogs>> + Send + 'a>> {
        Box::pin(async move {
            let task = req.task.ok_or_else(|| {
                LogSourceError::Unavailable("Loki log source needs a workflow task id".into())
            })?;
            let task_id = task.id_u64()?;
            let query = loki_query(task_id);
            let (start, end) = task_window(task);
            let url = self
                .base_url
                .join("/loki/api/v1/query_range")
                .context("building Loki query URL")?;
            let res = req
                .client
                .http()
                .get(url)
                .query(&[
                    ("query", query.as_str()),
                    ("start", start.as_str()),
                    ("end", end.as_str()),
                    ("direction", "forward"),
                    ("limit", LOKI_LIMIT),
                ])
                .send()
                .await
                .context("querying Loki for workflow logs")?;
            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(LogSourceError::Other(anyhow!(
                    "Loki workflow log query failed ({status}): {}",
                    body.trim()
                )));
            }
            let payload: LokiResponse = res.json().await.context("decoding Loki workflow logs")?;
            let mut entries = payload.into_lines();
            entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            let lines = entries
                .into_iter()
                .map(|entry| LogLine {
                    timestamp: Some(entry.timestamp),
                    stream: entry.stream,
                    message: clean_loki_line(&entry.message),
                })
                .collect();
            Ok(JobLogs {
                job: req.job,
                task_id: Some(task_id),
                job_id: req.api_job.map(|job| job.id),
                name: task.display_name(req.job),
                status: task.display_status(),
                lines,
            })
        })
    }
}

/// Where the Loki fallback comes from.
enum LokiChoice {
    /// `FJ_RUN_LOG_LOKI_URL` / `FJ_LOKI_URL`.
    Env,
    #[cfg(test)]
    Base(String),
    #[cfg(test)]
    Disabled,
}

impl LokiChoice {
    /// Loki for a job whose log the API reports missing. Keeps the historical
    /// built-in default.
    fn for_missing_job_log(&self) -> Result<Option<LokiLogSource>> {
        match self {
            LokiChoice::Env => LokiLogSource::from_env().map(Some),
            #[cfg(test)]
            LokiChoice::Base(base) => LokiLogSource::new(base).map(Some),
            #[cfg(test)]
            LokiChoice::Disabled => Ok(None),
        }
    }

    /// Loki for a server with no log API at all: only one the user configured.
    fn for_older_server(&self) -> Result<Option<LokiLogSource>> {
        match self {
            LokiChoice::Env => LokiLogSource::configured(),
            #[cfg(test)]
            LokiChoice::Base(base) => LokiLogSource::new(base).map(Some),
            #[cfg(test)]
            LokiChoice::Disabled => Ok(None),
        }
    }
}

struct NoLoki;

impl LogSource for NoLoki {
    fn available(&self) -> bool {
        false
    }

    fn fetch<'a>(
        &'a self,
        _req: LogRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = LogSourceResult<JobLogs>> + Send + 'a>> {
        Box::pin(async move {
            Err(LogSourceError::Unavailable(
                "no Loki log source is configured".into(),
            ))
        })
    }
}

pub async fn fetch_run_logs(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
) -> Result<RunLogs> {
    fetch_run_logs_inner(
        client,
        owner,
        name,
        run,
        job,
        attempt,
        failed_only,
        LokiChoice::Env,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_run_logs_with_loki_base(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
    loki_base: &str,
) -> Result<RunLogs> {
    fetch_run_logs_inner(
        client,
        owner,
        name,
        run,
        job,
        attempt,
        failed_only,
        LokiChoice::Base(loki_base.to_string()),
    )
    .await
}

/// `fetch_run_logs` with no Loki at all, so a test never depends on the
/// developer's environment.
#[cfg(test)]
pub(crate) async fn fetch_run_logs_without_loki(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<RunLogs> {
    fetch_run_logs_inner(
        client,
        owner,
        name,
        run,
        None,
        None,
        false,
        LokiChoice::Disabled,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn fetch_run_logs_inner(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
    loki: LokiChoice,
) -> Result<RunLogs> {
    let tasks = RemoteTasks {
        client,
        owner,
        name,
        run,
    };
    let api_jobs = match run_jobs_for_logs(client, owner, name, run).await {
        Ok(jobs) => jobs,
        Err(err) if is_actions_unavailable(&err) => {
            // No token-authenticated log route here. A separately authorized
            // source is the only other way in; without one, say so and hand
            // over the run URL.
            let Some(source) = loki.for_older_server()? else {
                return Err(err);
            };
            return fetch_run_logs_from_tasks(
                client,
                owner,
                name,
                run,
                job,
                attempt,
                failed_only,
                &source,
                &tasks,
            )
            .await;
        }
        Err(err) => return Err(err),
    };
    match loki.for_missing_job_log()? {
        Some(source) => {
            fetch_run_logs_with_sources(
                client,
                owner,
                name,
                run,
                job,
                attempt,
                failed_only,
                &ForgejoApiLogSource,
                &source,
                &tasks,
                &api_jobs,
            )
            .await
        }
        None => {
            fetch_run_logs_with_sources(
                client,
                owner,
                name,
                run,
                job,
                attempt,
                failed_only,
                &ForgejoApiLogSource,
                &NoLoki,
                &tasks,
                &api_jobs,
            )
            .await
        }
    }
}

/// The gate for reading a run's logs over the API: both the jobs route and the
/// job-log route must be on offer. `Err` is the limitation, with the run URL.
pub fn log_api_gate(info: &ServerInfo, run_url: &str) -> Result<()> {
    for (cap, what) in [
        (Capability::RunJobs, "list a run's jobs"),
        (Capability::JobLogs, "read job logs"),
    ] {
        if info.supports(cap) == Support::Unsupported {
            return Err(actions_limitation(
                &unsupported_reason(info, cap, what),
                run_url,
            ));
        }
    }
    Ok(())
}

async fn run_jobs_for_logs(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<Vec<WorkflowRunJob>> {
    let info = client.server_info().await;
    log_api_gate(&info, &run_web_url(client, owner, name, run))?;
    let run_id = resolve_run_api_id(client, owner, name, run).await?;
    list_run_jobs_api(client, owner, name, run, run_id).await
}

/// Find a run by its per-repo run number. The job, log, artifact and action
/// routes all want the INTERNAL run id, which only the run list reveals.
pub(crate) async fn find_run(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
) -> Result<WorkflowRun> {
    crate::api::workflow_run::find_run_by_number(client, owner, name, run_number)
        .await?
        .ok_or_else(|| anyhow!("workflow run #{run_number} was not found; check `fj run list`"))
}

pub(crate) async fn resolve_run_api_id(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
) -> Result<u64> {
    Ok(find_run(client, owner, name, run_number).await?.id)
}

/// Decode a run's job list. Forgejo answers a bare array and folds the outcome
/// into `status`; Gitea wraps the array in `{"jobs": [...]}`, names the attempt
/// `run_attempt`, and reports `status: completed` beside a `conclusion`. Both
/// are normalized to the Forgejo shape so every caller reads one `status`.
pub fn parse_run_jobs(value: Value) -> Result<Vec<WorkflowRunJob>> {
    let items = match value {
        Value::Array(items) => items,
        Value::Object(mut map) => match map.remove("jobs") {
            Some(Value::Array(items)) => items,
            _ => return Err(anyhow!("unexpected workflow run jobs response shape")),
        },
        _ => return Err(anyhow!("unexpected workflow run jobs response shape")),
    };
    items
        .into_iter()
        .map(|mut item| {
            if let Some(obj) = item.as_object_mut() {
                let conclusion = obj
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
                let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
                if let Some(conclusion) = conclusion {
                    if status.is_empty() || status == "completed" {
                        obj.insert("status".into(), Value::String(conclusion));
                    }
                }
                if !obj.contains_key("attempt") {
                    if let Some(attempt) = obj.get("run_attempt").cloned() {
                        obj.insert("attempt".into(), attempt);
                    }
                }
            }
            serde_json::from_value(item).context("decoding workflow run job")
        })
        .collect()
}

pub async fn list_run_jobs_api(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
    run_id: u64,
) -> Result<Vec<WorkflowRunJob>> {
    let info = client.server_info().await;
    let run_url = run_web_url(client, owner, name, run_number);
    if info.supports(Capability::RunJobs) == Support::Unsupported {
        return Err(actions_limitation(
            &unsupported_reason(&info, Capability::RunJobs, "list a run's jobs"),
            &run_url,
        ));
    }
    let path = format!(
        "{}/actions/runs/{run_id}/jobs",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::GET, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return parse_run_jobs(
            res.json()
                .await
                .context("decoding workflow run jobs JSON")?,
        );
    }
    let body = res.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        return Err(actions_limitation(
            &format!(
                "cannot read the jobs of run #{run_number} with an API token: this server ({}) \
                 does not serve `/api/v1/repos/{{owner}}/{{repo}}/actions/runs/{{run_id}}/jobs` \
                 (HTTP 404). See rasterstate/fj#103",
                info.describe()
            ),
            &run_url,
        ));
    }
    Err(anyhow!(
        "could not list workflow run jobs (HTTP {}){}",
        status.as_u16(),
        response_suffix(&body)
    ))
}

fn response_suffix(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}

/// Supplies the run's task metadata, but only when something asks for it.
///
/// `GET /actions/tasks` ignores its `run` filter on Forgejo 16, so it answers
/// with every task in the repository: measured at 3.2 MB and ~30 s on a
/// moderately busy repo. Only the Loki fallback needs that metadata (for the
/// per-task container name and the query window), so the native API path must
/// never pay for it.
///
/// `wanted` names the task ids the caller must find. A source that has to page
/// through a long list stops once it has them; an empty `wanted` means "the
/// run's tasks, whichever they are".
pub(crate) trait TaskSource: Send + Sync {
    fn load<'a>(
        &'a self,
        wanted: &'a [u64],
    ) -> Pin<Box<dyn Future<Output = Result<TaskList>> + Send + 'a>>;
}

/// The tasks a read found, and why it stopped short if it did. A task missing
/// from an incomplete list is unresolved, not absent, and the tasks that were
/// found stay usable: one page failing must not cost the logs of tasks already
/// resolved.
#[derive(Debug, Clone, Default)]
pub(crate) struct TaskList {
    pub tasks: Vec<WorkflowTask>,
    pub incomplete: Option<String>,
}

impl TaskList {
    fn complete(tasks: Vec<WorkflowTask>) -> Self {
        Self {
            tasks,
            incomplete: None,
        }
    }
}

/// Tasks already in hand, which is how tests and callers with a cached list
/// supply them.
impl TaskSource for Vec<WorkflowTask> {
    fn load<'a>(
        &'a self,
        _wanted: &'a [u64],
    ) -> Pin<Box<dyn Future<Output = Result<TaskList>> + Send + 'a>> {
        Box::pin(async move { Ok(TaskList::complete(self.clone())) })
    }
}

/// Tasks fetched from the server on first ask. A failed read is an error, not
/// an empty list: "the server could not say" must stay distinct from "the run
/// has no tasks".
pub(crate) struct RemoteTasks<'a> {
    pub client: &'a Client,
    pub owner: &'a str,
    pub name: &'a str,
    pub run: u64,
}

impl TaskSource for RemoteTasks<'_> {
    fn load<'a>(
        &'a self,
        wanted: &'a [u64],
    ) -> Pin<Box<dyn Future<Output = Result<TaskList>> + Send + 'a>> {
        Box::pin(async move {
            list_run_tasks(self.client, self.owner, self.name, self.run, wanted).await
        })
    }
}

/// Tasks per page asked of `/actions/tasks`, and the most pages one lookup walks.
/// The list is every task in the repository, newest first, so a run's tasks sit
/// near the top; 40 pages of 50 reaches 2000 tasks back before giving up.
const TASK_PAGE_SIZE: usize = 50;
const TASK_PAGE_LIMIT: u32 = 40;

/// The run's tasks, read page by page until the ids in `wanted` are found, the
/// walk has passed them (the list is newest first), or the list is exhausted
/// (its `total_count` is reached, or a page adds nothing new). A short page
/// proves nothing, since a server may clamp the page size. A list cut off
/// before that, by the page cap or by a page that fails after the first, comes
/// back with the tasks found so far and `incomplete` set: never as a shorter
/// answer. A first page that fails is an error. With no `wanted` ids (the
/// older-server path) the walk goes to exhaustion, because a run's tasks need
/// not be contiguous.
pub async fn list_run_tasks(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    wanted: &[u64],
) -> Result<TaskList> {
    let path = format!("{}/actions/tasks", crate::api::repo_path(owner, name));
    let mut found: Vec<WorkflowTask> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for page in 1..=TASK_PAGE_LIMIT {
        let query = vec![
            ("run".to_string(), run.to_string()),
            ("limit".to_string(), TASK_PAGE_SIZE.to_string()),
            ("page".to_string(), page.to_string()),
        ];
        let read = async {
            let value: Value = client
                .json(
                    Method::GET,
                    &path,
                    &query,
                    Option::<&serde_json::Value>::None,
                )
                .await?;
            let total = value.get("total_count").and_then(Value::as_u64);
            let everything = parse_workflow_tasks_for_run(value.clone(), None)?;
            let ours = parse_workflow_tasks_for_run(value, Some(run))?;
            Ok::<_, anyhow::Error>((total, everything, ours))
        };
        let (total, everything, ours) = match read.await {
            Ok(page) => page,
            Err(err) if page == 1 => return Err(err),
            Err(err) => {
                return Ok(TaskList {
                    tasks: found,
                    incomplete: Some(format!("page {page} failed: {err}")),
                })
            }
        };
        let fresh_ids: std::collections::HashSet<String> = everything
            .iter()
            .filter(|t| seen.insert(t.id.clone()))
            .map(|t| t.id.clone())
            .collect();
        let page_min = everything.iter().filter_map(|t| t.id_u64().ok()).min();
        found.extend(ours.into_iter().filter(|t| fresh_ids.contains(&t.id)));

        let have_all = !wanted.is_empty()
            && wanted
                .iter()
                .all(|id| found.iter().any(|t| t.id_u64().ok() == Some(*id)));
        let passed_wanted = match (wanted.iter().min(), page_min) {
            (Some(lowest), Some(min)) => min < *lowest,
            _ => false,
        };
        // No new task on this page: the server ignores paging and sent the
        // whole list (or this is past its end), so all of it has been read.
        let exhausted = fresh_ids.is_empty() || total.is_some_and(|n| seen.len() as u64 >= n);
        if have_all || passed_wanted || exhausted {
            return Ok(TaskList::complete(found));
        }
    }
    Ok(TaskList {
        tasks: found,
        incomplete: Some(format!(
            "{TASK_PAGE_LIMIT} pages of {TASK_PAGE_SIZE} did not reach the end of the list"
        )),
    })
}

#[cfg(test)]
pub fn parse_workflow_tasks(value: Value) -> Result<Vec<WorkflowTask>> {
    parse_workflow_tasks_for_run(value, None)
}

/// Like `parse_workflow_tasks`, keeping only the tasks of `run` when the
/// entries carry a `run_number`. The task list ignores its `run` filter on
/// Forgejo 16 and answers with every task in the repository.
pub fn parse_workflow_tasks_for_run(value: Value, run: Option<u64>) -> Result<Vec<WorkflowTask>> {
    let values = if let Some(array) = value.as_array() {
        array.clone()
    } else if let Some(array) = value.get("tasks").and_then(Value::as_array) {
        array.clone()
    } else if let Some(array) = value.get("workflow_tasks").and_then(Value::as_array) {
        array.clone()
    } else if let Some(array) = value.get("jobs").and_then(Value::as_array) {
        array.clone()
    } else if let Some(array) = value.get("workflow_runs").and_then(Value::as_array) {
        // Forgejo 16 wraps the task list in `workflow_runs` despite the
        // entries being tasks. Measured on 16.0.3.
        array.clone()
    } else {
        return Err(anyhow!("unexpected workflow task list response shape"));
    };

    values
        .into_iter()
        .filter(
            |value| match (run, value.get("run_number").and_then(Value::as_u64)) {
                (Some(run), Some(number)) => number == run,
                _ => true,
            },
        )
        .map(|value| serde_json::from_value(value).context("decoding workflow task"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn fetch_run_logs_with_sources(
    client: &Client,
    owner: &str,
    name: &str,
    _run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
    forgejo: &impl LogSource,
    loki: &impl LogSource,
    tasks: &dyn TaskSource,
    api_jobs: &[WorkflowRunJob],
) -> Result<RunLogs> {
    let selected = selected_jobs(api_jobs, job, failed_only)?;
    if selected.is_empty() {
        return Ok(RunLogs {
            source: "none".into(),
            jobs: Vec::new(),
            errors: Vec::new(),
        });
    }

    // Each job is settled on its own. One job whose log is missing (a job with
    // no task yet answers `resource does not exist`) must not discard the logs
    // of the jobs that did load, or fail the command on its own: that read a
    // real failure as infrastructure (CLS-963). A job whose retrieval *fails* is
    // kept apart from one whose log is confirmed missing: the logs that loaded
    // are still returned, and `errors` makes the caller fail after printing.
    let mut outcomes: Vec<JobOutcome> = Vec::with_capacity(selected.len());
    if forgejo.available() {
        for &(index, api_job) in &selected {
            let req = LogRequest {
                client,
                owner,
                name,
                job: index,
                attempt,
                task: None,
                api_job: Some(api_job),
            };
            outcomes.push(match forgejo.fetch(req).await {
                Ok(job) => JobOutcome::Got(job),
                Err(LogSourceError::Unavailable(msg)) => JobOutcome::Missing(msg),
                Err(LogSourceError::Other(err)) => JobOutcome::Failed(err.to_string()),
            });
        }
        if outcomes.iter().all(|o| matches!(o, JobOutcome::Got(_))) {
            return Ok(RunLogs {
                source: "forgejo-api".into(),
                jobs: outcomes
                    .into_iter()
                    .filter_map(|o| match o {
                        JobOutcome::Got(job) => Some(job),
                        _ => None,
                    })
                    .collect(),
                errors: Vec::new(),
            });
        }
    } else {
        outcomes.extend(
            selected.iter().map(|_| {
                JobOutcome::Missing("Forgejo workflow logs are not available".to_string())
            }),
        );
    }

    let served_by_forgejo = outcomes.iter().any(|o| matches!(o, JobOutcome::Got(_)));
    let needs_fallback = outcomes.iter().any(|o| matches!(o, JobOutcome::Missing(_)));
    if !served_by_forgejo && needs_fallback && !loki.available() {
        return Err(anyhow!("no workflow log source is available"));
    }

    // Task metadata is only needed for a job Forgejo could not serve. A failed
    // read of it is a failure for those jobs, never a missing log.
    let wanted: Vec<u64> = outcomes
        .iter()
        .zip(&selected)
        .filter(|(outcome, _)| matches!(outcome, JobOutcome::Missing(_)))
        .filter_map(|(_, &(_, api_job))| api_job.task_id)
        .collect();
    let tasks = if needs_fallback && loki.available() {
        tasks.load(&wanted).await.map_err(|err| err.to_string())
    } else {
        Ok(TaskList::complete(Vec::new()))
    };
    let mut served_by_loki = false;
    let mut jobs = Vec::with_capacity(selected.len());
    let mut errors: Vec<String> = Vec::new();
    for (outcome, &(index, api_job)) in outcomes.into_iter().zip(&selected) {
        let reason = match outcome {
            JobOutcome::Got(job) => {
                jobs.push(job);
                continue;
            }
            JobOutcome::Failed(err) => {
                jobs.push(failed_job(index, api_job, &err));
                errors.push(err);
                continue;
            }
            JobOutcome::Missing(reason) => reason,
        };
        let task = match (&tasks, loki.available()) {
            (Err(err), true) => {
                jobs.push(failed_job(index, api_job, err));
                errors.push(err.clone());
                continue;
            }
            (Ok(list), true) => match api_job.task_id {
                // The job names its task: find that task, and never read
                // another job's in its place. The task list can be capped or
                // ignore its run filter, so a task that is not in it is not
                // proof there is no log: it is a retrieval error.
                Some(task_id) => {
                    match list.tasks.iter().find(|t| t.id_u64().ok() == Some(task_id)) {
                        Some(task) => Some(task),
                        None => {
                            let err = format!(
                            "task {task_id} for this job is not in the repository task list{}, \
                             so its log cannot be read from Loki",
                            list.incomplete
                                .as_ref()
                                .map_or(String::new(), |why| format!(" (the list is incomplete: {why})"))
                        );
                            jobs.push(failed_job(index, api_job, &err));
                            errors.push(err);
                            continue;
                        }
                    }
                }
                // A server that does not report the task id: the run's task
                // list is in job order, as before, but only a complete list
                // can say which task is this job's.
                None => match &list.incomplete {
                    Some(why) => {
                        let err = format!(
                            "this job cannot be matched to a task: the task list is incomplete ({why})"
                        );
                        jobs.push(failed_job(index, api_job, &err));
                        errors.push(err);
                        continue;
                    }
                    None => list.tasks.get(index),
                },
            },
            (_, false) => None,
        };
        let Some(task) = task else {
            jobs.push(unavailable_job(index, api_job, &reason));
            continue;
        };
        let req = LogRequest {
            client,
            owner,
            name,
            job: index,
            attempt,
            task: Some(task),
            api_job: Some(api_job),
        };
        match loki.fetch(req).await {
            Ok(job) => {
                served_by_loki = true;
                jobs.push(job);
            }
            Err(LogSourceError::Unavailable(msg)) => {
                jobs.push(unavailable_job(index, api_job, &msg))
            }
            Err(LogSourceError::Other(err)) => {
                jobs.push(failed_job(index, api_job, &err.to_string()));
                errors.push(err.to_string());
            }
        }
    }

    // Nothing was retrieved at all: that is the failure itself, not a partial
    // read, so it stays the command's error.
    if !served_by_forgejo && !served_by_loki {
        if let Some(first) = errors.first() {
            return Err(anyhow!(first.clone()));
        }
    }
    let source = match (served_by_forgejo, served_by_loki) {
        (true, true) => "forgejo-api+loki",
        (true, false) => "forgejo-api",
        (false, true) => "loki",
        (false, false) => "none",
    };
    Ok(RunLogs {
        source: source.into(),
        jobs,
        errors,
    })
}

/// How one job's log read settled.
enum JobOutcome {
    Got(JobLogs),
    /// The server confirmed there is no log (yet): try the next source.
    Missing(String),
    /// The read itself failed (transport, decoding, a server error).
    Failed(String),
}

/// A job whose log could not be retrieved, marked in its own lines so the
/// printed run shows where the gap is. The caller still fails on `errors`.
fn failed_job(index: usize, api_job: &WorkflowRunJob, err: &str) -> JobLogs {
    JobLogs {
        lines: vec![LogLine {
            timestamp: None,
            stream: None,
            message: format!("log retrieval failed for this job: {err}"),
        }],
        ..unavailable_job(index, api_job, "")
    }
}

/// A job whose log no source could supply, reported as such in its own lines
/// so the rest of the run still reads.
fn unavailable_job(index: usize, api_job: &WorkflowRunJob, reason: &str) -> JobLogs {
    JobLogs {
        job: index,
        task_id: api_job.task_id,
        job_id: Some(api_job.id),
        name: api_job.display_name(index),
        status: api_job.status.clone(),
        lines: vec![LogLine {
            timestamp: None,
            stream: None,
            message: format!("log unavailable for this job: {reason}"),
        }],
    }
}

fn selected_jobs(
    jobs: &[WorkflowRunJob],
    job: Option<usize>,
    failed_only: bool,
) -> Result<Vec<(usize, &WorkflowRunJob)>> {
    if let Some(index) = job {
        let job = jobs.get(index).ok_or_else(|| {
            anyhow!(
                "job index {index} is out of range: this run has {} job(s)",
                jobs.len()
            )
        })?;
        return Ok(vec![(index, job)]);
    }

    Ok(jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| !failed_only || !matches!(job.status.as_str(), "success" | "skipped"))
        .collect())
}

/// Read a run's logs from Loki alone, addressing jobs by the run's task list.
/// This is the path for a server with no job API, so there are no API jobs to
/// lean on.
#[allow(clippy::too_many_arguments)]
async fn fetch_run_logs_from_tasks(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
    loki: &impl LogSource,
    tasks: &dyn TaskSource,
) -> Result<RunLogs> {
    let TaskList { tasks, incomplete } = tasks.load(&[]).await?;
    if tasks.is_empty() {
        return Err(match incomplete {
            Some(why) => anyhow!("the task list for run #{run} is incomplete: {why}"),
            None => anyhow!("Loki fallback found no workflow task metadata for run #{run}"),
        });
    }
    let selected = selected_tasks(&tasks, job, failed_only)?;
    // Each task is settled on its own, as on the native path: one failed Loki
    // query must not discard the logs of the tasks that loaded. A retrieval
    // failure is kept in `errors` so the caller fails after printing; a task
    // Loki confirms it has no log for reads as unavailable.
    let mut jobs = Vec::with_capacity(selected.len());
    let mut errors: Vec<String> = Vec::new();
    let mut served = false;
    for (index, task) in selected {
        let req = LogRequest {
            client,
            owner,
            name,
            job: index,
            attempt,
            task: Some(task),
            api_job: None,
        };
        match loki.fetch(req).await {
            Ok(job) => {
                served = true;
                jobs.push(job);
            }
            Err(LogSourceError::Unavailable(msg)) => jobs.push(task_log_note(
                index,
                task,
                format!("log unavailable for this job: {msg}"),
            )),
            Err(LogSourceError::Other(err)) => {
                let err = err.to_string();
                jobs.push(task_log_note(
                    index,
                    task,
                    format!("log retrieval failed for this job: {err}"),
                ));
                errors.push(err);
            }
        }
    }
    // Tasks the list never reached are unknown, not absent: the command fails
    // after printing the ones that were read.
    if let Some(why) = incomplete {
        errors.push(format!("the task list for run #{run} is incomplete: {why}"));
    }
    // Nothing was retrieved at all: that is the failure itself.
    if !served {
        if let Some(first) = errors.first() {
            return Err(anyhow!(first.clone()));
        }
    }
    Ok(RunLogs {
        source: "loki".into(),
        jobs,
        errors,
    })
}

/// A task whose log could not be shown, marked in its own lines so the printed
/// run shows where the gap is.
fn task_log_note(index: usize, task: &WorkflowTask, message: String) -> JobLogs {
    JobLogs {
        job: index,
        task_id: task.id_u64().ok(),
        job_id: None,
        name: task.display_name(index),
        status: task.display_status(),
        lines: vec![LogLine {
            timestamp: None,
            stream: None,
            message,
        }],
    }
}

fn selected_tasks(
    tasks: &[WorkflowTask],
    job: Option<usize>,
    failed_only: bool,
) -> Result<Vec<(usize, &WorkflowTask)>> {
    if let Some(index) = job {
        let task = tasks.get(index).ok_or_else(|| {
            anyhow!(
                "job index {index} is out of range: this run has {} job(s)",
                tasks.len()
            )
        })?;
        return Ok(vec![(index, task)]);
    }

    Ok(tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            !failed_only || !matches!(task.display_status().as_str(), "success" | "skipped")
        })
        .collect())
}

fn loki_query(task_id: u64) -> String {
    format!(r#"{{job="docker", container=~"/FORGEJO-ACTIONS-TASK-{task_id}-.*"}}"#)
}

fn task_window(task: &WorkflowTask) -> (String, String) {
    let end = task.stopped_at.or(task.updated_at).unwrap_or_else(Utc::now) + Duration::minutes(10);
    let start = task
        .started_at
        .or(task.created_at)
        .unwrap_or_else(|| end - Duration::hours(24))
        - Duration::minutes(10);
    (timestamp_nanos(start), timestamp_nanos(end))
}

fn timestamp_nanos(ts: DateTime<Utc>) -> String {
    ts.timestamp_nanos_opt().unwrap_or_default().to_string()
}

#[derive(Debug, Deserialize)]
struct LokiResponse {
    data: LokiData,
}

#[derive(Debug, Deserialize)]
struct LokiData {
    #[serde(default)]
    result: Vec<LokiStream>,
}

#[derive(Debug, Deserialize)]
struct LokiStream {
    #[serde(default)]
    stream: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    values: Vec<(String, String)>,
}

#[derive(Debug)]
struct LokiEntry {
    timestamp: String,
    stream: Option<String>,
    message: String,
}

impl LokiResponse {
    fn into_lines(self) -> Vec<LokiEntry> {
        let mut out = Vec::new();
        for stream in self.data.result {
            let stream_name = stream.stream.get("stream").cloned();
            for (timestamp, message) in stream.values {
                out.push(LokiEntry {
                    timestamp,
                    stream: stream_name.clone(),
                    message,
                });
            }
        }
        out
    }
}

fn clean_loki_line(line: &str) -> String {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(log) = value.get("log").and_then(Value::as_str) {
            return log.trim_end_matches(['\r', '\n']).to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_parses_window_and_total() {
        assert_eq!(parse_content_range("bytes 0-9/25"), Some((0, 9, Some(25))));
        assert_eq!(parse_content_range("bytes 10-19/*"), Some((10, 19, None)));
        assert_eq!(parse_content_range(" bytes  4-4/5 "), Some((4, 4, Some(5))));
        assert_eq!(parse_content_range("bytes 9-0/25"), None);
        assert_eq!(parse_content_range("items 0-9/25"), None);
        assert_eq!(parse_content_range("bytes */25"), None);
    }

    #[test]
    fn complete_lines_hold_back_a_partial_tail() {
        let mut pending = Vec::new();
        assert_eq!(split_complete_lines(&mut pending, b"one\ntw"), vec!["one"]);
        assert_eq!(pending, b"tw");
        assert!(split_complete_lines(&mut pending, b"o").is_empty());
        assert_eq!(
            split_complete_lines(&mut pending, b"\nthree\n"),
            vec!["two", "three"]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn run_jobs_parse_forgejo_array_and_gitea_envelope() {
        let forgejo = serde_json::json!([
            { "id": 7, "attempt": 2, "name": "build", "status": "failure", "task_id": 91 }
        ]);
        let jobs = parse_run_jobs(forgejo).unwrap();
        assert_eq!(
            (jobs[0].id, jobs[0].attempt, jobs[0].task_id),
            (7, 2, Some(91))
        );
        assert_eq!(jobs[0].status, "failure");

        let gitea = serde_json::json!({ "total_count": 2, "jobs": [
            { "id": 3, "run_attempt": 4, "name": "test", "status": "completed",
              "conclusion": "success" },
            { "id": 4, "name": "lint", "status": "in_progress", "conclusion": "" }
        ]});
        let jobs = parse_run_jobs(gitea).unwrap();
        assert_eq!((jobs[0].id, jobs[0].attempt), (3, 4));
        assert_eq!(jobs[0].status, "success");
        assert_eq!(jobs[1].status, "in_progress");

        assert!(parse_run_jobs(serde_json::json!({ "nope": [] })).is_err());
    }

    #[test]
    fn aggregate_status_prefers_the_worst_outcome() {
        assert_eq!(
            aggregate_job_status(["success", "failure"]),
            Some("failure")
        );
        assert_eq!(
            aggregate_job_status(["success", "skipped"]),
            Some("success")
        );
        assert_eq!(
            aggregate_job_status(["success", "running"]),
            Some("success")
        );
        assert_eq!(aggregate_job_status(["", "running"]), Some("running"));
        assert!(is_terminal_job_status("cancelled"));
        assert!(!is_terminal_job_status("running"));
        assert!(!is_terminal_job_status("completed"));
    }

    #[test]
    fn log_gate_withholds_on_old_servers_and_never_crosses_axes() {
        use crate::client::Flavor;

        let url = "https://h/o/r/actions/runs/5";
        assert!(log_api_gate(&ServerInfo::new(Flavor::Forgejo, "16.0.4"), url).is_ok());
        assert!(log_api_gate(&ServerInfo::new(Flavor::Gitea, "1.27.3"), url).is_ok());
        assert!(log_api_gate(&ServerInfo::unidentified(), url).is_ok());

        let err = log_api_gate(&ServerInfo::new(Flavor::Forgejo, "15.0.2"), url).unwrap_err();
        assert!(is_actions_unavailable(&err));
        let msg = err.to_string();
        assert!(msg.contains("Forgejo 15.0.2"), "{msg}");
        assert!(msg.contains(url), "{msg}");

        // Gitea 1.24 has job logs but not the jobs list that finds them.
        let err = log_api_gate(&ServerInfo::new(Flavor::Gitea, "1.24.0"), url).unwrap_err();
        assert!(err.to_string().contains("Gitea added the route in 1.25"));
    }

    #[test]
    fn tasks_are_narrowed_to_the_run_when_the_server_names_it() {
        let value = serde_json::json!({ "workflow_runs": [
            { "id": 1, "name": "a", "run_number": 7 },
            { "id": 2, "name": "b", "run_number": 8 },
            { "id": 3, "name": "c" }
        ]});
        let all = parse_workflow_tasks(value.clone()).unwrap();
        assert_eq!(all.len(), 3);
        let run = parse_workflow_tasks_for_run(value, Some(7)).unwrap();
        let ids: Vec<&str> = run.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "3"]);
    }
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn test_client() -> Client {
        Client::anonymous_for_base_url(Url::parse("http://127.0.0.1/api/v1/").unwrap()).unwrap()
    }

    #[derive(Clone)]
    struct FakeLogSource {
        available: bool,
        mode: FakeMode,
        calls: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    enum FakeMode {
        Ok(&'static str),
        /// Serves every job except the one at this index.
        OkExcept(&'static str, usize),
        Unavailable(&'static str),
        Other(&'static str),
        /// Serves every job except the one at this index, whose read fails.
        OkExceptFailing(&'static str, usize),
    }

    impl FakeLogSource {
        fn ok(source: &'static str) -> Self {
            Self {
                available: true,
                mode: FakeMode::Ok(source),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn ok_except(source: &'static str, job: usize) -> Self {
            Self {
                available: true,
                mode: FakeMode::OkExcept(source, job),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn unavailable(message: &'static str) -> Self {
            Self {
                available: true,
                mode: FakeMode::Unavailable(message),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn ok_except_failing(source: &'static str, job: usize) -> Self {
            Self {
                available: true,
                mode: FakeMode::OkExceptFailing(source, job),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn disabled() -> Self {
            Self {
                available: false,
                mode: FakeMode::Ok("disabled"),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn other(message: &'static str) -> Self {
            Self {
                available: true,
                mode: FakeMode::Other(message),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    /// Counts how many times the log path asked for task metadata.
    #[derive(Default)]
    struct CountingTasks {
        loads: Arc<AtomicUsize>,
    }

    impl CountingTasks {
        fn loads(&self) -> usize {
            self.loads.load(Ordering::SeqCst)
        }
    }

    impl TaskSource for CountingTasks {
        fn load<'a>(
            &'a self,
            _wanted: &'a [u64],
        ) -> Pin<Box<dyn Future<Output = Result<TaskList>> + Send + 'a>> {
            Box::pin(async move {
                self.loads.fetch_add(1, Ordering::SeqCst);
                parse_workflow_tasks(serde_json::json!([
                    {"id": 10, "name": "a", "status": "failure"},
                    {"id": 11, "name": "b", "status": "failure"}
                ]))
                .map(TaskList::complete)
            })
        }
    }

    impl LogSource for FakeLogSource {
        fn available(&self) -> bool {
            self.available
        }

        fn fetch<'a>(
            &'a self,
            req: LogRequest<'a>,
        ) -> Pin<Box<dyn Future<Output = LogSourceResult<JobLogs>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let FakeMode::OkExcept(_, missing) = self.mode {
                    if req.job == missing {
                        return Err(LogSourceError::Unavailable(
                            "resource does not exist".into(),
                        ));
                    }
                }
                if let FakeMode::OkExceptFailing(_, failing) = self.mode {
                    if req.job == failing {
                        return Err(LogSourceError::Other(anyhow!("loki query failed")));
                    }
                }
                match self.mode {
                    FakeMode::Ok(source)
                    | FakeMode::OkExcept(source, _)
                    | FakeMode::OkExceptFailing(source, _) => Ok(JobLogs {
                        job: req.job,
                        task_id: req.task.and_then(|task| task.id_u64().ok()),
                        job_id: req.api_job.map(|job| job.id),
                        name: source.into(),
                        status: req
                            .api_job
                            .map(|job| job.status.clone())
                            .unwrap_or_default(),
                        lines: vec![LogLine {
                            timestamp: None,
                            stream: None,
                            message: format!("{source}-{}", req.job),
                        }],
                    }),
                    FakeMode::Unavailable(message) => {
                        Err(LogSourceError::Unavailable(message.into()))
                    }
                    FakeMode::Other(message) => Err(LogSourceError::Other(anyhow!(message))),
                }
            })
        }
    }

    fn job_with_task(id: u64, name: &str, task_id: Option<u64>) -> WorkflowRunJob {
        WorkflowRunJob {
            id,
            attempt: 1,
            name: name.into(),
            status: "failure".into(),
            task_id,
        }
    }

    /// CLS-1022: a job that names its task reads THAT task's log, whatever
    /// position the task has in a list that can be unordered or longer than the
    /// run (Forgejo 16 ignores the `run` filter).
    #[tokio::test]
    async fn a_job_reads_the_task_it_names_not_the_one_at_its_position() {
        let client = test_client();
        let jobs = vec![
            job_with_task(100, "a", Some(10)),
            job_with_task(101, "b", Some(11)),
        ];
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 99, "name": "other run", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"},
            {"id": 10, "name": "a", "status": "failure"}
        ]))
        .unwrap();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::ok("loki"),
            &tasks,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs[0].task_id, Some(10));
        assert_eq!(logs.jobs[1].task_id, Some(11));
        assert!(logs.errors.is_empty());
    }

    /// CLS-1022: a task that is not in the list is not proof the job has no
    /// log. It is a retrieval error, the log that loaded is kept, and the
    /// command fails after printing.
    #[tokio::test]
    async fn a_named_task_missing_from_the_list_is_a_retrieval_error() {
        let client = test_client();
        let jobs = vec![
            job_with_task(100, "a", Some(10)),
            job_with_task(101, "b", Some(91)),
        ];
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"}
        ]))
        .unwrap();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except("forgejo", 1),
            &FakeLogSource::ok("loki"),
            &tasks,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs.len(), 2);
        assert_eq!(logs.errors.len(), 1);
        assert!(logs.errors[0].contains("task 91"), "{:?}", logs.errors);
        assert!(logs.jobs[1].lines[0]
            .message
            .contains("log retrieval failed"));
    }

    /// CLS-1022 item 2: on the older-server path one failed Loki query used to
    /// return at once, discarding the tasks that had loaded.
    #[tokio::test]
    async fn the_older_server_path_keeps_loaded_logs_when_one_query_fails() {
        let client = test_client();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"},
            {"id": 12, "name": "c", "status": "failure"}
        ]))
        .unwrap();

        let logs = fetch_run_logs_from_tasks(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except_failing("loki", 1),
            &tasks,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs.len(), 3);
        assert_eq!(logs.jobs[0].task_id, Some(10));
        assert!(logs.jobs[1].lines[0]
            .message
            .contains("log retrieval failed"));
        assert_eq!(logs.jobs[2].task_id, Some(12));
        assert_eq!(logs.errors, vec!["loki query failed".to_string()]);
    }

    /// A confirmed-missing log on that path is a note on the job, not an error.
    #[tokio::test]
    async fn the_older_server_path_reads_a_missing_log_as_unavailable() {
        let client = test_client();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"}
        ]))
        .unwrap();

        let logs = fetch_run_logs_from_tasks(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except("loki", 1),
            &tasks,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs.len(), 2);
        assert!(logs.jobs[1].lines[0].message.contains("log unavailable"));
        assert!(logs.errors.is_empty());
    }

    /// With every query failing nothing was retrieved: that is the command's error.
    #[tokio::test]
    async fn the_older_server_path_fails_when_nothing_loaded() {
        let client = test_client();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"}
        ]))
        .unwrap();

        let err = fetch_run_logs_from_tasks(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::other("loki down"),
            &tasks,
        )
        .await
        .unwrap_err();

        assert_eq!(err.to_string(), "loki down");
    }

    fn task_page(ids: &[u64], run_number: u64, total: u64) -> serde_json::Value {
        serde_json::json!({
            "total_count": total,
            "workflow_runs": ids.iter().map(|id| serde_json::json!({
                "id": id, "name": format!("t{id}"), "status": "failure", "run_number": run_number
            })).collect::<Vec<_>>()
        })
    }

    /// CLS-1022: the task list is paged until the wanted tasks turn up; a task
    /// beyond the first page is found, and later pages are not fetched.
    #[tokio::test]
    async fn the_task_list_is_paged_until_the_wanted_task_is_found() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let mut first: Vec<u64> = (1000..1050).rev().collect();
        first.truncate(TASK_PAGE_SIZE);
        // Page 1 holds other runs' tasks (run_number 5); page 2 holds ours (12).
        let page_one = task_page(&first, 5, 120);
        let page_two = task_page(&[900, 899], 12, 120);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_one))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_two))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let client = Client::anonymous_for_base_url(
            Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        )
        .unwrap();

        let list = list_run_tasks(&client, "o", "r", 12, &[900, 899])
            .await
            .unwrap();

        assert!(list.incomplete.is_none());
        let mut ids: Vec<u64> = list.tasks.iter().filter_map(|t| t.id_u64().ok()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![899, 900]);
    }

    fn mock_tasks_client(server: &wiremock::MockServer) -> Client {
        Client::anonymous_for_base_url(Url::parse(&format!("{}/api/v1/", server.uri())).unwrap())
            .unwrap()
    }

    /// CLS-1022 (review): a run's tasks need not be contiguous. With no wanted
    /// ids the walk goes to the end of the list, so a task of the run that sits
    /// behind an unrelated page is still found.
    #[tokio::test]
    async fn a_runs_tasks_separated_by_an_unrelated_page_are_all_found() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let pages = [
            task_page(&[300], 12, 3),
            task_page(&[250], 7, 3),
            task_page(&[100], 12, 3),
        ];
        for (n, page) in pages.into_iter().enumerate() {
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/r/actions/tasks"))
                .and(query_param("page", (n + 1).to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(page))
                .mount(&server)
                .await;
        }
        let client = mock_tasks_client(&server);

        let list = list_run_tasks(&client, "o", "r", 12, &[]).await.unwrap();

        let mut ids: Vec<u64> = list.tasks.iter().filter_map(|t| t.id_u64().ok()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![100, 300]);
        assert!(list.incomplete.is_none());
    }

    /// CLS-1022 (review): a server that clamps the page size and sends no count
    /// still has the decisive task on page 2; a short page is not the end.
    #[tokio::test]
    async fn a_clamped_first_page_does_not_end_the_walk() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let no_count = |ids: &[u64], run: u64| {
            let mut page = task_page(ids, run, 0);
            page.as_object_mut().unwrap().remove("total_count");
            page
        };
        let pages = [
            no_count(&[900, 899, 898], 5),
            no_count(&[500], 12),
            no_count(&[], 12),
        ];
        for (n, page) in pages.into_iter().enumerate() {
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/r/actions/tasks"))
                .and(query_param("page", (n + 1).to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(page))
                .mount(&server)
                .await;
        }
        let client = mock_tasks_client(&server);

        let list = list_run_tasks(&client, "o", "r", 12, &[500]).await.unwrap();

        assert_eq!(list.tasks.len(), 1);
        assert_eq!(list.tasks[0].id, "500");
        assert!(list.incomplete.is_none());
    }

    /// CLS-1022 (review): page 2 failing after page 1 resolved task A keeps A.
    /// Through the production adapter and the fallback, A's log is read and only
    /// B, whose task was never reached, is marked failed.
    #[tokio::test]
    async fn a_later_page_failing_keeps_the_tasks_already_resolved() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(task_page(&[20], 12, 3)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = mock_tasks_client(&server);
        let remote = RemoteTasks {
            client: &client,
            owner: "o",
            name: "r",
            run: 12,
        };
        let jobs = vec![
            job_with_task(100, "a", Some(20)),
            job_with_task(101, "b", Some(10)),
        ];

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::ok("loki"),
            &remote,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs[0].task_id, Some(20));
        assert_eq!(logs.jobs[0].name, "loki");
        assert!(logs.jobs[1].lines[0]
            .message
            .contains("log retrieval failed"));
        assert_eq!(logs.errors.len(), 1);
        assert!(logs.errors[0].contains("task 10"), "{:?}", logs.errors);
        assert!(logs.errors[0].contains("incomplete"), "{:?}", logs.errors);
    }

    /// A first page that fails is the failure itself, not an empty list.
    #[tokio::test]
    async fn a_failing_first_page_is_an_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = mock_tasks_client(&server);

        assert!(list_run_tasks(&client, "o", "r", 12, &[1]).await.is_err());
    }

    /// A server that ignores paging sends the whole list every time; the walk
    /// stops at the first repeat instead of looping.
    #[tokio::test]
    async fn a_server_that_ignores_paging_is_read_once() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                (100..100 + TASK_PAGE_SIZE as u64)
                    .map(|id| {
                        serde_json::json!({"id": id, "name": "a", "status": "failure", "run_number": 12})
                    })
                    .collect::<Vec<_>>(),
            ))
            .expect(2)
            .mount(&server)
            .await;
        let client = Client::anonymous_for_base_url(
            Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        )
        .unwrap();

        // A wanted task that is not there (id 5, below the page): page 1 is
        // full and adds fifty tasks, page 2 repeats them, and the walk ends.
        let list = list_run_tasks(&client, "o", "r", 12, &[5]).await.unwrap();
        assert_eq!(list.tasks.len(), TASK_PAGE_SIZE);
        assert!(list.incomplete.is_none());
    }

    /// A list cut off before the wanted tasks is marked incomplete, not a shorter answer.
    #[tokio::test]
    async fn a_task_list_that_never_reaches_the_task_is_incomplete() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

        struct EndlessPages(Arc<AtomicUsize>);
        impl Respond for EndlessPages {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let n = self.0.fetch_add(1, Ordering::SeqCst) as u64;
                let ids: Vec<u64> = (0..TASK_PAGE_SIZE as u64)
                    .map(|i| 1_000_000 - n * 100 - i)
                    .collect();
                ResponseTemplate::new(200).set_body_json(task_page(&ids, 5, 1_000_000))
            }
        }

        let server = MockServer::start().await;
        let pages = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/actions/tasks"))
            .respond_with(EndlessPages(pages.clone()))
            .mount(&server)
            .await;
        let client = Client::anonymous_for_base_url(
            Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        )
        .unwrap();

        // Newest first and never reaching id 1: the walk gives up at the cap
        // and says the list is incomplete instead of answering short.
        let list = list_run_tasks(&client, "o", "r", 12, &[1]).await.unwrap();

        assert!(list.incomplete.is_some(), "{list:?}");
        assert_eq!(pages.load(Ordering::SeqCst), TASK_PAGE_LIMIT as usize);
    }

    #[test]
    fn parses_array_task_response() {
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 123, "name": "build", "status": "failure"}
        ]))
        .unwrap();
        assert_eq!(tasks[0].id_u64().unwrap(), 123);
        assert_eq!(tasks[0].display_name(0), "build");
    }

    #[test]
    fn parses_task_response_envelope_variants_and_fallback_fields() {
        for key in ["workflow_tasks", "jobs", "workflow_runs"] {
            let tasks = parse_workflow_tasks(serde_json::json!({
                key: [{
                    "id": 42,
                    "display_title": "fallback title",
                    "conclusion": "cancelled",
                    "created": "2026-08-07T05:00:00Z",
                    "updated": "2026-08-07T05:05:00Z"
                }]
            }))
            .unwrap();
            assert_eq!(tasks[0].id, "42");
            assert_eq!(tasks[0].display_name(0), "fallback title");
            assert_eq!(tasks[0].display_status(), "cancelled");
            assert!(tasks[0].created_at.is_some());
            assert!(tasks[0].updated_at.is_some());
        }
    }

    #[test]
    fn parse_task_response_rejects_unknown_shape_and_bad_rows() {
        let err = parse_workflow_tasks(serde_json::json!({"unexpected": []})).unwrap_err();
        assert!(err.to_string().contains("unexpected workflow task list"));

        let err = parse_workflow_tasks(serde_json::json!([{"id": []}])).unwrap_err();
        assert!(err.to_string().contains("decoding workflow task"));
    }

    #[tokio::test]
    async fn remote_tasks_are_not_loaded_when_forgejo_serves_logs() {
        // The task list is the expensive call, so the native API path has to
        // finish without ever asking for it. `test_client` points at a dead
        // address: a load attempt would error, not hang on a real request.
        let client = test_client();
        let jobs = vec![WorkflowRunJob {
            id: 100,
            attempt: 1,
            name: "a".into(),
            status: "failure".into(),
            task_id: Some(77),
        }];
        let counting = CountingTasks::default();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok("forgejo"),
            &FakeLogSource::ok("loki"),
            &counting,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "forgejo-api");
        assert_eq!(counting.loads(), 0);
    }

    #[tokio::test]
    async fn remote_tasks_are_loaded_once_for_the_loki_fallback() {
        let client = test_client();
        let jobs = vec![
            WorkflowRunJob {
                id: 100,
                attempt: 1,
                name: "a".into(),
                status: "failure".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 101,
                attempt: 1,
                name: "b".into(),
                status: "failure".into(),
                task_id: None,
            },
        ];
        let counting = CountingTasks::default();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::ok("loki"),
            &counting,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "loki");
        assert_eq!(logs.jobs.len(), 2);
        assert_eq!(counting.loads(), 1);
    }

    #[test]
    fn run_logs_status_prefers_failure_then_cancelled_then_timeout() {
        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("failure"), job("cancelled")],
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("failure"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("cancelled"), job("timed_out")],
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("cancelled"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("timed_out")],
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("timed_out"));
    }

    #[test]
    fn run_logs_status_handles_all_success_and_unknown() {
        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("skipped")],
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("success"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("queued"), job("")],
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("queued"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: Vec::new(),
            errors: Vec::new(),
        };
        assert_eq!(logs.status(), Some("success"));
    }

    #[test]
    fn selected_tasks_filters_green_jobs_for_failed_only() {
        let tasks = parse_workflow_tasks(serde_json::json!({"tasks": [
            {"id": 1, "name": "a", "status": "success"},
            {"id": 2, "name": "b", "status": "failure"},
            {"id": 3, "name": "c", "status": "skipped"}
        ]}))
        .unwrap();
        let selected = selected_tasks(&tasks, None, true).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, 1);
        assert_eq!(selected[0].1.id_u64().unwrap(), 2);
    }

    #[test]
    fn selected_tasks_reports_index_errors_and_keeps_all_when_not_failed_only() {
        let tasks = parse_workflow_tasks(serde_json::json!({"tasks": [
            {"id": 1, "name": "a", "status": "success"},
            {"id": 2, "name": "b", "status": "failure"}
        ]}))
        .unwrap();
        let selected = selected_tasks(&tasks, None, false).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected_tasks(&tasks, Some(1), true).unwrap()[0].1.id, "2");

        let err = selected_tasks(&tasks, Some(9), false).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn selected_jobs_filters_by_failure_and_reports_index_errors() {
        let jobs = vec![
            WorkflowRunJob {
                id: 1,
                attempt: 0,
                name: "build".into(),
                status: "success".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 2,
                attempt: 3,
                name: String::new(),
                status: "failure".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 3,
                attempt: 0,
                name: "lint".into(),
                status: "skipped".into(),
                task_id: None,
            },
        ];

        let selected = selected_jobs(&jobs, None, true).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, 1);
        assert_eq!(selected[0].1.display_name(1), "job 1");
        assert_eq!(selected_jobs(&jobs, Some(0), true).unwrap()[0].1.id, 1);

        let err = selected_jobs(&jobs, Some(7), false).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn task_window_uses_recorded_bounds_with_padding() {
        let task = parse_workflow_tasks(serde_json::json!([{
            "id": 1,
            "started": "2026-08-07T05:00:00Z",
            "stopped": "2026-08-07T05:05:00Z"
        }]))
        .unwrap()
        .remove(0);

        let (start, end) = task_window(&task);
        assert_eq!(start, "1786078200000000000");
        assert_eq!(end, "1786079700000000000");
    }

    #[test]
    fn loki_response_flattens_streams_and_cleans_lines() {
        let response: LokiResponse = serde_json::from_value(serde_json::json!({
            "data": {
                "result": [
                    {
                        "stream": {"stream": "stderr"},
                        "values": [["10", "{\"log\":\"err\\n\"}"]]
                    },
                    {
                        "stream": {},
                        "values": [["11", "plain\r\n"]]
                    }
                ]
            }
        }))
        .unwrap();
        let lines = response.into_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp, "10");
        assert_eq!(lines[0].stream.as_deref(), Some("stderr"));
        assert_eq!(clean_loki_line(&lines[0].message), "err");
        assert_eq!(lines[1].stream, None);
        assert_eq!(clean_loki_line(&lines[1].message), "plain");
    }

    #[test]
    fn response_suffix_formats_only_non_empty_bodies() {
        assert_eq!(response_suffix(" \n\t"), "");
        assert_eq!(response_suffix(" no such job \n"), ": no such job");
    }

    #[tokio::test]
    async fn fetch_sources_uses_forgejo_when_all_selected_jobs_have_logs() {
        let client = test_client();
        let forgejo = FakeLogSource::ok("forgejo");
        let loki = FakeLogSource::ok("loki");
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "success"},
            {"id": 11, "name": "b", "status": "failure"}
        ]))
        .unwrap();
        let jobs = vec![
            WorkflowRunJob {
                id: 100,
                attempt: 1,
                name: "a".into(),
                status: "success".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 101,
                attempt: 1,
                name: "b".into(),
                status: "failure".into(),
                task_id: None,
            },
        ];

        let logs = fetch_run_logs_with_sources(
            &client, "o", "r", 12, None, None, false, &forgejo, &loki, &tasks, &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "forgejo-api");
        assert_eq!(logs.jobs.len(), 2);
        assert_eq!(forgejo.calls(), 2);
        assert_eq!(loki.calls(), 0);
    }

    #[tokio::test]
    async fn fetch_sources_falls_back_to_loki_after_forgejo_unavailable() {
        let client = test_client();
        let forgejo = FakeLogSource::unavailable("no native logs");
        let loki = FakeLogSource::ok("loki");
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "success"},
            {"id": 11, "name": "b", "status": "failure"},
            {"id": 12, "name": "c", "status": "skipped"}
        ]))
        .unwrap();
        let jobs = vec![
            WorkflowRunJob {
                id: 100,
                attempt: 1,
                name: "a".into(),
                status: "success".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 101,
                attempt: 1,
                name: "b".into(),
                status: "failure".into(),
                task_id: None,
            },
            WorkflowRunJob {
                id: 102,
                attempt: 1,
                name: "c".into(),
                status: "skipped".into(),
                task_id: None,
            },
        ];

        let logs = fetch_run_logs_with_sources(
            &client, "o", "r", 12, None, None, true, &forgejo, &loki, &tasks, &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "loki");
        assert_eq!(logs.jobs.len(), 1);
        assert_eq!(logs.jobs[0].job, 1);
        assert_eq!(logs.jobs[0].task_id, Some(11));
        assert_eq!(logs.jobs[0].job_id, Some(101));
        assert_eq!(forgejo.calls(), 1);
        assert_eq!(loki.calls(), 1);
    }

    #[tokio::test]
    async fn fetch_sources_returns_none_for_empty_selection() {
        let client = test_client();
        let forgejo = FakeLogSource::ok("forgejo");
        let loki = FakeLogSource::ok("loki");
        let jobs = vec![WorkflowRunJob {
            id: 100,
            attempt: 1,
            name: "a".into(),
            status: "success".into(),
            task_id: None,
        }];

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            true,
            &forgejo,
            &loki,
            &Vec::new(),
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "none");
        assert!(logs.jobs.is_empty());
        assert_eq!(forgejo.calls(), 0);
        assert_eq!(loki.calls(), 0);
    }

    #[tokio::test]
    async fn fetch_sources_reports_missing_loki_and_other_errors() {
        let client = test_client();
        let jobs = vec![WorkflowRunJob {
            id: 100,
            attempt: 1,
            name: "a".into(),
            status: "failure".into(),
            task_id: None,
        }];
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"}
        ]))
        .unwrap();

        let err = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::disabled(),
            &tasks,
            &jobs,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no workflow log source"));

        let err = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::other("forgejo broke"),
            &FakeLogSource::ok("loki"),
            &tasks,
            &jobs,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "forgejo broke");
    }

    fn two_failed_jobs() -> Vec<WorkflowRunJob> {
        ["a", "b"]
            .iter()
            .zip([100, 101])
            .map(|(name, id)| WorkflowRunJob {
                id,
                attempt: 1,
                name: (*name).into(),
                status: "failure".into(),
                task_id: None,
            })
            .collect()
    }

    /// CLS-963: job 1 has no log (no task yet) while job 0 loaded. The run
    /// keeps job 0's log and says plainly that job 1 has none.
    #[tokio::test]
    async fn one_missing_job_log_does_not_discard_the_others() {
        let client = test_client();
        let jobs = two_failed_jobs();
        let loki = FakeLogSource::ok("loki");

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except("forgejo", 1),
            &loki,
            &Vec::new(),
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "forgejo-api");
        assert_eq!(logs.jobs.len(), 2);
        assert_eq!(logs.jobs[0].lines[0].message, "forgejo-0");
        assert!(logs.jobs[1].lines[0].message.contains("log unavailable"));
        assert!(logs.jobs[1].lines[0]
            .message
            .contains("resource does not exist"));
        assert_eq!(logs.jobs[1].status, "failure");
        assert_eq!(loki.calls(), 0);
    }

    #[tokio::test]
    async fn a_missing_job_log_falls_back_to_loki_for_that_job_only() {
        let client = test_client();
        let jobs = two_failed_jobs();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"}
        ]))
        .unwrap();
        let forgejo = FakeLogSource::ok_except("forgejo", 1);
        let loki = FakeLogSource::ok("loki");

        let logs = fetch_run_logs_with_sources(
            &client, "o", "r", 12, None, None, false, &forgejo, &loki, &tasks, &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "forgejo-api+loki");
        assert_eq!(logs.jobs[0].lines[0].message, "forgejo-0");
        assert_eq!(logs.jobs[1].lines[0].message, "loki-1");
        assert_eq!(loki.calls(), 1);
    }

    /// With nothing served anywhere the command still answers, job by job,
    /// instead of failing on missing task metadata.
    #[tokio::test]
    async fn no_task_metadata_reports_each_job_as_unavailable() {
        let client = test_client();
        let jobs = two_failed_jobs();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::ok("loki"),
            &Vec::new(),
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "none");
        assert_eq!(logs.jobs.len(), 2);
        assert!(logs
            .jobs
            .iter()
            .all(|job| job.lines[0].message.contains("log unavailable")));
    }

    /// A transport failure is not "no log": it still fails the command.
    #[tokio::test]
    async fn a_failing_loki_query_still_fails_the_run() {
        let client = test_client();
        let jobs = two_failed_jobs();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"}
        ]))
        .unwrap();

        let err = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::other("loki down"),
            &tasks,
            &jobs,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "loki down");
    }

    struct FailingTasks;

    impl TaskSource for FailingTasks {
        fn load<'a>(
            &'a self,
            _wanted: &'a [u64],
        ) -> Pin<Box<dyn Future<Output = Result<TaskList>> + Send + 'a>> {
            Box::pin(async move { Err(anyhow!("task list failed (500)")) })
        }
    }

    /// A native-log 404 followed by a failed task-list read is a failed
    /// retrieval, not a job with no log. The job that loaded is kept and the
    /// failure is recorded so the command fails after printing it.
    #[tokio::test]
    async fn a_failed_task_list_read_keeps_loaded_logs_and_records_the_failure() {
        let client = test_client();
        let jobs = two_failed_jobs();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except("forgejo", 1),
            &FakeLogSource::ok("loki"),
            &FailingTasks,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.jobs[0].lines[0].message, "forgejo-0");
        assert!(logs.jobs[1].lines[0]
            .message
            .contains("log retrieval failed"));
        assert_eq!(logs.errors, vec!["task list failed (500)".to_string()]);
    }

    #[tokio::test]
    async fn a_failing_loki_query_keeps_the_logs_that_loaded() {
        let client = test_client();
        let jobs = two_failed_jobs();
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 10, "name": "a", "status": "failure"},
            {"id": 11, "name": "b", "status": "failure"}
        ]))
        .unwrap();

        let logs = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::ok_except("forgejo", 1),
            &FakeLogSource::other("loki down"),
            &tasks,
            &jobs,
        )
        .await
        .unwrap();

        assert_eq!(logs.source, "forgejo-api");
        assert_eq!(logs.jobs[0].lines[0].message, "forgejo-0");
        assert_eq!(logs.errors, vec!["loki down".to_string()]);
    }

    /// With nothing retrieved at all the failure is the command's error.
    #[tokio::test]
    async fn a_failed_task_list_read_with_no_logs_at_all_fails_the_run() {
        let client = test_client();
        let jobs = two_failed_jobs();

        let err = fetch_run_logs_with_sources(
            &client,
            "o",
            "r",
            12,
            None,
            None,
            false,
            &FakeLogSource::unavailable("no native logs"),
            &FakeLogSource::ok("loki"),
            &FailingTasks,
            &jobs,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("task list failed"));
    }

    #[test]
    fn cleans_docker_json_log_lines() {
        assert_eq!(clean_loki_line(r#"{"log":"hello\n"}"#), "hello");
        assert_eq!(clean_loki_line("plain\n"), "plain");
    }

    fn job(status: &str) -> JobLogs {
        JobLogs {
            job: 0,
            task_id: None,
            job_id: None,
            name: "job".into(),
            status: status.into(),
            lines: Vec::new(),
        }
    }
}
