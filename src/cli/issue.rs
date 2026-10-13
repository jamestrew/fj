use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::{CreateIssue, EditIssue, ListOptions, State};
use crate::api::template::TemplateKind;
use crate::cli::context::{repo_web_url, resolve_repo, RepoFlag};
use crate::cli::create_template;
use crate::cli::issue_comment::{self, CommentArgs, DeleteCommentArgs, EditCommentArgs};
use crate::cli::issue_dependency::{self, DependencyCmd};
use crate::cli::issue_reaction::{self, ReactionCmd};
use crate::cli::issue_time::{self, TimeCmd};
use crate::cli::list as list_output;
use crate::client::{ApiError, Page};
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
    /// Lock an issue conversation.
    Lock(LockArgs),
    /// Unlock an issue conversation.
    Unlock(NumberOnly),
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
    /// React to an issue, a pull request, or a comment, and see the tally.
    Reaction(ReactionCmd),
    /// Show and change what an issue or pull request depends on and blocks.
    Dependency(DependencyCmd),
    /// Show, log, and delete tracked time on an issue or pull request.
    Time(TimeCmd),
}

/// Orders the forge's issue list endpoint honors. The names are the forge's
/// own `sort` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortArg {
    /// Most recently updated first
    Recentupdate,
    /// Least recently updated first
    Leastupdate,
    /// Newest first
    Latest,
    /// Oldest first
    Oldest,
    /// Most commented first
    Mostcomment,
    /// Least commented first
    Leastcomment,
    /// Nearest due date first
    Nearduedate,
    /// Farthest due date first
    Farduedate,
}

impl SortArg {
    pub fn as_str(self) -> &'static str {
        match self {
            SortArg::Recentupdate => "recentupdate",
            SortArg::Leastupdate => "leastupdate",
            SortArg::Latest => "latest",
            SortArg::Oldest => "oldest",
            SortArg::Mostcomment => "mostcomment",
            SortArg::Leastcomment => "leastcomment",
            SortArg::Nearduedate => "nearduedate",
            SortArg::Farduedate => "farduedate",
        }
    }
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
    /// Filter by milestone name or id
    #[arg(long)]
    pub milestone: Option<String>,
    /// Filter by mentioned user login
    #[arg(long)]
    pub mentioned: Option<String>,
    /// Search text to filter by
    #[arg(short = 'S', long = "search")]
    pub query: Option<String>,
    /// Order the list (omit for the forge's default order)
    #[arg(long, value_enum, value_name = "ORDER")]
    pub sort: Option<SortArg>,
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
    /// Preload the issue body from a repository issue template.
    #[arg(long, value_name = "NAME", num_args = 0..=1)]
    pub template: Option<Option<String>>,
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
    /// Milestone title or id to file the issue under.
    #[arg(short = 'm', long, value_name = "TITLE|ID")]
    pub milestone: Option<String>,
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
pub struct LockArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue number
    pub number: u64,
    /// Reason for locking the conversation.
    #[arg(long, value_enum)]
    pub reason: Option<LockReasonArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LockReasonArg {
    OffTopic,
    TooHeated,
    Resolved,
    Spam,
}

