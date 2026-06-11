use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::{CreateIssue, EditIssue, ListOptions, State};
use crate::cli::context::{repo_web_url, resolve_repo, RepoFlag};
use crate::cli::issue_comment::{self, CommentArgs, DeleteCommentArgs, EditCommentArgs};
use crate::cli::list as list_output;
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
    /// Create an issue with optional labels and assignees.
    Create(CreateArgs),
    /// Edit an issue's title/body or add/remove labels and assignees.
    Edit(EditArgs),
    /// Close an issue.
    Close(NumberOnly),
    /// Reopen a closed issue.
    Reopen(NumberOnly),
    /// Delete an issue permanently (requires repo admin).
    Delete(DeleteArgs),
    /// Pin an issue to the top of the issue list.
    Pin(NumberOnly),
    /// Unpin a pinned issue.
    Unpin(NumberOnly),
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
    /// Filter by state
    #[arg(short = 's', long, value_enum, default_value_t = StateFilter::Open)]
    pub state: StateFilter,
    /// Maximum number of issues to fetch (default 30; results may be capped unless --paginate)
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Fetch all pages instead of stopping at --limit
    #[arg(long)]
    pub paginate: bool,
    /// Page of results to fetch
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// Filter by label
    #[arg(short = 'l', long)]
    pub label: Option<String>,
    /// Filter by assignee
    #[arg(short = 'a', long)]
    pub assignee: Option<String>,
    /// Search text to filter by
    #[arg(short = 'S', long = "search")]
    pub query: Option<String>,
    /// Emit JSON instead of a table
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
    /// Issue number
    pub number: u64,
    /// Include the comment thread
    #[arg(long, default_value_t = false)]
    pub comments: bool,
    /// Emit JSON instead of a table
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
    /// Issue title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(short = 'F', long = "body-file", value_name = "FILE")]
    pub body_file: Option<String>,
    /// Label name to add. Repeat or use commas for multiple labels.
    #[arg(
        short = 'l',
        long = "label",
        value_name = "NAME",
        value_delimiter = ','
    )]
    pub label: Vec<String>,
    /// Username to assign. Repeat or use commas for multiple assignees.
    #[arg(
        short = 'a',
        long = "assignee",
        value_name = "USERNAME",
        value_delimiter = ','
    )]
    pub assignee: Vec<String>,
    /// Open the new issue in your browser after creating.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
    pub number: u64,
    /// Issue title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// New body. Use `-` to read from stdin. Pass `--body-editor` to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(short = 'F', long = "body-file", value_name = "FILE")]
    pub body_file: Option<String>,
    /// Open `$EDITOR` to edit the body inline (overrides `--body`).
    #[arg(long)]
    pub body_editor: bool,
    /// Label name to add. Repeat or use commas for multiple labels.
    #[arg(long = "add-label", value_name = "NAME", value_delimiter = ',')]
    pub add_label: Vec<String>,
    /// Label name to remove. Repeat or use commas for multiple labels.
    #[arg(long = "remove-label", value_name = "NAME", value_delimiter = ',')]
    pub remove_label: Vec<String>,
    /// Username to assign. Repeat or use commas for multiple assignees.
    #[arg(long = "add-assignee", value_name = "USERNAME", value_delimiter = ',')]
    pub add_assignee: Vec<String>,
    /// Username to unassign. Repeat or use commas for multiple assignees.
    #[arg(
        long = "remove-assignee",
        value_name = "USERNAME",
        value_delimiter = ','
    )]
    pub remove_assignee: Vec<String>,
}

