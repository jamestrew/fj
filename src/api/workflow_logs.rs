//! Workflow run logs through the Forgejo API with Loki fallback.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::client::Client;

const DEFAULT_LOKI_URL: &str = "http://100.105.235.123:3100";
const LOKI_LIMIT: &str = "5000";

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunLogs {
    pub source: String,
    pub jobs: Vec<JobLogs>,
}

impl RunLogs {
    pub fn status(&self) -> Option<&str> {
        if self.jobs.iter().any(|j| j.status == "failure") {
            return Some("failure");
        }
        if self.jobs.iter().any(|j| j.status == "cancelled") {
            return Some("cancelled");
        }
        if self.jobs.iter().any(|j| j.status == "timed_out") {
            return Some("timed_out");
        }
        if self
            .jobs
            .iter()
            .all(|j| matches!(j.status.as_str(), "success" | "skipped"))
        {
            return Some("success");
        }
        self.jobs
            .iter()
            .find_map(|j| (!j.status.is_empty()).then_some(j.status.as_str()))
    }
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
    fn display_name(&self, index: usize) -> String {
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
            let path = format!(
                "{}/actions/jobs/{}/logs",
                crate::api::repo_path(req.owner, req.name),
                api_job.id
            );
            let query = req
                .attempt
                .or_else(|| (api_job.attempt > 0).then_some(api_job.attempt))
                .map(|attempt| vec![("attempt".to_string(), attempt.to_string())])
                .unwrap_or_default();
            let res = req.client.request(Method::GET, &path, &query, None).await?;
            if res.status() == StatusCode::NOT_FOUND {
                return Err(LogSourceError::Unavailable(
                    "Forgejo native workflow logs endpoint is unavailable".into(),
                ));
            }
            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(LogSourceError::Other(anyhow!(
                    "Forgejo workflow log fetch failed ({status}): {}",
                    body.trim()
                )));
            }
            let text = res
                .text()
                .await
                .context("reading Forgejo workflow log response")?;
            let lines = text
                .lines()
                .map(|message| LogLine {
                    timestamp: None,
                    stream: None,
                    message: message.to_string(),
                })
                .collect();
            Ok(JobLogs {
                job: req.job,
                task_id: api_job
                    .task_id
                    .or_else(|| req.task.and_then(|task| task.id_u64().ok())),
                job_id: Some(api_job.id),
                name: req
                    .api_job
                    .map(|job| job.display_name(req.job))
                    .unwrap_or_else(|| format!("job {}", req.job)),
                status: api_job.status.clone(),
                lines,
            })
        })
    }
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

pub async fn fetch_run_logs(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
) -> Result<RunLogs> {
    let run_id = resolve_run_api_id(client, owner, name, run).await?;
    let api_jobs = list_run_jobs_api(client, owner, name, run, run_id).await?;
    let tasks = RemoteTasks {
        client,
        owner,
        name,
        run,
    };
    fetch_run_logs_with_sources(
        client,
        owner,
        name,
        run,
        job,
        attempt,
        failed_only,
        &ForgejoApiLogSource,
        &LokiLogSource::from_env()?,
        &tasks,
        &api_jobs,
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
    let run_id = resolve_run_api_id(client, owner, name, run).await?;
    let api_jobs = list_run_jobs_api(client, owner, name, run, run_id).await?;
    let tasks = RemoteTasks {
        client,
        owner,
        name,
        run,
    };
    fetch_run_logs_with_sources(
        client,
        owner,
        name,
        run,
        job,
        attempt,
        failed_only,
        &ForgejoApiLogSource,
        &LokiLogSource::new(loki_base)?,
        &tasks,
        &api_jobs,
    )
    .await
}

async fn resolve_run_api_id(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
) -> Result<u64> {
    let runs = crate::api::workflow_run::list_runs_filtered(
        client,
        owner,
        name,
        50,
        true,
        &crate::api::workflow_run::RunListFilters::default(),
    )
    .await?;
    runs.iter()
        .find(|run| run.run_number == run_number)
        .map(|run| run.id)
        .ok_or_else(|| anyhow!("workflow run #{run_number} was not found; check `fj run list`"))
}

pub async fn list_run_jobs_api(
    client: &Client,
    owner: &str,
    name: &str,
    run_number: u64,
    run_id: u64,
) -> Result<Vec<WorkflowRunJob>> {
    let path = format!(
        "{}/actions/runs/{run_id}/jobs",
        crate::api::repo_path(owner, name)
    );
    let res = client.request(Method::GET, &path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return res.json().await.context("decoding workflow run jobs JSON");
    }
    let body = res.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        let version = forgejo_version(client)
            .await
            .unwrap_or_else(|_| "unknown".into());
        return Err(anyhow!(
            "cannot read logs for run #{run_number}: this Forgejo server reports version `{version}` \
             and does not expose the token-authenticated Actions job log API. Upgrade the \
             server to Forgejo stable with `/api/v1/repos/.../actions/runs/{{run_id}}/jobs` and \
             `/api/v1/repos/.../actions/jobs/{{job_id}}/logs` support, or open the run in the \
             Forgejo web UI. See rasterstate/fj#103."
        ));
    }
    Err(anyhow!(
        "could not list workflow run jobs (HTTP {}){}",
        status.as_u16(),
        response_suffix(&body)
    ))
}

