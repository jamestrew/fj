//! `fj run` — Forgejo Actions run management (list/view/watch/download/rerun/cancel).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
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
    /// Print a workflow run's logs.
    Log(LogArgs),
    /// Stream a run's logs live until the job finishes.
    Watch(WatchArgs),
    /// List or download a run's artifacts.
    Download(DownloadArgs),
    /// Rerun a workflow run, or just its failed/single jobs.
    Rerun(RerunArgs),
    /// Cancel an in-progress workflow run.
    Cancel(IdArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Maximum number of runs to fetch
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    /// Fetch every available page
    #[arg(long)]
    pub paginate: bool,
    /// Filter by run status
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by source branch
    #[arg(long)]
    pub branch: Option<String>,
    /// Filter by triggering event
    #[arg(long)]
    pub event: Option<String>,
    /// Filter by workflow file
    #[arg(long)]
    pub workflow: Option<String>,
    /// Emit JSON instead of a table
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
    /// Job index within the run (0-based). Omit to log every job (with
    /// `--log-failed`, only the jobs that did not succeed).
    #[arg(short = 'j', long)]
    pub job: Option<usize>,
    /// Attempt number. Defaults to the latest attempt (follows reruns).
    #[arg(long)]
    pub attempt: Option<u32>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Exit with status 1 unless the run conclusion is success or skipped.
    #[arg(long)]
    pub exit_status: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Job index within the run (0-based). Omit to log every job.
    #[arg(short = 'j', long)]
    pub job: Option<usize>,
    /// Attempt number. Defaults to the latest attempt exposed by the task log source.
    #[arg(long)]
    pub attempt: Option<u32>,
    /// Print only jobs that did not succeed.
    #[arg(long)]
    pub failed: bool,
    /// Emit JSON instead of text.
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
    /// Exit with status 1 unless the run conclusion is success or skipped.
    #[arg(long)]
    pub exit_status: bool,
}

#[derive(Debug, Args)]
pub struct DispatchArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Workflow file name, e.g. `ci.yml`.
    pub workflow: String,
    /// Git ref (branch or tag) to run on. Defaults to the repo's default branch.
    #[arg(long = "ref")]
    pub ref_: Option<String>,
    /// Workflow input as `key=value` (repeatable).
    #[arg(short = 'f', long = "field")]
    pub fields: Vec<String>,
    /// Emit JSON describing the triggered run.
    #[arg(long)]
    pub json: bool,
    /// Stream the triggered run's logs live until it finishes.
    #[arg(long)]
    pub watch: bool,
    /// Job index within the run to watch (0-based).
    #[arg(short = 'j', long, default_value_t = 0, requires = "watch")]
    pub job: usize,
    /// Seconds between polls while the job is running.
    #[arg(short = 'i', long, default_value_t = 3, requires = "watch")]
    pub interval: u64,
    /// Exit with status 1 unless the run conclusion is success or skipped.
    #[arg(long, requires = "watch")]
    pub exit_status: bool,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Workflow run id
    pub run_id: u64,
}

