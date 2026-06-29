//! `fj variable` — Forgejo Actions variables (list/set/delete).

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::workflow_secret::{
    read_value, resolve_workflow_scope, SecretDeleteArgs, SecretListArgs, SecretSetArgs,
    WorkflowScope,
};
use crate::output;

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
    let rows: Vec<Vec<String>> = v
        .iter()
        .map(|x| vec![x.name.clone(), output::relative_time(x.created_at)])
        .collect();
    print!("{}", output::render_table(&["NAME", "CREATED"], &rows));
    Ok(())
}

async fn var_set(args: SecretSetArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::set_variable(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value)
                .await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::set_org_variable(client, org, &args.name, &value).await?
        }
    }
    println!("✓ Set variable {}", args.name);
    Ok(())
}

async fn var_delete(args: SecretDeleteArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete variable '{}'? Type name to confirm",
            args.name
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
