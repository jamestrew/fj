use std::io::Read;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::{CreateIssue, EditIssue, ListOptions, State};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

use super::editor;
use super::web;

#[derive(Debug, Args)]
pub struct IssueCmd {
    #[command(subcommand)]
    pub command: IssueSub,
}

#[derive(Debug, Subcommand)]
pub enum IssueSub {
    /// List issues in a repo.
    List(ListArgs),
    /// View an issue by number.
    View(ViewArgs),
    /// Create an issue.
    Create(CreateArgs),
    /// Edit an issue's title, body, labels, or assignees.
    Edit(EditArgs),
    /// Close an issue.
    Close(NumberOnly),
    /// Reopen a closed issue.
    Reopen(NumberOnly),
    /// Delete an issue permanently (requires repo admin).
    Delete(DeleteArgs),
    /// Add a comment.
    Comment(CommentArgs),
    /// Edit a comment by id (comment ids come from `issue view --comments`).
    EditComment(EditCommentArgs),
    /// Delete a comment by id.
    DeleteComment(DeleteCommentArgs),
    /// Create a branch tied to the issue.
    Develop(DevelopArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateFilter {
    Open,
    Closed,
    All,
}

impl From<StateFilter> for State {
    fn from(value: StateFilter) -> Self {
        match value {
            StateFilter::Open => State::Open,
            StateFilter::Closed => State::Closed,
            StateFilter::All => State::All,
        }
    }
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(short = 's', long, value_enum, default_value_t = StateFilter::Open)]
    pub state: StateFilter,
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(short = 'l', long)]
    pub label: Option<String>,
    #[arg(short = 'a', long)]
    pub assignee: Option<String>,
    #[arg(short = 'S', long = "search")]
    pub query: Option<String>,
    #[arg(long)]
    pub json: bool,
    /// Open the issue list in your browser.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    #[arg(long, default_value_t = false)]
    pub comments: bool,
    #[arg(long)]
    pub json: bool,
    /// Open the issue in your browser.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Open the new issue in your browser after creating.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// New body. Use `-` to read from stdin. Pass `--body-editor` to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Open `$EDITOR` to edit the body inline (overrides `--body`).
    #[arg(long)]
    pub body_editor: bool,
}

#[derive(Debug, Args)]
pub struct NumberOnly {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

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

#[derive(Debug, Args)]
pub struct DevelopArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    /// Branch name. Defaults to `issue-<number>-<slug-of-title>`.
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Base branch to start from. Defaults to the repo's default branch.
    #[arg(long)]
    pub base: Option<String>,
    /// Just print the branch name; don't run git.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(cmd: IssueCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        IssueSub::List(args) => list(args, host).await,
        IssueSub::View(args) => view(args, host).await,
        IssueSub::Create(args) => create(args, host).await,
        IssueSub::Edit(args) => edit(args, host).await,
        IssueSub::Close(args) => set_state(args, host, "closed").await,
        IssueSub::Reopen(args) => set_state(args, host, "open").await,
        IssueSub::Delete(args) => delete(args, host).await,
        IssueSub::Comment(args) => comment(args, host).await,
        IssueSub::EditComment(args) => edit_comment(args, host).await,
        IssueSub::DeleteComment(args) => delete_comment(args, host).await,
        IssueSub::Develop(args) => develop(args, host).await,
    }
}