#[derive(Debug, Args)]
pub struct RerunArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Rerun only the jobs that did not succeed, instead of the whole run.
    #[arg(long, conflicts_with = "job")]
    pub failed: bool,
    /// Rerun a single job by its index (the row order shown by `fj run view`).
    #[arg(short = 'j', long)]
    pub job: Option<usize>,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: RunCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        RunSub::List(args) => list(args, host).await,
        RunSub::View(args) => view(args, host).await,
        RunSub::Log(args) => log(args, host).await,
        RunSub::Watch(args) => watch(args, host).await,
        RunSub::Download(args) => workflow_artifact::download(args, host).await,
        RunSub::Rerun(args) => rerun(args, host).await,
        RunSub::Cancel(args) => cancel(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn dispatch(args: DispatchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let ref_ = match args.ref_ {
        Some(r) => r,
        None => {
            api::repo::get(&ctx.client, &ctx.owner, &ctx.name)
                .await?
                .default_branch
        }
    };
    let inputs = parse_dispatch_fields(&args.fields)?;
    let seen = dispatch_run_keys(&ctx, &args.workflow, &ref_).await?;
    api::workflow::dispatch(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &args.workflow,
        &ref_,
        inputs,
    )
    .await?;
    let run = wait_for_dispatched_run(&ctx, &args.workflow, &ref_, &seen).await?;
    let handle = DispatchRunHandle::from_run(&ctx, &run);

    if args.json {
        output::print_json(&serde_json::to_value(&handle)?)?;
    } else {
        println!("✓ Dispatched {} on {}", args.workflow, ref_);
        println!("Run #{}: {}", handle.run_number, handle.url);
    }

    if args.watch {
        return watch_resolved(
            &ctx,
            WatchOptions {
                run_id: handle.run_number,
                job: args.job,
                attempt: None,
                interval: args.interval,
                exit_status: args.exit_status,
            },
        )
        .await;
    }

    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let filters = api::workflow::RunListFilters {
        status: args.status,
        branch: args.branch,
        event: args.event,
        workflow: args.workflow,
    };
    let runs = api::workflow::list_runs_filtered(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.limit,
        args.paginate,
        &filters,
    )
    .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&runs)?);
    }
    if runs.is_empty() {
        println!("(no workflow runs)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = runs.iter().map(run_list_row).collect();
    print!("{}", output::render_table(run_list_headers(), &rows));
    Ok(())
}

fn run_list_headers() -> &'static [&'static str] {
    &["#", "WORKFLOW", "STATUS", "BRANCH", "UPDATED"]
}

fn run_list_row(r: &api::workflow::WorkflowRun) -> Vec<String> {
    vec![
        r.run_number.to_string(),
        output::truncate(&r.name, 40),
        r.status.clone(),
        output::truncate(&r.head_branch, 30),
        output::dim(
            &r.updated_at
                .map(output::relative_time)
                .unwrap_or_else(|| "-".into()),
        ),
    ]
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;

    if args.log || args.log_failed {
        let logs = api::workflow::fetch_run_logs(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.run_id,
            args.job,
            args.attempt,
            args.log_failed,
        )
        .await?;
        if args.json {
            output::print_json(&serde_json::to_value(&logs)?)?;
        } else {
            print_run_logs(&logs);
        }
        return ensure_exit_status(logs.status().unwrap_or(""), args.exit_status);
    }

    // Non-log `view` still reads Forgejo web routes. The API/Loki log path
    // above intentionally stays available for Fjord Account gateway hosts.
    guard_web_routes(&ctx.client)?;

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
        output::print_json(&value)?;
        return ensure_exit_status(&run.status, args.exit_status);
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
    ensure_exit_status(&run.status, args.exit_status)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn log(args: LogArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let logs = api::workflow::fetch_run_logs(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.run_id,
        args.job,
        args.attempt,
        args.failed,
    )
    .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&logs)?);
    }
    print_run_logs(&logs);
    Ok(())
}

