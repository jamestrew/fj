//! `fj protect` — branch protection rules.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::protect::{self, CreateBranchProtection};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct ProtectCmd {
    #[command(subcommand)]
    pub command: ProtectSub,
}

#[derive(Debug, Subcommand)]
pub enum ProtectSub {
    /// List branch protection rules.
    List(ListArgs),
    /// Show a single rule.
    View(BranchArgs),
    /// Create or update a rule.
    Set(SetArgs),
    /// Delete a rule.
    Delete(BranchArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct BranchArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub branch: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub branch: String,
    /// Require signed commits.
    #[arg(long)]
    pub require_signed: bool,
    /// Block merges from outdated branches.
    #[arg(long)]
    pub block_outdated: bool,
    /// Minimum required approvals.
    #[arg(long, default_value_t = 0)]
    pub required_approvals: i64,
    /// Disable direct pushes.
    #[arg(long)]
    pub no_push: bool,
    /// Required status check context (repeatable). Enables status checks.
    #[arg(long = "status-check", value_name = "CONTEXT")]
    pub status_check: Vec<String>,
}

pub async fn run(cmd: ProtectCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        ProtectSub::List(args) => list(args, host).await,
        ProtectSub::View(args) => view(args, host).await,
        ProtectSub::Set(args) => set(args, host).await,
        ProtectSub::Delete(args) => delete(args, host).await,
    }
}

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let items = protect::list(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no protection rules)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|p| {
            vec![
                p.branch_name.clone(),
                p.required_approvals.to_string(),
                p.require_signed_commits.to_string(),
                p.block_on_outdated_branch.to_string(),
                p.enable_push.to_string(),
                if p.enable_status_check {
                    if p.status_check_contexts.is_empty() {
                        "true".to_string()
                    } else {
                        p.status_check_contexts.join(",")
                    }
                } else {
                    "false".to_string()
                },
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(
            &[
                "BRANCH",
                "APPROVALS",
                "SIGNED",
                "BLOCK_OUTDATED",
                "PUSH",
                "STATUS_CHECK"
            ],
            &rows
        )
    );
    Ok(())
}

async fn view(args: BranchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let p = protect::get(&ctx.client, &ctx.owner, &ctx.name, &args.branch).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&p)?);
    }
    println!("{}", output::bold(&p.branch_name));
    println!("Required approvals:    {}", p.required_approvals);
    println!("Require signed:        {}", p.require_signed_commits);
    println!("Block outdated branch: {}", p.block_on_outdated_branch);
    println!("Direct push allowed:   {}", p.enable_push);
    println!("Status checks:         {}", p.enable_status_check);
    if !p.status_check_contexts.is_empty() {
        println!(
            "  Contexts:            {}",
            p.status_check_contexts.join(", ")
        );
    }
    Ok(())
}

async fn set(args: SetArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = CreateBranchProtection {
        branch_name: &args.branch,
        enable_push: !args.no_push,
        require_signed_commits: args.require_signed,
        block_on_outdated_branch: args.block_outdated,
        required_approvals: args.required_approvals,
        enable_status_check: !args.status_check.is_empty(),
        status_check_contexts: &args.status_check,
    };
    let p = protect::create(&ctx.client, &ctx.owner, &ctx.name, &body).await?;
    println!("✓ Protected {}", p.branch_name);
    Ok(())
}

async fn delete(args: BranchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    protect::delete(&ctx.client, &ctx.owner, &ctx.name, &args.branch).await?;
    println!("✓ Removed protection on {}", args.branch);
    Ok(())
}