async fn edit_comment(args: EditCommentArgs, host: Option<&str>) -> Result<()> {
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

async fn delete_comment(args: DeleteCommentArgs, host: Option<&str>) -> Result<()> {
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

async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&format!(
            "https://{}/{}/{}/issues",
            ctx.host, ctx.owner, ctx.name
        ));
    }
    let opts = ListOptions {
        state: args.state.into(),
        limit: args.limit,
        page: args.page,
        labels: args.label.as_deref(),
        assignee: args.assignee.as_deref(),
        query: args.query.as_deref(),
    };
    let page = api::issue::list(&ctx.client, &ctx.owner, &ctx.name, opts).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&page.items)?);
    }
    if page.items.is_empty() {
        println!("(no issues)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|i| {
            let labels = i
                .labels
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            vec![
                format!("#{}", i.number),
                output::state_pill(&i.state, false),
                truncate(&i.title, 60),
                output::dim(&labels),
                output::dim(&format!("{}c", i.comments)),
                output::dim(&output::relative_time(i.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["", "STATE", "TITLE", "LABELS", "💬", "UPDATED"], &rows)
    );
    Ok(())
}

async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let issue = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if args.web {
        return web::open(&issue.html_url);
    }
    if args.json {
        let mut v = serde_json::to_value(&issue)?;
        if args.comments {
            let comments =
                api::issue::list_comments(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            v["comments_list"] = serde_json::to_value(comments)?;
        }
        return output::print_json(&v);
    }

    println!(
        "{}  {}  {}",
        output::bold(&format!("#{} {}", issue.number, issue.title)),
        output::state_pill(&issue.state, false),
        output::dim(&output::relative_time(issue.updated_at)),
    );
    println!("{}", output::dim(&issue.html_url));
    println!();
    if !issue.body.is_empty() {
        println!("{}", issue.body);
        println!();
    }
    if args.comments {
        let comments =
            api::issue::list_comments(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
        if comments.is_empty() {
            println!("{}", output::dim("(no comments)"));
        } else {
            for c in comments {
                println!(
                    "── {} {}",
                    output::bold(&c.user.login),
                    output::dim(&output::relative_time(c.created_at)),
                );
                println!("{}", c.body);
                println!();
            }
        }
    }
    Ok(())
}

async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let title = match args.title {
        Some(t) => t,
        None => editor::prompt_line("Title")?,
    };
    if title.trim().is_empty() {
        return Err(anyhow!("title is required"));
    }
    let body = editor::resolve_body(args.body.as_deref(), "ISSUE_BODY.md", "")?;
    let payload = CreateIssue {
        title: &title,
        body: body.as_deref(),
        assignees: None,
        labels: None,
    };
    let issue = api::issue::create(&ctx.client, &ctx.owner, &ctx.name, &payload).await?;
    println!("✓ Created issue #{}: {}", issue.number, issue.title);
    println!("{}", issue.html_url);
    if args.web {
        web::open(&issue.html_url)?;
    }
    Ok(())
}

async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = if args.body_editor {
        let existing = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
        Some(editor::edit_text("ISSUE_BODY.md", &existing.body)?)
    } else {
        editor::read_body(args.body.as_deref())?
    };
    let patch = EditIssue {
        title: args.title.as_deref(),
        body: body.as_deref(),
        state: None,
    };
    let issue = api::issue::edit(&ctx.client, &ctx.owner, &ctx.name, args.number, &patch).await?;
    println!("✓ Updated #{}: {}", issue.number, issue.title);
    Ok(())
}

async fn set_state(args: NumberOnly, host: Option<&str>, state: &str) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let issue = api::issue::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        &EditIssue {
            state: Some(state),
            ..Default::default()
        },
    )
    .await?;
    println!(
        "✓ Issue #{} is now {}",
        issue.number,
        output::state_pill(&issue.state, false)
    );
    Ok(())
}

async fn delete(args: DeleteArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if !args.yes {
        let ans = editor::prompt_line(&format!(
            "Delete issue #{}? This cannot be undone. Type the number to confirm",
            args.number
        ))?;
        if ans != args.number.to_string() {
            return Err(anyhow!("aborted"));
        }
    }
    api::issue::delete(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ Deleted issue #{}", args.number);
    Ok(())
}

async fn comment(args: CommentArgs, host: Option<&str>) -> Result<()> {
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

async fn develop(args: DevelopArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let issue = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let branch = match args.name {
        Some(n) => n,
        None => format!("issue-{}-{}", issue.number, slugify(&issue.title)),
    };
    let base = match args.base {
        Some(b) => b,
        None => {
            api::repo::get(&ctx.client, &ctx.owner, &ctx.name)
                .await?
                .default_branch
        }
    };
    println!("Branch: {branch}");
    println!("From:   {base}");
    if args.dry_run {
        return Ok(());
    }
    crate::git::run(&["fetch", "origin", &base])?;
    crate::git::run(&["checkout", "-b", &branch, &format!("origin/{base}")])?;
    Ok(())
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("issue");
    }
    out.chars().take(40).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slugify("Fix login bug"), "fix-login-bug");
    }

    #[test]
    fn slug_strips_repeated_separators() {
        assert_eq!(slugify("a -- b"), "a-b");
    }

    #[test]
    fn slug_falls_back_for_empty() {
        assert_eq!(slugify("???"), "issue");
    }

    #[test]
    fn slug_caps_length() {
        let long = "x".repeat(100);
        assert!(slugify(&long).len() <= 40);
    }
}
