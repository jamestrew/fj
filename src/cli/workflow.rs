//! `fj workflow` — list workflow files and trigger `workflow_dispatch`.
//!
//! Thin facade: the run / secret / variable command groups live in flat
//! sibling modules (`workflow_run`, `workflow_secret`, `workflow_variable`).
//! The glob re-exports below keep every `cli::workflow::*` path resolving
//! (e.g. the `Command` enum's `RunCmd` / `SecretCmd` / `VariableCmd`).

pub use crate::cli::workflow_run::*;
pub use crate::cli::workflow_secret::*;
pub use crate::cli::workflow_variable::*;

use anyhow::Result;
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
    /// Enable a workflow file.
    Enable(WorkflowEnableArgs),
    /// Disable a workflow file.
    Disable(WorkflowDisableArgs),
    /// View workflow metadata and state.
    View(WorkflowViewArgs),
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
pub struct WorkflowEnableArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Workflow file name, e.g. `ci.yml`.
    pub workflow: String,
}

#[derive(Debug, Args)]
pub struct WorkflowDisableArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Workflow file name, e.g. `ci.yml`.
    pub workflow: String,
}

#[derive(Debug, Args)]
pub struct WorkflowViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Workflow file name, e.g. `ci.yml`.
    pub workflow: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run_workflow(cmd: WorkflowCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        WorkflowSub::List(args) => list_workflows(args, host).await,
        WorkflowSub::Run(args) => dispatch(args, host).await,
        WorkflowSub::Enable(args) => enable(args, host).await,
        WorkflowSub::Disable(args) => disable(args, host).await,
        WorkflowSub::View(args) => view(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn enable(args: WorkflowEnableArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::workflow::enable_workflow(&ctx.client, &ctx.owner, &ctx.name, &args.workflow).await?;
    println!("✓ Enabled {}", args.workflow);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn disable(args: WorkflowDisableArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::workflow::disable_workflow(&ctx.client, &ctx.owner, &ctx.name, &args.workflow).await?;
    println!("✓ Disabled {}", args.workflow);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: WorkflowViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let wf =
        api::workflow::get_workflow(&ctx.client, &ctx.owner, &ctx.name, &args.workflow).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&wf)?);
    }

    let name = if wf.name.is_empty() {
        args.workflow.clone()
    } else {
        wf.name.clone()
    };
    let path = if wf.path.is_empty() {
        args.workflow
    } else {
        wf.path
    };
    println!("{}", output::bold(&name));
    println!("Path:      {}", path);
    println!(
        "State:     {}",
        if wf.state.is_empty() {
            "unknown"
        } else {
            &wf.state
        }
    );
    if !wf.id.is_empty() {
        println!("ID:        {}", wf.id);
    }
    if !wf.url.is_empty() {
        println!("API URL:   {}", wf.url);
    }
    if !wf.html_url.is_empty() {
        println!("Run URL:   {}", wf.html_url);
    }
    if !wf.badge_url.is_empty() {
        println!("Badge URL: {}", wf.badge_url);
    }
    if let Some(created) = wf.created_at {
        println!("Created:   {}", output::relative_time(created));
    }
    if let Some(updated) = wf.updated_at {
        println!("Updated:   {}", output::relative_time(updated));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn parse_workflow_enable_args() {
        let cli = Cli::parse_from(["fj", "workflow", "enable", "-R", "rasterstate/fj", "ci.yml"]);
        match cli.command {
            Command::Workflow(cmd) => match cmd.command {
                WorkflowSub::Enable(args) => {
                    assert_eq!(args.r.repo, Some("rasterstate/fj".to_string()));
                    assert_eq!(args.workflow, "ci.yml");
                }
                _ => panic!("expected workflow enable"),
            },
            _ => panic!("not workflow command"),
        }
    }

    #[test]
    fn parse_workflow_disable_args() {
        let cli = Cli::parse_from([
            "fj",
            "workflow",
            "disable",
            "-R",
            "rasterstate/fj",
            "ci.yml",
        ]);
        match cli.command {
            Command::Workflow(cmd) => match cmd.command {
                WorkflowSub::Disable(args) => {
                    assert_eq!(args.r.repo, Some("rasterstate/fj".to_string()));
                    assert_eq!(args.workflow, "ci.yml");
                }
                _ => panic!("expected workflow disable"),
            },
            _ => panic!("not workflow command"),
        }
    }

    #[test]
    fn parse_workflow_view_args_has_json_flag() {
        let cli = Cli::parse_from([
            "fj",
            "workflow",
            "view",
            "--json",
            "-R",
            "rasterstate/fj",
            "ci.yml",
        ]);
        match cli.command {
            Command::Workflow(cmd) => match cmd.command {
                WorkflowSub::View(args) => {
                    assert!(args.json);
                    assert_eq!(args.workflow, "ci.yml");
                }
                _ => panic!("expected workflow view"),
            },
            _ => panic!("not workflow command"),
        }
    }
}
