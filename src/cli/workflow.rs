//! `fj run` and `fj secret` / `fj variable` — Forgejo Actions.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
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
    /// View a workflow run.
    View(ViewArgs),
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
    pub run_id: u64,
    #[arg(long)]
    pub json: bool,
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
    let r = api::workflow::get_run(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&r)?);
    }
    println!("Run #{}: {}", r.run_number, r.name);
    println!("Status:     {}", r.status);
    if let Some(c) = &r.conclusion {
        println!("Conclusion: {c}");
    }
    println!("Branch:     {}", r.head_branch);
    println!("SHA:        {}", r.head_sha);
    println!("URL:        {}", r.html_url);
    Ok(())
}

async fn rerun(args: IdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::workflow::rerun(&ctx.client, &ctx.owner, &ctx.name, args.run_id).await?;
    println!("✓ Re-ran workflow run #{}", args.run_id);
    Ok(())
}

async fn cancel(args: IdArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
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