async fn forgejo_version(client: &Client) -> Result<String> {
    #[derive(Deserialize)]
    struct Version {
        version: String,
    }

    let version: Version = client
        .json(Method::GET, "/api/v1/version", &[], None::<&()>)
        .await
        .context("probing Forgejo version")?;
    Ok(version.version)
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
pub(crate) trait TaskSource: Send + Sync {
    fn load(&self) -> Pin<Box<dyn Future<Output = Vec<WorkflowTask>> + Send + '_>>;
}

/// Tasks already in hand, which is how tests and callers with a cached list
/// supply them.
impl TaskSource for Vec<WorkflowTask> {
    fn load(&self) -> Pin<Box<dyn Future<Output = Vec<WorkflowTask>> + Send + '_>> {
        Box::pin(async move { self.clone() })
    }
}

/// Tasks fetched from the server on first ask. A failure degrades to an empty
/// list, which the Loki fallback reports as missing task metadata.
pub(crate) struct RemoteTasks<'a> {
    pub client: &'a Client,
    pub owner: &'a str,
    pub name: &'a str,
    pub run: u64,
}

impl TaskSource for RemoteTasks<'_> {
    fn load(&self) -> Pin<Box<dyn Future<Output = Vec<WorkflowTask>> + Send + '_>> {
        Box::pin(async move {
            list_run_tasks(self.client, self.owner, self.name, self.run)
                .await
                .unwrap_or_default()
        })
    }
}

pub async fn list_run_tasks(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
) -> Result<Vec<WorkflowTask>> {
    let path = format!("{}/actions/tasks", crate::api::repo_path(owner, name));
    let query = vec![("run".to_string(), run.to_string())];
    let value: Value = client
        .json(
            Method::GET,
            &path,
            &query,
            Option::<&serde_json::Value>::None,
        )
        .await?;
    parse_workflow_tasks(value)
}

pub fn parse_workflow_tasks(value: Value) -> Result<Vec<WorkflowTask>> {
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
        });
    }

    if forgejo.available() {
        let mut jobs = Vec::with_capacity(selected.len());
        let mut unavailable = false;
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
            match forgejo.fetch(req).await {
                Ok(job) => jobs.push(job),
                Err(LogSourceError::Unavailable(_)) => {
                    unavailable = true;
                    break;
                }
                Err(LogSourceError::Other(err)) => return Err(err),
            }
        }
        if !unavailable {
            return Ok(RunLogs {
                source: "forgejo-api".into(),
                jobs,
            });
        }
    }

    if !loki.available() {
        return Err(anyhow!("no workflow log source is available"));
    }

    let tasks = tasks.load().await;
    let mut jobs = Vec::with_capacity(selected.len());
    for &(index, api_job) in &selected {
        let task = tasks.get(index).ok_or_else(|| {
            anyhow!("Loki fallback needs workflow task metadata for job index {index}")
        })?;
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
            Ok(job) => jobs.push(job),
            Err(LogSourceError::Unavailable(msg)) => return Err(anyhow!(msg)),
            Err(LogSourceError::Other(err)) => return Err(err),
        }
    }

    Ok(RunLogs {
        source: "loki".into(),
        jobs,
    })
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

#[allow(dead_code)]
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
        Unavailable(&'static str),
        Other(&'static str),
    }

    impl FakeLogSource {
        fn ok(source: &'static str) -> Self {
            Self {
                available: true,
                mode: FakeMode::Ok(source),
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
        fn load(&self) -> Pin<Box<dyn Future<Output = Vec<WorkflowTask>> + Send + '_>> {
            Box::pin(async move {
                self.loads.fetch_add(1, Ordering::SeqCst);
                parse_workflow_tasks(serde_json::json!([
                    {"id": 10, "name": "a", "status": "failure"},
                    {"id": 11, "name": "b", "status": "failure"}
                ]))
                .unwrap()
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
                match self.mode {
                    FakeMode::Ok(source) => Ok(JobLogs {
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
        };
        assert_eq!(logs.status(), Some("failure"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("cancelled"), job("timed_out")],
        };
        assert_eq!(logs.status(), Some("cancelled"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("timed_out")],
        };
        assert_eq!(logs.status(), Some("timed_out"));
    }

    #[test]
    fn run_logs_status_handles_all_success_and_unknown() {
        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("success"), job("skipped")],
        };
        assert_eq!(logs.status(), Some("success"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: vec![job("queued"), job("")],
        };
        assert_eq!(logs.status(), Some("queued"));

        let logs = RunLogs {
            source: "test".into(),
            jobs: Vec::new(),
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

    #[tokio::test]
    async fn fetch_sources_requires_tasks_for_loki_fallback() {
        let client = test_client();
        let jobs = vec![WorkflowRunJob {
            id: 100,
            attempt: 1,
            name: "a".into(),
            status: "failure".into(),
            task_id: None,
        }];

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
            &Vec::new(),
            &jobs,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("workflow task metadata"));
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
