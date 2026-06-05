//! `fj workflow` — list workflow files and trigger `workflow_dispatch`.
//!
//! Thin facade: the run / secret / variable command groups live in flat
//! sibling modules (`workflow_run`, `workflow_secret`, `workflow_variable`).
//! The glob re-exports below keep every `cli::workflow::*` path resolving
//! (e.g. the `Command` enum's `RunCmd` / `SecretCmd` / `VariableCmd`).

pub use crate::cli::workflow_run::*;
pub use crate::cli::workflow_secret::*;
pub use crate::cli::workflow_variable::*;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

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
    /// Emit JSON instead of a table
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