fn print_run_logs(logs: &api::workflow::RunLogs) {
    if logs.jobs.is_empty() {
        println!("{}", output::dim("No matching jobs in this run."));
        return;
    }
    let multi = logs.jobs.len() > 1;
    for (i, job) in logs.jobs.iter().enumerate() {
        if multi {
            if i > 0 {
                println!();
            }
            let task = job
                .task_id
                .map(|id| format!("task {id}"))
                .unwrap_or_else(|| "task unknown".into());
            println!(
                "{} {} {}",
                output::bold(&format!("==> {}", job.name)),
                output::action_status(&job.status),
                output::dim(&format!("{} - {task}", logs.source))
            );
        }
        for line in &job.lines {
            println!("{}", line.message);
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn watch(args: WatchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    watch_resolved(
        &ctx,
        WatchOptions {
            run_id: args.run_id,
            job: args.job,
            attempt: args.attempt,
            interval: args.interval,
            exit_status: args.exit_status,
        },
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct WatchOptions {
    run_id: u64,
    job: usize,
    attempt: Option<u32>,
    interval: u64,
    exit_status: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn watch_resolved(ctx: &RepoContext, args: WatchOptions) -> Result<()> {
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
            return ensure_exit_status(&view.state.run.status, args.exit_status);
        }
        tokio::time::sleep(Duration::from_secs(args.interval.max(1))).await;
    }
}

#[derive(Debug, serde::Serialize)]
struct DispatchRunHandle {
    id: u64,
    run_number: u64,
    workflow: String,
    status: String,
    conclusion: Option<String>,
    url: String,
}

impl DispatchRunHandle {
    fn from_run(ctx: &RepoContext, run: &api::workflow::WorkflowRun) -> Self {
        Self {
            id: run.id,
            run_number: run.run_number,
            workflow: run.workflow_id.clone(),
            status: run.status.clone(),
            conclusion: run.conclusion.clone(),
            url: run_url(ctx, run),
        }
    }
}

fn parse_dispatch_fields(fields: &[String]) -> Result<BTreeMap<String, String>> {
    let mut inputs = BTreeMap::new();
    for f in fields {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --field '{f}', expected key=value"))?;
        inputs.insert(k.to_string(), v.to_string());
    }
    Ok(inputs)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn dispatch_run_keys(
    ctx: &RepoContext,
    workflow: &str,
    ref_: &str,
) -> Result<BTreeSet<RunKey>> {
    Ok(dispatch_matching_runs(ctx, workflow, ref_)
        .await?
        .iter()
        .map(RunKey::from)
        .collect())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn wait_for_dispatched_run(
    ctx: &RepoContext,
    workflow: &str,
    ref_: &str,
    seen: &BTreeSet<RunKey>,
) -> Result<api::workflow::WorkflowRun> {
    for _ in 0..30 {
        let runs = dispatch_matching_runs(ctx, workflow, ref_).await?;
        if let Some(run) = find_new_dispatch_run(&runs, seen) {
            return Ok(run.clone());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!(
        "dispatched `{workflow}` on `{ref_}`, but could not find the triggered run"
    ))
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn dispatch_matching_runs(
    ctx: &RepoContext,
    workflow: &str,
    ref_: &str,
) -> Result<Vec<api::workflow::WorkflowRun>> {
    let runs = api::workflow::list_runs(&ctx.client, &ctx.owner, &ctx.name, 50).await?;
    Ok(runs
        .into_iter()
        .filter(|run| run_matches_dispatch(run, workflow, ref_))
        .collect())
}

fn run_matches_dispatch(run: &api::workflow::WorkflowRun, workflow: &str, ref_: &str) -> bool {
    workflow_matches_dispatch(&run.workflow_id, workflow)
        && run.head_branch == normalize_dispatch_ref(ref_)
        && run.event == "workflow_dispatch"
}

fn workflow_matches_dispatch(actual: &str, wanted: &str) -> bool {
    actual == wanted || actual.rsplit('/').next() == Some(wanted)
}

fn normalize_dispatch_ref(ref_: &str) -> String {
    ref_.strip_prefix("refs/heads/").unwrap_or(ref_).to_string()
}

fn find_new_dispatch_run<'a>(
    runs: &'a [api::workflow::WorkflowRun],
    seen: &BTreeSet<RunKey>,
) -> Option<&'a api::workflow::WorkflowRun> {
    runs.iter().find(|run| !seen.contains(&RunKey::from(*run)))
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct RunKey {
    id: u64,
    run_number: u64,
}

impl From<&api::workflow::WorkflowRun> for RunKey {
    fn from(run: &api::workflow::WorkflowRun) -> Self {
        Self {
            id: run.id,
            run_number: run.run_number,
        }
    }
}

fn run_url(ctx: &RepoContext, run: &api::workflow::WorkflowRun) -> String {
    if !run.html_url.is_empty() {
        return absolutize(&ctx.client, &run.html_url);
    }
    let link = format!(
        "/{}/{}/actions/runs/{}",
        api::path_segment(&ctx.owner),
        api::path_segment(&ctx.name),
        run.run_number
    );
    absolutize(&ctx.client, &link)
}

fn ensure_exit_status(status: &str, exit_status: bool) -> Result<()> {
    if !exit_status || run_status_succeeded(status) {
        return Ok(());
    }
    Err(anyhow!(
        "workflow run concluded with status `{status}` (expected success or skipped)"
    ))
}

fn run_status_succeeded(status: &str) -> bool {
    matches!(status, "success" | "skipped")
}

/// Which job indices `view --log` should fetch when `--job` is omitted.
/// `--log` spans every job; `--log-failed` keeps only the jobs that did not
/// finish green (anything but `success` / `skipped`), so a failure isn't
/// hidden behind an earlier passing job.
#[cfg(test)]
fn jobs_to_log(statuses: &[String], failed_only: bool) -> Vec<usize> {
    if !failed_only {
        return (0..statuses.len()).collect();
    }
    failed_job_indices(statuses)
}

/// Indices of jobs that did not finish green (anything but `success` /
/// `skipped`). Shared by `view --log-failed` and `rerun --failed` so both treat
/// "failed" identically.
fn failed_job_indices(statuses: &[String]) -> Vec<usize> {
    statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| !matches!(s.as_str(), "success" | "skipped"))
        .map(|(i, _)| i)
        .collect()
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn rerun(args: RerunArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    let (owner, name) = (ctx.owner.as_str(), ctx.name.as_str());

    // `--job N` and `--failed` both target individual jobs through Forgejo's
    // per-job rerun route; only the bare command reruns the whole run.
    if let Some(job) = args.job {
        // Validate the index against the run so a typo'd `--job` fails with a
        // clear message instead of Forgejo's bare 404.
        let summary =
            api::workflow::fetch_run_summary(&ctx.client, owner, name, args.run_id).await?;
        validate_job(&summary, job)?;
        api::workflow::rerun_job(&ctx.client, owner, name, args.run_id, job).await?;
        println!("✓ Re-ran job {job} of workflow run #{}", args.run_id);
        return Ok(());
    }

    if args.failed {
        let summary =
            api::workflow::fetch_run_summary(&ctx.client, owner, name, args.run_id).await?;
        let statuses: Vec<String> = summary
            .state
            .run
            .jobs
            .iter()
            .map(|j| j.status.clone())
            .collect();
        let failed = failed_job_indices(&statuses);
        if failed.is_empty() {
            let note = if statuses.is_empty() {
                "This run has no jobs."
            } else {
                "No failed jobs to rerun."
            };
            println!("{}", output::dim(note));
            return Ok(());
        }
        for &job in &failed {
            api::workflow::rerun_job(&ctx.client, owner, name, args.run_id, job).await?;
        }
        println!(
            "✓ Re-ran {} failed job(s) of workflow run #{}",
            failed.len(),
            args.run_id
        );
        return Ok(());
    }

    api::workflow::rerun(&ctx.client, owner, name, args.run_id).await?;
    println!("✓ Re-ran workflow run #{}", args.run_id);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

    fn statuses(ss: &[&str]) -> Vec<String> {
        ss.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ensure_exit_status_ignores_failed_runs_without_flag() {
        assert!(ensure_exit_status("failure", false).is_ok());
    }

    #[test]
    fn ensure_exit_status_accepts_success_and_skipped() {
        assert!(ensure_exit_status("success", true).is_ok());
        assert!(ensure_exit_status("skipped", true).is_ok());
    }

    #[test]
    fn ensure_exit_status_rejects_failed_and_cancelled_runs() {
        assert!(ensure_exit_status("failure", true).is_err());
        assert!(ensure_exit_status("cancelled", true).is_err());
    }

    #[test]
    fn failed_job_indices_keeps_only_non_green_jobs() {
        let ss = statuses(&["success", "failure", "skipped", "cancelled"]);
        assert_eq!(failed_job_indices(&ss), vec![1, 3]);
    }

    #[test]
    fn failed_job_indices_empty_when_all_green() {
        let ss = statuses(&["success", "skipped"]);
        assert!(failed_job_indices(&ss).is_empty());
    }

    #[test]
    fn jobs_to_log_without_failed_only_spans_every_job() {
        let ss = statuses(&["success", "failure", "success"]);
        assert_eq!(jobs_to_log(&ss, false), vec![0, 1, 2]);
        assert_eq!(jobs_to_log(&ss, true), vec![1]);
    }

    #[test]
    fn rerun_failed_and_job_are_mutually_exclusive() {
        use clap::Parser;

        let err =
            crate::cli::Cli::try_parse_from(["fj", "run", "rerun", "56", "--failed", "--job", "1"])
                .expect_err("--failed and --job together should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn rerun_job_flag_parses() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["fj", "run", "rerun", "56", "--job", "2"])
            .expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::Rerun(args),
        }) = cli.command
        else {
            panic!("expected run rerun command");
        };
        assert_eq!(args.run_id, 56);
        assert_eq!(args.job, Some(2));
        assert!(!args.failed);
    }

    #[test]
    fn rerun_failed_flag_parses() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["fj", "run", "rerun", "56", "--failed"])
            .expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::Rerun(args),
        }) = cli.command
        else {
            panic!("expected run rerun command");
        };
        assert_eq!(args.run_id, 56);
        assert!(args.failed);
        assert_eq!(args.job, None);
    }

    #[test]
    fn rerun_bare_leaves_flags_unset() {
        use clap::Parser;

        let cli =
            crate::cli::Cli::try_parse_from(["fj", "run", "rerun", "56"]).expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::Rerun(args),
        }) = cli.command
        else {
            panic!("expected run rerun command");
        };
        assert!(!args.failed);
        assert_eq!(args.job, None);
    }

    #[test]
    fn run_list_filter_flags_parse() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from([
            "fj",
            "run",
            "list",
            "--paginate",
            "--status",
            "failure",
            "--branch",
            "main",
            "--event",
            "push",
            "--workflow",
            "ci.yml",
        ])
        .expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::List(args),
        }) = cli.command
        else {
            panic!("expected run list command");
        };
        assert!(args.paginate);
        assert_eq!(args.status.as_deref(), Some("failure"));
        assert_eq!(args.branch.as_deref(), Some("main"));
        assert_eq!(args.event.as_deref(), Some("push"));
        assert_eq!(args.workflow.as_deref(), Some("ci.yml"));
    }

    #[test]
    fn workflow_run_handle_flags_parse() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from([
            "fj",
            "workflow",
            "run",
            "ci.yml",
            "--ref",
            "main",
            "--field",
            "deploy=true",
            "--json",
            "--watch",
            "--exit-status",
            "--job",
            "1",
            "--interval",
            "2",
        ])
        .expect("should parse");
        let crate::cli::Command::Workflow(crate::cli::workflow::WorkflowCmd {
            command: crate::cli::workflow::WorkflowSub::Run(args),
        }) = cli.command
        else {
            panic!("expected workflow run command");
        };
        assert_eq!(args.workflow, "ci.yml");
        assert_eq!(args.ref_.as_deref(), Some("main"));
        assert_eq!(args.fields, vec!["deploy=true"]);
        assert!(args.json);
        assert!(args.watch);
        assert!(args.exit_status);
        assert_eq!(args.job, 1);
        assert_eq!(args.interval, 2);
    }

    #[test]
    fn workflow_run_exit_status_requires_watch() {
        use clap::Parser;

        let err =
            crate::cli::Cli::try_parse_from(["fj", "workflow", "run", "ci.yml", "--exit-status"])
                .expect_err("--exit-status without --watch should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parse_dispatch_fields_accepts_key_value_pairs() {
        let fields = vec!["env=prod".to_string(), "dry_run=false".to_string()];
        let parsed = parse_dispatch_fields(&fields).expect("fields should parse");
        assert_eq!(parsed.get("env").map(String::as_str), Some("prod"));
        assert_eq!(parsed.get("dry_run").map(String::as_str), Some("false"));
    }

    #[test]
    fn parse_dispatch_fields_rejects_missing_equals() {
        let fields = vec!["env".to_string()];
        assert!(parse_dispatch_fields(&fields).is_err());
    }

    #[test]
    fn run_matches_dispatch_accepts_matching_workflow_ref_and_event() {
        let run = workflow_run(10, 20);
        assert!(run_matches_dispatch(&run, "ci.yml", "refs/heads/main"));
    }

    #[test]
    fn run_matches_dispatch_rejects_other_events() {
        let mut run = workflow_run(10, 20);
        run.event = "push".to_string();
        assert!(!run_matches_dispatch(&run, "ci.yml", "main"));
    }

    #[test]
    fn dispatch_ref_normalization_strips_refs_heads_prefix() {
        assert_eq!(normalize_dispatch_ref("refs/heads/main"), "main");
        assert_eq!(normalize_dispatch_ref("main"), "main");
    }

    fn workflow_run(id: u64, run_number: u64) -> api::workflow::WorkflowRun {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "run_number": run_number,
            "workflow_id": "ci.yml",
            "head_branch": "main",
            "event": "workflow_dispatch",
            "status": "queued"
        }))
        .unwrap()
    }

    #[test]
    fn find_new_dispatch_run_skips_seen_runs() {
        let old = workflow_run(10, 20);
        let new = workflow_run(11, 21);
        let seen = [RunKey::from(&old)].into_iter().collect();
        let runs = vec![old, new];
        let found = find_new_dispatch_run(&runs, &seen).expect("new run should be selected");
        assert_eq!(found.run_number, 21);
    }

    #[test]
    fn find_new_dispatch_run_returns_none_when_every_run_is_seen() {
        let old = workflow_run(10, 20);
        let seen = [RunKey::from(&old)].into_iter().collect();
        assert!(find_new_dispatch_run(&[old], &seen).is_none());
    }

    #[test]
    fn run_view_exit_status_flag_parses() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["fj", "run", "view", "42", "--exit-status"])
            .expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::View(args),
        }) = cli.command
        else {
            panic!("expected run view command");
        };
        assert!(args.exit_status);
    }

    #[test]
    fn run_watch_exit_status_flag_parses() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["fj", "run", "watch", "42", "--exit-status"])
            .expect("should parse");
        let crate::cli::Command::Run(RunCmd {
            command: RunSub::Watch(args),
        }) = cli.command
        else {
            panic!("expected run watch command");
        };
        assert!(args.exit_status);
    }

    #[test]
    fn jobs_to_log_full_spans_every_job() {
        let s = statuses(&["success", "failure", "skipped"]);
        assert_eq!(jobs_to_log(&s, false), vec![0, 1, 2]);
        assert_eq!(jobs_to_log(&statuses(&[]), false), Vec::<usize>::new());
    }

    #[test]
    fn jobs_to_log_failed_keeps_only_non_green_jobs() {
        // job 0 passed, job 1 failed, job 2 skipped -> only job 1
        let s = statuses(&["success", "failure", "skipped"]);
        assert_eq!(jobs_to_log(&s, true), vec![1]);
        // cancelled / error count as failed too
        assert_eq!(
            jobs_to_log(&statuses(&["cancelled", "error"]), true),
            vec![0, 1]
        );
        // all green -> empty (caller prints "No failed jobs")
        assert!(jobs_to_log(&statuses(&["success", "skipped"]), true).is_empty());
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
    fn run_list_table_uses_status_without_dead_conclusion_column() {
        let run = api::workflow::WorkflowRun {
            run_number: 42,
            name: "CI".into(),
            status: "success".into(),
            conclusion: None,
            head_branch: "main".into(),
            ..serde_json::from_value(serde_json::json!({})).unwrap()
        };
        assert_eq!(
            run_list_headers(),
            &["#", "WORKFLOW", "STATUS", "BRANCH", "UPDATED"]
        );
        let row = run_list_row(&run);
        assert_eq!(row[2], "success");
        assert_eq!(row.len(), run_list_headers().len());
    }

    #[test]
    fn run_list_status_help_names_status_only() {
        use clap::CommandFactory;
        let help = crate::cli::Cli::command()
            .find_subcommand_mut("run")
            .unwrap()
            .find_subcommand_mut("list")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("Filter by run status"));
        assert!(!help.contains("Filter by run status or conclusion"));
    }

    #[test]
    fn guard_web_routes_allows_token_clients() {
        // A token-auth client (the default for `for_base_url`) is permitted;
        // only the Fjord gateway scheme is refused.
        assert!(guard_web_routes(&test_client()).is_ok());
    }
}