impl LockReasonArg {
    pub fn as_str(self) -> &'static str {
        match self {
            LockReasonArg::OffTopic => "off-topic",
            LockReasonArg::TooHeated => "too-heated",
            LockReasonArg::Resolved => "resolved",
            LockReasonArg::Spam => "spam",
        }
    }
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
        IssueSub::Lock(args) => lock(args, host).await,
        IssueSub::Unlock(args) => unlock(args, host).await,
        IssueSub::Delete(args) => delete(args, host).await,
        IssueSub::Pin(args) => set_pinned(args, host, true).await,
        IssueSub::Unpin(args) => set_pinned(args, host, false).await,
        IssueSub::Comment(args) => issue_comment::comment(args, host).await,
        IssueSub::EditComment(args) => issue_comment::edit_comment(args, host).await,
        IssueSub::DeleteComment(args) => issue_comment::delete_comment(args, host).await,
        IssueSub::Develop(args) => develop(args, host).await,
        IssueSub::Reaction(cmd) => issue_reaction::run(cmd, host).await,
        IssueSub::Dependency(cmd) => issue_dependency::run(cmd, host).await,
        IssueSub::Time(cmd) => issue_time::run(cmd, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&repo_web_url(&ctx, "issues")?);
    }
    if let Some(milestone) = args
        .milestone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let milestones = api::milestone::list(&ctx.client, &ctx.owner, &ctx.name, "all").await?;
        let known = milestones
            .iter()
            .any(|m| m.title == milestone || m.id.to_string() == milestone);
        if !known {
            if !args.json {
                println!("(no issues: no milestone named '{milestone}')");
            }
            let page: Page<api::issue::Issue> = Page::single(Vec::new());
            return if args.json {
                list_output::print_json(&page)
            } else {
                Ok(())
            };
        }
    }
    let opts = ListOptions {
        state: args.state.into(),
        limit: args.limit,
        page: args.page,
        paginate: args.paginate,
        labels: args.label.as_deref(),
        assignee: args.assignee.as_deref(),
        milestone: args.milestone.as_deref(),
        mentioned: args.mentioned.as_deref(),
        query: args.query.as_deref(),
        sort: args.sort.map(SortArg::as_str),
    };
    let page = match api::issue::list(&ctx.client, &ctx.owner, &ctx.name, opts).await {
        Ok(page) => page,
        Err(err)
            if args.mentioned.is_some()
                && err
                    .downcast_ref::<ApiError>()
                    .is_some_and(|e| e.status == reqwest::StatusCode::NOT_FOUND) =>
        {
            if !args.json {
                let mentioned = args.mentioned.as_deref().unwrap_or_default();
                println!("(no issues: no user named '{mentioned}')");
            }
            Page::single(Vec::new())
        }
        Err(err) => {
            return Err(api::issue_tracker::explain_not_found(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                err,
            )
            .await)
        }
    };
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
    let template_body = create_template::resolve(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        TemplateKind::Issue,
        args.template,
    )
    .await?;
    let body = editor::resolve_body_input(
        args.body.as_deref(),
        args.body_file.as_deref(),
        Some(("ISSUE_BODY.md", template_body.as_deref().unwrap_or(""))),
    )?;
    let label_names = clean_csv_values(args.label);
    let requested_labels = if label_names.is_empty() {
        Vec::new()
    } else {
        api::label::resolve_names(&ctx.client, &ctx.owner, &ctx.name, &label_names).await?
    };
    let labels =
        (!requested_labels.is_empty()).then(|| requested_labels.iter().map(|l| l.id).collect());
    let assignees = clean_csv_values(args.assignee);
    let assignees = (!assignees.is_empty()).then_some(assignees);
    // Resolved before the create so a bad name never leaves an issue behind,
    // and sent in the same request so there is no second step to fail.
    let milestone = match args.milestone.as_deref().map(str::trim) {
        Some("") => return Err(anyhow!("--milestone needs a title or an id")),
        Some(wanted) => {
            Some(api::milestone::resolve(&ctx.client, &ctx.owner, &ctx.name, wanted).await?)
        }
        None => None,
    };
    let payload = CreateIssue {
        title: &title,
        body: body.as_deref(),
        assignees,
        labels,
        milestone: milestone.as_ref().map(|m| m.id),
    };
    let issue = match api::issue::create(&ctx.client, &ctx.owner, &ctx.name, &payload).await {
        Ok(issue) => issue,
        Err(err) => {
            return Err(api::issue_tracker::explain_not_found(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                err,
            )
            .await)
        }
    };
    println!("✓ Created issue #{}: {}", issue.number, issue.title);
    for line in crate::cli::label_assign::explain_created(&requested_labels, &issue.labels) {
        println!("{line}");
    }
    println!("{}", issue.html_url);
    if let Some(wanted) = &milestone {
        println!(
            "{}",
            milestone_line(
                issue.milestone.as_ref().map(|m| m.title.as_str()),
                &wanted.title
            )
        );
    }
    if args.web {
        web::open(&issue.html_url)?;
    }
    Ok(())
}

