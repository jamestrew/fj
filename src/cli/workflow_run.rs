//! `fj run` — Forgejo Actions run management (list/view/watch/download/rerun/cancel).

use std::io::Write;
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::workflow_artifact::{self, DownloadArgs};
use crate::client::AuthScheme;
use crate::output;

#[derive(Debug, Args)]
pub struct RunCmd {
    #[command(subcommand)]
    pub command: RunSub,
}

#[derive(Debug, Subcommand)]
pub enum RunSub {
    /// List workflow runs.
    List(ListArgs),
    /// View a workflow run, or its logs with `--log` / `--log-failed`.
    View(ViewArgs),
    /// Stream a run's logs live until the job finishes.
    Watch(WatchArgs),
    /// List or download a run's artifacts.
    Download(DownloadArgs),
    /// Rerun a workflow run.
    Rerun(IdArgs),
    /// Cancel an in-progress workflow run.
    Cancel(IdArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Print the full job log instead of the run summary.
    #[arg(long)]
    pub log: bool,
    /// Print only the log of steps that did not succeed.
    #[arg(long)]
    pub log_failed: bool,
    /// Job index within the run (0-based). Most runs have a single job.
    #[arg(short = 'j', long, default_value_t = 0)]
    pub job: usize,
    /// Attempt number. Defaults to the latest attempt (follows reruns).
    #[arg(long)]
    pub attempt: Option<u32>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Job index within the run (0-based).
    #[arg(short = 'j', long, default_value_t = 0)]
    pub job: usize,
    /// Attempt number. Defaults to the latest attempt.
    #[arg(long)]
    pub attempt: Option<u32>,
    /// Seconds between polls while the job is running.
    #[arg(short = 'i', long, default_value_t = 3)]
    pub interval: u64,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub run_id: u64,
}

pub async fn run(cmd: RunCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        RunSub::List(args) => list(args, host).await,
        RunSub::View(args) => view(args, host).await,
        RunSub::Watch(args) => watch(args, host).await,
        RunSub::Download(args) => workflow_artifact::download(args, host).await,
        RunSub::Rerun(args) => rerun(args, host).await,
        RunSub::Cancel(args) => cancel(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let runs = api::workflow::list_runs(&ctx.client, &ctx.owner, &ctx.name, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&runs)?);
    }
    if runs.is_empty() {
        println!("(no workflow runs)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = runs
        .iter()
        .map(|r| {
            let conclusion = r.conclusion.clone().unwrap_or_default();
            vec![
                r.run_number.to_string(),
                output::truncate(&r.name, 40),
                r.status.clone(),
                conclusion,
                output::truncate(&r.head_branch, 30),
                output::dim(
                    &r.updated_at
                        .map(output::relative_time)
                        .unwrap_or_else(|| "-".into()),
                ),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(
            &["#", "WORKFLOW", "STATUS", "CONCLUSION", "BRANCH", "UPDATED"],
            &rows
        )
    );
    Ok(())
}

async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;

    // Everything `view` shows comes from the Forgejo web routes, not the API:
    // `/api/v1` has no single-run or log endpoint on current Forgejo (see
    // api::workflow's run-logs section).
    guard_web_routes(&ctx.client)?;

    if args.log || args.log_failed {
        let attempt = match args.attempt {
            Some(a) => a,
            None => {
                api::workflow::latest_attempt(
                    &ctx.client,
                    &ctx.owner,
                    &ctx.name,
                    args.run_id,
                    args.job,
                )
                .await?
            }
        };
        let view = api::workflow::fetch_job_logs(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.run_id,
            args.job,
            attempt,
        )
        .await?;
        validate_job(&view, args.job)?;
        print_logs(&view, args.log_failed);
        return Ok(());
    }

    let view =
        api::workflow::fetch_run_summary(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    let run = &view.state.run;
    let url = absolutize(&ctx.client, &run.link);

    if args.json {
        let jobs: Vec<serde_json::Value> = run
            .jobs
            .iter()
            .map(|j| {
                serde_json::json!({ "name": j.name, "status": j.status, "duration": j.duration })
            })
            .collect();
        let value = serde_json::json!({
            "run_number": args.run_id,
            "title": run.title,
            "status": run.status,
            "branch": run.commit.branch.name,
            "sha": run.commit.short_sha,
            "pushed_by": run.commit.pusher.display_name,
            "url": url,
            "jobs": jobs,
        });
        return output::print_json(&value);
    }

    println!("Run #{}: {}", args.run_id, run.title);
    println!("Status:   {}", output::action_status(&run.status));
    println!("Branch:   {}", run.commit.branch.name);
    println!("SHA:      {}", run.commit.short_sha);
    if !run.commit.pusher.display_name.is_empty() {
        println!("Pushed:   {}", run.commit.pusher.display_name);
    }
    println!("URL:      {url}");
    if !run.jobs.is_empty() {
        println!();
        let rows: Vec<Vec<String>> = run
            .jobs
            .iter()
            .map(|j| {
                vec![
                    j.name.clone(),
                    output::action_status(&j.status),
                    output::dim(&j.duration),
                ]
            })
            .collect();
        let table = output::render_table(&["JOB", "STATUS", "DURATION"], &rows);
        println!("{}", table.trim_end());
        println!("{}", output::dim("Use --log to see step output."));
    }
    Ok(())
}

/// Render a fully-fetched job view. With `failed_only`, skip steps that
/// succeeded or were skipped.
fn print_logs(view: &api::workflow::RunView, failed_only: bool) {
    let job = &view.state.current_job;
    let title = if job.title.is_empty() {
        "job"
    } else {
        &job.title
    };
    println!(
        "{} {}",
        output::bold(title),
        output::action_status(&view.state.run.status),
    );
    if view.state.run.jobs.len() > 1 {
        let others: Vec<String> = view
            .state
            .run
            .jobs
            .iter()
            .map(|j| format!("{} ({})", j.name, j.duration))
            .collect();
        println!(
            "{}",
            output::dim(&format!("jobs in run: {}", others.join(", ")))
        );
    }
    let steps = &job.steps;
    for sl in &view.logs.steps_log {
        let meta = steps.get(sl.step);
        let status = meta.map(|s| s.status.as_str()).unwrap_or("");
        if failed_only && matches!(status, "success" | "skipped" | "") {
            continue;
        }
        let summary = meta.map(|s| s.summary.as_str()).unwrap_or("step");
        let duration = meta.map(|s| s.duration.as_str()).unwrap_or("");
        println!(
            "{} {} {}",
            output::bold(&format!("==> {summary}")),
            output::action_status(status),
            output::dim(duration),
        );
        for line in &sl.lines {
            println!("{}", line.message);
        }
    }
}

async fn watch(args: WatchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    let (owner, name) = (ctx.owner.as_str(), ctx.name.as_str());
    let attempt = match args.attempt {
        Some(a) => a,
        None => {
            api::workflow::latest_attempt(&ctx.client, owner, name, args.run_id, args.job).await?
        }
    };

    // Bootstrap: learn the step list (empty cursors return state, no lines).
    let boot = api::workflow::fetch_run_view(
        &ctx.client,
        owner,
        name,
        args.run_id,
        args.job,
        attempt,
        &[],
    )
    .await?;
    validate_job(&boot, args.job)?;
    let mut cursors: Vec<u64> = vec![0; boot.state.current_job.steps.len()];
    let mut header_shown: Vec<bool> = vec![false; cursors.len()];
    let mut stdout = std::io::stdout();

    loop {
        let reqs: Vec<api::workflow::LogCursor> = cursors
            .iter()
            .enumerate()
            .map(|(step, &cursor)| api::workflow::LogCursor {
                step,
                cursor,
                expanded: true,
            })
            .collect();
        let view = api::workflow::fetch_run_view(
            &ctx.client,
            owner,
            name,
            args.run_id,
            args.job,
            attempt,
            &reqs,
        )
        .await?;

        let steps = &view.state.current_job.steps;
        for sl in &view.logs.steps_log {
            if sl.lines.is_empty() {
                if sl.cursor > 0 && sl.step < cursors.len() {
                    cursors[sl.step] = sl.cursor;
                }
                continue;
            }
            if sl.step < header_shown.len() && !header_shown[sl.step] {
                header_shown[sl.step] = true;
                let summary = steps
                    .get(sl.step)
                    .map(|s| s.summary.as_str())
                    .unwrap_or("step");
                let _ = writeln!(stdout, "{}", output::bold(&format!("==> {summary}")));
            }
            for line in &sl.lines {
                let _ = writeln!(stdout, "{}", line.message);
            }
            if sl.step < cursors.len() {
                cursors[sl.step] = sl.cursor;
            }
        }
        let _ = stdout.flush();

        // New steps can appear as the job progresses; grow the cursor vectors.
        let n = view.state.current_job.steps.len();
        if n > cursors.len() {
            cursors.resize(n, 0);
            header_shown.resize(n, false);
        }

        if view.state.run.done {
            println!(
                "\n{} {}",
                output::bold("run finished:"),
                output::action_status(&view.state.run.status),
            );
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(args.interval.max(1))).await;
    }
}

/// Forgejo clamps an out-of-range job index to the first job rather than
/// 404ing, which would silently show the wrong logs. Reject it instead.
fn validate_job(view: &api::workflow::RunView, job: usize) -> Result<()> {
    let n = view.state.run.jobs.len();
    if n > 0 && job >= n {
        return Err(anyhow!(
            "job index {job} is out of range: this run has {n} job(s) (0..{})",
            n - 1
        ));
    }
    Ok(())
}

/// Most of `fj run` (view/watch/rerun/cancel) reads or POSTs Forgejo web
/// routes, which a Fjord-Account gateway does not proxy. Refuse early with an
/// actionable message rather than emitting a confusing 404 from the gateway.
pub(crate) fn guard_web_routes(client: &crate::client::Client) -> Result<()> {
    if matches!(client.auth_scheme(), AuthScheme::FjordBearer) {
        return Err(anyhow!(
            "`fj run` isn't supported over a Fjord Account gateway yet: it uses \
             Forgejo's web routes, which the gateway doesn't forward. Use a host \
             configured with a personal access token instead."
        ));
    }
    Ok(())
}

/// Turn a relative web path (`/owner/repo/actions/runs/56`) into an absolute
/// URL against the client's host origin. Returns the input unchanged if it is
/// empty or already absolute.
fn absolutize(client: &crate::client::Client, link: &str) -> String {
    if link.is_empty() || link.starts_with("http://") || link.starts_with("https://") {
        return link.to_string();
    }
    match client.base().join("/") {
        Ok(origin) => format!("{}{}", origin, link.trim_start_matches('/')),
        Err(_) => link.to_string(),
    }
}

async fn rerun(args: IdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    api::workflow::rerun(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    println!("✓ Re-ran workflow run #{}", args.run_id);
    Ok(())
}

async fn cancel(args: IdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    api::workflow::cancel(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    println!("✓ Cancelled workflow run #{}", args.run_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_view_with_jobs(n: usize) -> api::workflow::RunView {
        let jobs: Vec<serde_json::Value> = (0..n).map(|_| serde_json::json!({})).collect();
        serde_json::from_value(serde_json::json!({
            "state": { "run": { "jobs": jobs }, "currentJob": {} }
        }))
        .unwrap()
    }

    #[test]
    fn validate_job_accepts_in_range_index() {
        let view = run_view_with_jobs(2);
        assert!(validate_job(&view, 0).is_ok());
        assert!(validate_job(&view, 1).is_ok());
    }

    #[test]
    fn validate_job_rejects_out_of_range_index() {
        let view = run_view_with_jobs(2);
        assert!(validate_job(&view, 2).is_err());
    }

    #[test]
    fn validate_job_allows_any_index_when_no_jobs() {
        let view = run_view_with_jobs(0);
        assert!(validate_job(&view, 5).is_ok());
    }

    fn test_client() -> crate::client::Client {
        crate::client::Client::for_base_url(
            "https://git.example.com".parse().unwrap(),
            "tok".into(),
        )
        .unwrap()
    }

    #[test]
    fn absolutize_prefixes_a_relative_path_with_the_host_origin() {
        let c = test_client();
        assert_eq!(
            absolutize(&c, "/owner/repo/actions/runs/56"),
            "https://git.example.com/owner/repo/actions/runs/56"
        );
    }

    #[test]
    fn absolutize_leaves_absolute_urls_untouched() {
        let c = test_client();
        for url in ["https://elsewhere.test/x", "http://elsewhere.test/y", ""] {
            assert_eq!(absolutize(&c, url), url);
        }
    }

    #[test]
    fn guard_web_routes_allows_token_clients() {
        // A token-auth client (the default for `for_base_url`) is permitted;
        // only the Fjord gateway scheme is refused.
        assert!(guard_web_routes(&test_client()).is_ok());
    }
}
