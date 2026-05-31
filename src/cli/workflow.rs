//! `fj run` and `fj secret` / `fj variable` — Forgejo Actions.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
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

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Run number, as shown in the `#` column of `fj run list`.
    pub run_id: u64,
    /// Download only this artifact (by name). Omit to download all.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Output directory (default: cwd). Each artifact is written as `<name>.zip`.
    #[arg(short = 'o', long)]
    pub output_dir: Option<String>,
    /// List the run's artifacts instead of downloading.
    #[arg(long)]
    pub list: bool,
    #[arg(long)]
    pub json: bool,
}

pub async fn run(cmd: RunCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        RunSub::List(args) => list(args, host).await,
        RunSub::View(args) => view(args, host).await,
        RunSub::Watch(args) => watch(args, host).await,
        RunSub::Download(args) => download(args, host).await,
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
                truncate(&r.name, 40),
                r.status.clone(),
                conclusion,
                truncate(&r.head_branch, 30),
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

async fn download(args: DownloadArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    guard_web_routes(&ctx.client)?;
    let arts =
        api::workflow::list_artifacts(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&arts)?);
    }
    if arts.is_empty() {
        println!("(no artifacts)");
        return Ok(());
    }
    if args.list {
        let rows: Vec<Vec<String>> = arts
            .iter()
            .map(|a| {
                vec![
                    a.name.clone(),
                    human_size(a.size),
                    output::action_status(&a.status),
                ]
            })
            .collect();
        print!(
            "{}",
            output::render_table(&["NAME", "SIZE", "STATUS"], &rows)
        );
        return Ok(());
    }
    let out = args.output_dir.unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out)?;
    let mut got = 0usize;
    for a in &arts {
        if let Some(filter) = &args.name {
            if &a.name != filter {
                continue;
            }
        }
        if a.status == "expired" {
            println!("{}", output::dim(&format!("skipping {} (expired)", a.name)));
            continue;
        }
        let dest = artifact_dest(Path::new(&out), &a.name)?;
        let bytes = api::workflow::download_artifact(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.run_id,
            &a.name,
        )
        .await?;
        std::fs::write(&dest, &bytes)?;
        println!("✓ Downloaded {} → {}", a.name, dest.display());
        got += 1;
    }
    if got == 0 {
        println!("(no artifacts matched)");
    }
    Ok(())
}

/// Destination path for an artifact zip, rejecting names that try to escape
/// the output directory.
fn artifact_dest(dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(anyhow!("refusing unsafe artifact name {name:?}"));
    }
    Ok(dir.join(format!("{name}.zip")))
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
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
fn guard_web_routes(client: &crate::client::Client) -> Result<()> {
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ── Workflows ────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct WorkflowCmd {
    #[command(subcommand)]
    pub command: WorkflowSub,
}

#[derive(Debug, Subcommand)]
pub enum WorkflowSub {
    /// List workflow files in the repo.
    List(WorkflowListArgs),
    /// Trigger a `workflow_dispatch` run of a workflow file.
    Run(DispatchArgs),
}

#[derive(Debug, Args)]
pub struct WorkflowListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(long)]
    pub json: bool,
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
}

pub async fn run_workflow(cmd: WorkflowCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        WorkflowSub::List(args) => list_workflows(args, host).await,
        WorkflowSub::Run(args) => dispatch(args, host).await,
    }
}

async fn list_workflows(args: WorkflowListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let files = api::workflow::list_workflows(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&files)?);
    }
    if files.is_empty() {
        println!("(no workflow files found in .forgejo/workflows or .github/workflows)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = files
        .iter()
        .map(|f| vec![f.name.clone(), output::dim(&f.path)])
        .collect();
    print!("{}", output::render_table(&["WORKFLOW", "PATH"], &rows));
    Ok(())
}

async fn dispatch(args: DispatchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let ref_ = match args.ref_ {
        Some(r) => r,
        None => {
            api::repo::get(&ctx.client, &ctx.owner, &ctx.name)
                .await?
                .default_branch
        }
    };
    let mut inputs = std::collections::BTreeMap::new();
    for f in &args.fields {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --field '{f}', expected key=value"))?;
        inputs.insert(k.to_string(), v.to_string());
    }
    api::workflow::dispatch(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &args.workflow,
        &ref_,
        inputs,
    )
    .await?;
    println!("✓ Dispatched {} on {}", args.workflow, ref_);
    Ok(())
}

// ── Secrets ─────────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct SecretCmd {
    #[command(subcommand)]
    pub command: SecretSub,
}

