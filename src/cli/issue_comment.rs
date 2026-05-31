//! Comment subcommands for `fj issue`: add, edit, and delete a comment.
//! Comment ids (for edit/delete) are distinct from issue numbers and come
//! from `issue view --comments --json`.

use std::io::Read;

use anyhow::{anyhow, Result};
use clap::Args;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};

use super::editor;

#[derive(Debug, Args)]
pub struct CommentArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    /// Comment body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditCommentArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Comment id (NOT the issue number). Find it via `issue view --comments --json`.
    pub comment_id: u64,
    /// New body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteCommentArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub comment_id: u64,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn comment(args: CommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        Some(s) => s.to_string(),
        None => editor::edit_text("ISSUE_COMMENT.md", "")?,
    };
    if body.trim().is_empty() {
        return Err(anyhow!("comment body is empty"));
    }
    let c = api::issue::comment(&ctx.client, &ctx.owner, &ctx.name, args.number, &body).await?;
    println!("✓ Commented on #{} ({})", args.number, c.html_url);
    Ok(())
}

pub async fn edit_comment(args: EditCommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        Some(s) => s.to_string(),
        None => editor::edit_text("COMMENT_BODY.md", "")?,
    };
    if body.trim().is_empty() {
        return Err(anyhow!("comment body is empty"));
    }
    let c = api::issue::edit_comment(&ctx.client, &ctx.owner, &ctx.name, args.comment_id, &body)
        .await?;
    println!("✓ Updated comment #{} ({})", c.id, c.html_url);
    Ok(())
}

pub async fn delete_comment(args: DeleteCommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = editor::prompt_line(&format!(
            "Delete comment {}? Type the id to confirm",
            args.comment_id
        ))?;
        if ans != args.comment_id.to_string() {
            return Err(anyhow!("aborted"));
        }
    }
    api::issue::delete_comment(&ctx.client, &ctx.owner, &ctx.name, args.comment_id).await?;
    println!("✓ Deleted comment {}", args.comment_id);
    Ok(())
}
