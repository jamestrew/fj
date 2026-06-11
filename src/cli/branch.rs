use anyhow::Result;
use clap::Args;

use crate::api::repo::{self, CreateBranch};
use crate::cli::context::{resolve_repo, RepoFlag};

use super::editor;

#[derive(Debug, Args)]
pub struct BranchCreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Branch name to create
    pub name: String,
    /// Source branch, tag, or SHA. Defaults to the server's default branch.
    #[arg(long = "from")]
    pub from: Option<String>,
}

#[derive(Debug, Args)]
pub struct BranchDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Branch name to delete
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn create(args: BranchCreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let branch = repo::create_branch(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreateBranch {
            new_branch_name: &args.name,
            old_branch_name: args.from.as_deref(),
        },
    )
    .await?;
    println!("✓ Created branch '{}'", branch.name);
    Ok(())
}

pub async fn delete(args: BranchDeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let answer = editor::prompt_line(&format!(
            "Delete branch '{}'? Type the name to confirm",
            args.name
        ))?;
        if answer != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    repo::delete_branch(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?;
    println!("✓ Deleted branch '{}'", args.name);
    Ok(())
}