#[derive(Debug, Subcommand)]
pub enum SecretSub {
    /// List secret names (values are write-only).
    List(SecretListArgs),
    /// Set or replace a secret.
    Set(SecretSetArgs),
    /// Delete a secret.
    Delete(SecretDeleteArgs),
}

#[derive(Debug, Args)]
pub struct SecretListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
}

#[derive(Debug, Args)]
pub struct SecretSetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    /// Value (raw string). For multi-line secrets, prefer `--from-file -` and pipe stdin.
    #[arg(short = 'v', long)]
    pub value: Option<String>,
    /// Read value from this file. Use `-` for stdin.
    #[arg(short = 'f', long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct SecretDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub name: String,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run_secret(cmd: SecretCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        SecretSub::List(args) => secret_list(args, host).await,
        SecretSub::Set(args) => secret_set(args, host).await,
        SecretSub::Delete(args) => secret_delete(args, host).await,
    }
}

async fn secret_list(args: SecretListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let s = api::workflow::list_secrets(&ctx.client, &ctx.owner, &ctx.name).await?;
    if s.is_empty() {
        println!("(no secrets)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = s
        .iter()
        .map(|x| {
            vec![
                x.name.clone(),
                output::dim(&output::relative_time(x.created_at)),
            ]
        })
        .collect();
    print!("{}", output::render_table(&["NAME", "CREATED"], &rows));
    Ok(())
}

async fn secret_set(args: SecretSetArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    api::workflow::set_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value).await?;
    println!("✓ Set secret {}", args.name);
    Ok(())
}

async fn secret_delete(args: SecretDeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete secret '{}'? Type name to confirm",
            args.name
        ))?;
        if ans != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::workflow::delete_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
    println!("✓ Deleted secret {}", args.name);
    Ok(())
}

// ── Variables ──────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct VariableCmd {
    #[command(subcommand)]
    pub command: VariableSub,
}

#[derive(Debug, Subcommand)]
pub enum VariableSub {
    /// List Actions variables.
    List(SecretListArgs),
    /// Set or replace a variable.
    Set(SecretSetArgs),
    /// Delete a variable.
    Delete(SecretDeleteArgs),
}

pub async fn run_variable(cmd: VariableCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        VariableSub::List(args) => var_list(args, host).await,
        VariableSub::Set(args) => var_set(args, host).await,
        VariableSub::Delete(args) => var_delete(args, host).await,
    }
}

async fn var_list(args: SecretListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let v = api::workflow::list_variables(&ctx.client, &ctx.owner, &ctx.name).await?;
    if v.is_empty() {
        println!("(no variables)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = v
        .iter()
        .map(|x| vec![x.name.clone(), x.data.clone()])
        .collect();
    print!("{}", output::render_table(&["NAME", "VALUE"], &rows));
    Ok(())
}

async fn var_set(args: SecretSetArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    api::workflow::set_variable(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value).await?;
    println!("✓ Set variable {}", args.name);
    Ok(())
}

async fn var_delete(args: SecretDeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete variable '{}'? Type name to confirm",
            args.name
        ))?;
        if ans != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    api::workflow::delete_variable(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
    println!("✓ Deleted variable {}", args.name);
    Ok(())
}

fn read_value(value: Option<&str>, from_file: Option<&str>) -> Result<String> {
    if let Some(v) = value {
        return Ok(v.to_string());
    }
    if let Some(path) = from_file {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf.trim_end().to_string());
        }
        return Ok(std::fs::read_to_string(path)?.trim_end().to_string());
    }
    Err(anyhow::anyhow!(
        "pass --value or --from-file (use `-` for stdin)"
    ))
}
