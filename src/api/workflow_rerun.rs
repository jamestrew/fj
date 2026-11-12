//! Token-authenticated reruns, with Forgejo and Gitea route differences.
use anyhow::{anyhow, Result};
use reqwest::{Method, StatusCode};

use crate::api::workflow_logs::{is_terminal_job_status, list_run_jobs_api, resolve_run_api_id};
use crate::api::workflow_run::{actions_limitation, run_action_error, run_web_url};
use crate::client::capability::unsupported_reason;
use crate::client::{Capability, Client, Flavor, Support};

/// What `fj run rerun` should restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunTarget {
    /// Every job in the run.
    Run,
    /// Failed or cancelled jobs (and their server-selected dependents).
    Failed,
    /// One job by its zero-based row index in `fj run view`.
    Job(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunOutcome {
    Started,
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
    let run_id = resolve_run_api_id(client, owner, name, run).await?;
    let repo = crate::api::repo_path(owner, name);
    let forgejo = info.flavor == Flavor::Forgejo;
    if forgejo && target == RerunTarget::Failed {
        return rerun_failed(client, owner, name, run, run_id, &run_url).await;
    }
    let path = match target {
        RerunTarget::Run => format!("{repo}/actions/runs/{run_id}/rerun"),
        RerunTarget::Failed => format!("{repo}/actions/runs/{run_id}/rerun-failed-jobs"),
        RerunTarget::Job(index) => {
            let jobs = list_run_jobs_api(client, owner, name, run, run_id).await?;
            let job = jobs.get(index).ok_or_else(|| {
                anyhow!(
                    "job index {index} is out of range: this run has {} job(s)",
                    jobs.len()
                )
            })?;
            if forgejo {
                format!("{repo}/actions/jobs/{}/rerun", job.id)
            } else {
                format!("{repo}/actions/runs/{run_id}/jobs/{}/rerun", job.id)
            }
        }
    };
    post_rerun(client, &path, run, target == RerunTarget::Failed, &run_url).await
}

async fn rerun_failed(
    client: &Client,
    owner: &str,
    name: &str,
    run: u64,
    run_id: u64,
    run_url: &str,
) -> Result<RerunOutcome> {
    let jobs = list_run_jobs_api(client, owner, name, run, run_id).await?;
    let selected: Vec<_> = jobs
        .iter()
        .filter(|job| matches!(job.status.as_str(), "failure" | "cancelled"))
        .map(|job| (job.id, job.attempt))
        .collect();
    if selected.is_empty() {
        return Ok(RerunOutcome::NothingFailed);
    }
    let repo = crate::api::repo_path(owner, name);
    if selected.len() == jobs.len() {
        return post_rerun(
            client,
            &format!("{repo}/actions/runs/{run_id}/rerun"),
            run,
            false,
            run_url,
        )
        .await;
    }
    let mut errors = Vec::new();
    let mut started = 0;
    for (id, attempt) in selected {
        // A prior job rerun may already have rescheduled this dependent.
        let current = list_run_jobs_api(client, owner, name, run, run_id)
            .await
            .map_err(|error| {
                anyhow!(
                    "{started} job rerun(s) started; could not refresh jobs: {error}; {}",
                    errors.join("; ")
                )
            })?;
        let job = current.iter().find(|job| job.id == id).ok_or_else(|| {
            anyhow!("selected job {id} disappeared; {started} rerun(s) already started")
        })?;
        // Still running, or already rerun and finished again: either way the
        // attempt we selected is gone, and another POST would repeat the work.
        if !is_terminal_job_status(&job.status) || job.attempt != attempt {
            continue;
        }
        let path = format!("{repo}/actions/jobs/{id}/rerun");
        match post_rerun(client, &path, run, false, run_url).await {
            Ok(_) => started += 1,
            Err(error) => errors.push(format!("job {id}: {error}")),
        }
    }
    if !errors.is_empty() {
        return Err(anyhow!(
            "{started} job rerun(s) started; {}",
            errors.join("; ")
        ));
    }
    Ok(RerunOutcome::Started)
}

async fn post_rerun(
    client: &Client,
    path: &str,
    run: u64,
    failed: bool,
    run_url: &str,
) -> Result<RerunOutcome> {
    let res = client.request(Method::POST, path, &[], None).await?;
    let status = res.status();
    if status.is_success() {
        return Ok(RerunOutcome::Started);
    }
    let body = res.text().await.unwrap_or_default();
    if failed
        && status == StatusCode::BAD_REQUEST
        && body.to_ascii_lowercase().contains("no failed jobs")
    {
        return Ok(RerunOutcome::NothingFailed);
    }
    Err(run_action_error("rerun", run, status, &body, run_url))
}

#[cfg(test)]
#[path = "workflow_rerun_tests.rs"]
mod tests;
