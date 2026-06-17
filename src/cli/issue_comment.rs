//! Comment subcommands for `fj issue`: add, edit, and delete a comment.
//! Comment ids (for edit/delete) are distinct from issue numbers and come
//! from `issue view --comments --json`.

use anyhow::{anyhow, Result};
use clap::Args;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};

use super::editor;

#[derive(Debug, Args)]
pub struct CommentArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
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
pub struct DeleteCommentArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Comment id
    pub comment_id: u64,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn comment(args: CommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = comment_body(
        args.body.as_deref(),
        args.body_file.as_deref(),
        "ISSUE_COMMENT.md",
    )?;
    let c = api::issue::comment(&ctx.client, &ctx.owner, &ctx.name, args.number, &body).await?;
    println!("✓ Commented on #{} ({})", args.number, c.html_url);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn edit_comment(args: EditCommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = comment_body(
        args.body.as_deref(),
        args.body_file.as_deref(),
        "COMMENT_BODY.md",
    )?;
    let c = api::issue::edit_comment(&ctx.client, &ctx.owner, &ctx.name, args.comment_id, &body)
        .await?;
    println!("✓ Updated comment #{} ({})", c.id, c.html_url);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

fn comment_body(
    body: Option<&str>,
    body_file: Option<&str>,
    template_name: &str,
) -> Result<String> {
    let body = editor::resolve_body_input(body, body_file, Some((template_name, "")))?
        .ok_or_else(|| anyhow!("comment body is empty"))?;
    if body.trim().is_empty() {
        Err(anyhow!("comment body is empty"))
    } else {
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::issue::{IssueCmd, IssueSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn comment_parses_body_file_and_stdin_markers() {
        let cli = Cli::try_parse_from([
            "fj",
            "issue",
            "comment",
            "12",
            "--body-file",
            "path/to/body.md",
        ])
        .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Comment(args),
            }) => {
                assert_eq!(args.body, None);
                assert_eq!(args.body_file.as_deref(), Some("path/to/body.md"));
            }
            other => panic!("expected issue comment, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "issue", "comment", "12", "--body", "-"])
            .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Comment(args),
            }) => {
                assert_eq!(args.body.as_deref(), Some("-"));
            }
            other => panic!("expected issue comment, got {other:?}"),
        }
    }

    #[test]
    fn comment_rejects_body_and_body_file_together() {
        let err = Cli::try_parse_from([
            "fj",
            "issue",
            "comment",
            "12",
            "--body",
            "inline",
            "--body-file",
            "path/to/body.md",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn edit_comment_parses_body_file() {
        let cli = Cli::try_parse_from(["fj", "issue", "edit-comment", "55", "--body-file", "-"])
            .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::EditComment(args),
            }) => {
                assert_eq!(args.body_file.as_deref(), Some("-"));
                assert_eq!(args.body, None);
            }
            other => panic!("expected issue edit-comment, got {other:?}"),
        }
    }
}