#[derive(Debug, Args)]
pub struct NumberOnly {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
    pub number: u64,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
    pub number: u64,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct DevelopArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
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

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: IssueCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        IssueSub::List(args) => list(args, host).await,
        IssueSub::View(args) => view(args, host).await,
        IssueSub::Create(args) => create(args, host).await,
        IssueSub::Edit(args) => edit(args, host).await,
        IssueSub::Close(args) => set_state(args, host, "closed").await,
        IssueSub::Reopen(args) => set_state(args, host, "open").await,
        IssueSub::Delete(args) => delete(args, host).await,
        IssueSub::Pin(args) => set_pinned(args, host, true).await,
        IssueSub::Unpin(args) => set_pinned(args, host, false).await,
        IssueSub::Comment(args) => issue_comment::comment(args, host).await,
        IssueSub::EditComment(args) => issue_comment::edit_comment(args, host).await,
        IssueSub::DeleteComment(args) => issue_comment::delete_comment(args, host).await,
        IssueSub::Develop(args) => develop(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&repo_web_url(&ctx, "issues")?);
    }
    let opts = ListOptions {
        state: args.state.into(),
        limit: args.limit,
        page: args.page,
        paginate: args.paginate,
        labels: args.label.as_deref(),
        assignee: args.assignee.as_deref(),
        query: args.query.as_deref(),
    };
    let page = api::issue::list(&ctx.client, &ctx.owner, &ctx.name, opts).await?;
    if args.json {
        return list_output::print_json(&page);
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
                output::truncate(&i.title, 60),
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
    if !args.paginate {
        list_output::print_truncation_footer(&page, args.limit);
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let title = match args.title {
        Some(t) => t,
        None => editor::prompt_line("Title")?,
    };
    if title.trim().is_empty() {
        return Err(anyhow!("title is required"));
    }
    let body = editor::resolve_body_input(
        args.body.as_deref(),
        args.body_file.as_deref(),
        Some(("ISSUE_BODY.md", "")),
    )?;
    let label_names = clean_csv_values(args.label);
    let labels = if label_names.is_empty() {
        None
    } else {
        Some(
            api::label::resolve_names_to_ids(&ctx.client, &ctx.owner, &ctx.name, &label_names)
                .await?,
        )
    };
    let assignees = clean_csv_values(args.assignee);
    let assignees = (!assignees.is_empty()).then_some(assignees);
    let payload = CreateIssue {
        title: &title,
        body: body.as_deref(),
        assignees,
        labels,
    };
    let issue = api::issue::create(&ctx.client, &ctx.owner, &ctx.name, &payload).await?;
    println!("✓ Created issue #{}: {}", issue.number, issue.title);
    println!("{}", issue.html_url);
    if args.web {
        web::open(&issue.html_url)?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn clean_csv_values(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let add_labels = clean_csv_values(args.add_label);
    let remove_labels = clean_csv_values(args.remove_label);
    let add_assignees = clean_csv_values(args.add_assignee);
    let remove_assignees = clean_csv_values(args.remove_assignee);
    let has_additive_edits = !add_labels.is_empty()
        || !remove_labels.is_empty()
        || !add_assignees.is_empty()
        || !remove_assignees.is_empty();
    let should_patch = args.title.is_some()
        || args.body.is_some()
        || args.body_file.is_some()
        || args.body_editor
        || !has_additive_edits;

    let mut updated_issue = None;
    if should_patch {
        let body = if args.body_editor {
            let existing = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            Some(editor::edit_text("ISSUE_BODY.md", &existing.body)?)
        } else {
            editor::resolve_body_input(args.body.as_deref(), args.body_file.as_deref(), None)?
        };
        let patch = EditIssue {
            title: args.title.as_deref(),
            body: body.as_deref(),
            state: None,
        };
        let issue =
            api::issue::edit(&ctx.client, &ctx.owner, &ctx.name, args.number, &patch).await?;
        updated_issue = Some(issue);
    }

    if !add_labels.is_empty() {
        let label_ids =
            api::label::resolve_names_to_ids(&ctx.client, &ctx.owner, &ctx.name, &add_labels)
                .await?;
        api::label::add_to_issue(&ctx.client, &ctx.owner, &ctx.name, args.number, label_ids)
            .await?;
    }
    if !remove_labels.is_empty() {
        let label_ids =
            api::label::resolve_names_to_ids(&ctx.client, &ctx.owner, &ctx.name, &remove_labels)
                .await?;
        for label_id in label_ids {
            api::label::remove_from_issue(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                args.number,
                label_id,
            )
            .await?;
        }
    }
    if !add_assignees.is_empty() {
        updated_issue = Some(
            api::issue::add_assignees(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                args.number,
                add_assignees,
            )
            .await?,
        );
    }
    if !remove_assignees.is_empty() {
        updated_issue = Some(
            api::issue::remove_assignees(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                args.number,
                remove_assignees,
            )
            .await?,
        );
    }

    if let Some(issue) = updated_issue {
        println!("✓ Updated #{}: {}", issue.number, issue.title);
    } else {
        println!("✓ Updated #{}", args.number);
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_pinned(args: NumberOnly, host: Option<&str>, pinned: bool) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::issue::set_pinned(&ctx.client, &ctx.owner, &ctx.name, args.number, pinned).await?;
    println!(
        "✓ Issue #{} {}",
        args.number,
        if pinned { "pinned" } else { "unpinned" }
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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
