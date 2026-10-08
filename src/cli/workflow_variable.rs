//! `fj variable`: Forgejo Actions variables (list/set/delete).
//!
//! Variables are plain, readable configuration. They are a separate surface
//! from `fj secret`, whose values are write-only: this group has its own
//! argument types and wording so the two are never presented as one thing.
//! The pure rendering helpers live in `workflow_secret` (which carries the
//! unit tests for both groups).

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::RepoFlag;
use crate::cli::workflow_secret::{
    read_value, resolve_workflow_scope, variable_rows, variable_set_message, WorkflowScope,
};
use crate::output;

#[derive(Debug, Args)]
pub struct VariableCmd {
    #[command(subcommand)]
    pub command: VariableSub,
}

#[derive(Debug, Subcommand)]
pub enum VariableSub {
    /// List Actions variables with their values. Variables are plain
    /// configuration, not secrets: anyone who can read this list can read
    /// the values. Use `fj secret` for anything sensitive.
    List(VariableListArgs),
    /// Create a variable, or replace its value if it already exists. The
    /// value is stored readable (not a secret); use `fj secret set` for
    /// credentials.
    Set(VariableSetArgs),
    /// Delete a variable.
    Delete(VariableDeleteArgs),
}

#[derive(Debug, Args)]
pub struct VariableListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope: variables shared by the organization's repos.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct VariableSetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope: a variable shared by the organization's repos.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Variable name
    pub name: String,
    /// Value (raw string, stored readable). For multi-line values, prefer
    /// `--from-file -` and pipe stdin.
    #[arg(short = 'v', long)]
    pub value: Option<String>,
    /// Read value from this file. Use `-` for stdin.
    #[arg(short = 'f', long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct VariableDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope: a variable shared by the organization's repos.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Variable name
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run_variable(cmd: VariableCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        VariableSub::List(args) => var_list(args, host).await,
        VariableSub::Set(args) => var_set(args, host).await,
        VariableSub::Delete(args) => var_delete(args, host).await,
    }
}

/// How the requested scope reads in output ("o/r" or "team").
fn scope_target(scope: &WorkflowScope) -> String {
    match scope {
        WorkflowScope::Repo(ctx) => format!("{}/{}", ctx.owner, ctx.name),
        WorkflowScope::Org { org, .. } => org.clone(),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn var_list(args: VariableListArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    let v = match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::list_variables(&ctx.client, &ctx.owner, &ctx.name).await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::list_org_variables(client, org).await?
        }
    };
    if args.json {
        return output::print_json(&serde_json::to_value(&v)?);
    }
    if v.is_empty() {
        println!("(no variables)");
        return Ok(());
    }
    print!(
        "{}",
        output::render_table(&["NAME", "VALUE", "SCOPE"], &variable_rows(&v))
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn var_set(args: VariableSetArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    let outcome = match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::set_variable(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value)
                .await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::set_org_variable(client, org, &args.name, &value).await?
        }
    };
    println!(
        "{}",
        variable_set_message(
            &outcome,
            matches!(scope, WorkflowScope::Org { .. }),
            &scope_target(&scope)
        )
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn var_delete(args: VariableDeleteArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete variable '{}' from {}? Type name to confirm",
            args.name,
            scope_target(&scope)
        ))?;
        if ans != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::delete_variable(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::delete_org_variable(client, org, &args.name).await?
        }
    }
    println!("✓ Deleted variable {}", args.name);
    Ok(())
}