/// What to say about the milestone after a create: the one the forge
/// reports, or a warning when it reports none for the one we sent.
fn milestone_line(confirmed: Option<&str>, wanted: &str) -> String {
    match confirmed {
        Some(title) => format!("Milestone: {title}"),
        None => format!(
            "warning: the forge did not report milestone '{wanted}' on the new issue; check it with `fj issue view`"
        ),
    }
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
        crate::cli::label_assign::add_names(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            &add_labels,
        )
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
async fn lock(args: LockArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let issue = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if issue.is_locked {
        println!("Issue #{} is already locked", args.number);
        return Ok(());
    }
    api::issue::lock(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        args.reason.map(LockReasonArg::as_str),
    )
    .await?;
    println!("✓ Locked issue #{}", args.number);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn unlock(args: NumberOnly, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let issue = api::issue::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if !issue.is_locked {
        println!("Issue #{} is already unlocked", args.number);
        return Ok(());
    }
    api::issue::unlock(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ Unlocked issue #{}", args.number);
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
    fn list_parses_milestone_and_mentioned_filters() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj",
            "issue",
            "list",
            "--milestone",
            "v1",
            "--mentioned",
            "alice",
        ])
        .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::List(a),
            }) => {
                assert_eq!(a.milestone.as_deref(), Some("v1"));
                assert_eq!(a.mentioned.as_deref(), Some("alice"));
            }
            other => panic!("expected issue list, got {other:?}"),
        }
    }

    #[test]
    fn list_parses_every_sort_order_to_its_wire_value() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        for name in [
            "recentupdate",
            "leastupdate",
            "latest",
            "oldest",
            "mostcomment",
            "leastcomment",
            "nearduedate",
            "farduedate",
        ] {
            let cli =
                Cli::try_parse_from(["fj", "issue", "list", "--sort", name]).expect("should parse");
            match cli.command {
                Command::Issue(IssueCmd {
                    command: IssueSub::List(a),
                }) => assert_eq!(a.sort.map(SortArg::as_str), Some(name)),
                other => panic!("expected issue list, got {other:?}"),
            }
        }
        // `relevance` only means something to a search, and an unknown order
        // would silently fall back to the default on the forge.
        for bad in ["relevance", "priority", "newest"] {
            assert!(Cli::try_parse_from(["fj", "issue", "list", "--sort", bad]).is_err());
        }
        let cli = Cli::try_parse_from(["fj", "issue", "list"]).expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::List(a),
            }) => assert!(a.sort.is_none()),
            other => panic!("expected issue list, got {other:?}"),
        }
    }

    #[test]
    fn create_parses_milestone() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        for flag in ["--milestone", "-m"] {
            let cli = Cli::try_parse_from(["fj", "issue", "create", "-t", "x", flag, "v1.0"])
                .expect("should parse");
            match cli.command {
                Command::Issue(IssueCmd {
                    command: IssueSub::Create(a),
                }) => assert_eq!(a.milestone.as_deref(), Some("v1.0")),
                other => panic!("expected issue create, got {other:?}"),
            }
        }
    }

    #[test]
    fn milestone_line_reports_what_the_forge_confirmed() {
        assert_eq!(milestone_line(Some("v1.0"), "v1.0"), "Milestone: v1.0");
        let warning = milestone_line(None, "v1.0");
        assert!(warning.starts_with("warning:") && warning.contains("'v1.0'"));
    }

    #[test]
    fn create_parses_template_with_and_without_name() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "issue", "create", "--template", "bug"])
            .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Create(a),
            }) => assert_eq!(a.template, Some(Some("bug".into()))),
            other => panic!("expected issue create, got {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["fj", "issue", "create", "--template"]).expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Create(a),
            }) => assert_eq!(a.template, Some(None)),
            other => panic!("expected issue create, got {other:?}"),
        }
    }

    #[test]
    fn lock_and_unlock_parse() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "issue", "lock", "7", "--reason", "too-heated"])
            .expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Lock(a),
            }) => {
                assert_eq!(a.number, 7);
                assert!(matches!(a.reason, Some(LockReasonArg::TooHeated)));
            }
            other => panic!("expected issue lock, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "issue", "unlock", "7"]).expect("should parse");
        match cli.command {
            Command::Issue(IssueCmd {
                command: IssueSub::Unlock(a),
            }) => assert_eq!(a.number, 7),
            other => panic!("expected issue unlock, got {other:?}"),
        }
    }

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
