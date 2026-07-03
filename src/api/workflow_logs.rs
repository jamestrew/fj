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
    pub run: u64,
    pub job: usize,
    pub attempt: Option<u32>,
    pub task: Option<&'a WorkflowTask>,
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
            let path = format!(
                "{}/actions/runs/{}/jobs/{}/logs",
                crate::api::repo_path(req.owner, req.name),
                req.run,
                req.job
            );
            let query = req
                .attempt
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
                task_id: req.task.and_then(|task| task.id_u64().ok()),
                name: req
                    .task
                    .map(|task| task.display_name(req.job))
                    .unwrap_or_else(|| format!("job {}", req.job)),
                status: req
                    .task
                    .map(WorkflowTask::display_status)
                    .unwrap_or_default(),
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
    let tasks = list_run_tasks(client, owner, name, run).await?;
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
    let tasks = list_run_tasks(client, owner, name, run).await?;
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
    )
    .await
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
    run: u64,
    job: Option<usize>,
    attempt: Option<u32>,
    failed_only: bool,
    forgejo: &impl LogSource,
    loki: &impl LogSource,
    tasks: &[WorkflowTask],
) -> Result<RunLogs> {
    let selected = selected_tasks(tasks, job, failed_only)?;
    if selected.is_empty() {
        return Ok(RunLogs {
            source: "none".into(),
            jobs: Vec::new(),
        });
    }

    if forgejo.available() {
        let mut jobs = Vec::with_capacity(selected.len());
        let mut unavailable = false;
        for &(index, task) in &selected {
            let req = LogRequest {
                client,
                owner,
                name,
                run,
                job: index,
                attempt,
                task: Some(task),
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

    let mut jobs = Vec::with_capacity(selected.len());
    for &(index, task) in &selected {
        let req = LogRequest {
            client,
            owner,
            name,
            run,
            job: index,
            attempt,
            task: Some(task),
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
    fn parses_array_task_response() {
        let tasks = parse_workflow_tasks(serde_json::json!([
            {"id": 123, "name": "build", "status": "failure"}
        ]))
        .unwrap();
        assert_eq!(tasks[0].id_u64().unwrap(), 123);
        assert_eq!(tasks[0].display_name(0), "build");
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
    fn cleans_docker_json_log_lines() {
        assert_eq!(clean_loki_line(r#"{"log":"hello\n"}"#), "hello");
        assert_eq!(clean_loki_line("plain\n"), "plain");
    }
}
