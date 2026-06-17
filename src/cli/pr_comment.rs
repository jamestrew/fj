//! Comment subcommands for `fj pr`: add (the default), edit, and delete.
//! In Forgejo a PR is an issue, so every comment mutation reuses the issue
//! comment endpoints (`api::issue::comment`/`edit_comment`/`delete_comment`)
//! against the shared `/repos/{owner}/{repo}/issues/comments/{id}` path.
//!
//! The `--edit-last` / `--create-if-none` / `--create-or-edit` flags turn the
//! add form into the gh-parity idempotent upsert that the maintained
//! status-comment pattern needs: a bot posts "build running" once, then edits
//! that same comment to "build green" without juggling the comment id itself.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::issue::Comment;
use crate::cli::context::{resolve_repo, RepoFlag};

use super::editor;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct CommentCmd {
    #[command(subcommand)]
    pub action: Option<CommentAction>,
    /// Add a comment (the default when no subcommand is given).
    #[command(flatten)]
    pub add: AddArgs,
}

#[derive(Debug, Subcommand)]
pub enum CommentAction {
    /// Edit a comment by id (ids come from `pr view --comments`).
    Edit(EditArgs),
    /// Delete a comment by id.
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Comment body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(
        short = 'F',
        long = "body-file",
        value_name = "FILE",
        conflicts_with = "body"
    )]
    pub body_file: Option<String>,
    /// Edit your most recent comment on the PR instead of posting a new one.
    /// Errors if you have no comment yet (unless --create-if-none).
    #[arg(long)]
    pub edit_last: bool,
    /// With --edit-last, post a new comment when you have none yet.
    #[arg(long)]
    pub create_if_none: bool,
    /// Edit your most recent comment, or post one when none exists.
    /// Shorthand for `--edit-last --create-if-none`.
    #[arg(long)]
    pub create_or_edit: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Comment id (NOT the PR number). Find it via `pr view --comments`.
    pub comment_id: u64,
    /// New body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(
        short = 'F',
        long = "body-file",
        value_name = "FILE",
        conflicts_with = "body"
    )]
    pub body_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Comment id
    pub comment_id: u64,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: CommentCmd, host: Option<&str>) -> Result<()> {
    match cmd.action {
        Some(CommentAction::Edit(args)) => edit(args, host).await,
        Some(CommentAction::Delete(args)) => delete(args, host).await,
        None => add(cmd.add, host).await,
    }
}

/// Most recent comment authored by `login`, or `None` if the user has not
/// commented. Forgejo returns comments oldest-first, but we pick by the
/// monotonic comment id rather than trusting list order.
pub fn last_own_comment<'a>(comments: &'a [Comment], login: &str) -> Option<&'a Comment> {
    comments
        .iter()
        .filter(|c| c.user.login == login)
        .max_by_key(|c| c.id)
}

fn read_body(body: Option<&str>, body_file: Option<&str>) -> Result<String> {
    let text = editor::resolve_body_input(body, body_file, Some(("PR_COMMENT.md", "")))?
        .ok_or_else(|| anyhow!("comment body is empty"))?;
    if text.trim().is_empty() {
        return Err(anyhow!("comment body is empty"));
    }
    Ok(text)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn add(args: AddArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let create_if_none = args.create_if_none || args.create_or_edit;
    let edit_last = args.edit_last || create_if_none;

    if edit_last {
        let me = api::user::current(&ctx.client).await?;
        let comments =
            api::issue::list_comments(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
        if let Some(existing) = last_own_comment(&comments, &me.login) {
            let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
            let c =
                api::issue::edit_comment(&ctx.client, &ctx.owner, &ctx.name, existing.id, &body)
                    .await?;
            println!(
                "✓ Updated comment {} on #{} ({})",
                c.id, args.number, c.html_url
            );
            return Ok(());
        }
        if !create_if_none {
            return Err(anyhow!(
                "no comment by {} on #{} to edit; pass --create-if-none (or use --create-or-edit) to post one",
                me.login,
                args.number
            ));
        }
        // No existing comment but creation is allowed: fall through to post.
    }

    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    let c = api::issue::comment(&ctx.client, &ctx.owner, &ctx.name, args.number, &body).await?;
    println!("✓ Commented on #{} ({})", args.number, c.html_url);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = read_body(args.body.as_deref(), args.body_file.as_deref())?;
    let c = api::issue::edit_comment(&ctx.client, &ctx.owner, &ctx.name, args.comment_id, &body)
        .await?;
    println!("✓ Updated comment {} ({})", c.id, c.html_url);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::pr::{PrCmd, PrSub};
    use crate::cli::{Cli, Command};
    use chrono::Utc;
    use clap::Parser;
    use std::io::Write;

    fn comment(id: u64, login: &str) -> Comment {
        Comment {
            id,
            body: String::new(),
            user: crate::api::user::User {
                id: 1,
                login: login.to_string(),
                full_name: String::new(),
                email: String::new(),
                avatar_url: String::new(),
                is_admin: false,
            },
            html_url: String::new(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn last_own_comment_picks_highest_id_for_login() {
        let comments = vec![comment(1, "alice"), comment(5, "bob"), comment(3, "alice")];
        let picked = last_own_comment(&comments, "alice").expect("alice has comments");
        assert_eq!(picked.id, 3);
    }

    #[test]
    fn last_own_comment_none_when_login_absent() {
        let comments = vec![comment(1, "alice"), comment(2, "bob")];
        assert!(last_own_comment(&comments, "carol").is_none());
    }

    #[test]
    fn last_own_comment_none_on_empty() {
        assert!(last_own_comment(&[], "alice").is_none());
    }

    #[test]
    fn add_comment_parses_body_file_and_stdin() {
        let cli = Cli::try_parse_from([
            "fj",
            "pr",
            "comment",
            "33",
            "--body-file",
            "path/to/body.md",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Comment(cmd),
            }) => match cmd.action {
                None => {
                    assert_eq!(cmd.add.body_file.as_deref(), Some("path/to/body.md"));
                    assert_eq!(cmd.add.body, None);
                }
                Some(_) => panic!("expected default add action"),
            },
            other => panic!("expected pr comment add, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "pr", "comment", "33", "--body", "-"])
            .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Comment(cmd),
            }) => match cmd.action {
                None => assert_eq!(cmd.add.body.as_deref(), Some("-")),
                Some(_) => panic!("expected default add action"),
            },
            other => panic!("expected pr comment add, got {other:?}"),
        }
    }

    #[test]
    fn add_comment_rejects_body_and_body_file_together() {
        let err = Cli::try_parse_from([
            "fj",
            "pr",
            "comment",
            "33",
            "--body",
            "inline",
            "--body-file",
            "path/to/body.md",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn edit_comment_reads_body_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"edited comment body\n").unwrap();

        let body = read_body(None, Some(file.path().to_str().unwrap())).unwrap();
        assert_eq!(body, "edited comment body");
    }
}
