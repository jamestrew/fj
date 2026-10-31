use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Args, Subcommand};

use crate::api;
use crate::api::pull::{CreatePull, EditPull, ListOptions};
use crate::api::template::TemplateKind;
use crate::cli::context::{repo_web_url, resolve_repo, RepoFlag};
use crate::cli::create_template;
use crate::cli::issue::LockReasonArg;
use crate::cli::pr_args::{MergeStyleArg, ReviewEventArg, StateFilter, UpdateStyle};
use crate::cli::pr_fill::{self, FillMode};
use crate::cli::pr_status::StatusArgs;
use crate::cli::{
    list as list_output, pr_comment, pr_inspect, pr_merge_check, pr_preflight, pr_review, pr_status,
};
use crate::git;
use crate::output;

use super::editor;
use super::web;

#[derive(Debug, Args)]
pub struct PrCmd {
    #[command(subcommand)]
    pub command: PrSub,
}

#[derive(Debug, Subcommand)]
pub enum PrSub {
    /// List pull requests.
    List(ListArgs),
    /// View a pull request.
    View(ViewArgs),
    /// Create a pull request.
    Create(CreateArgs),
    /// Edit a pull request.
    Edit(EditArgs),
    /// Show the unified diff for a pull request. With `--json` the diff text
    /// is wrapped as `{ "diff": "..." }`.
    Diff(InspectArgs),
    /// List commits in a pull request.
    Commits(InspectArgs),
    /// Show the file change list for a pull request.
    Files(InspectArgs),
    /// Show CI / commit status checks for a pull request's head SHA.
    Checks(InspectArgs),
    /// Convert a draft pull request to ready-for-review.
    Ready(SimpleArgs),
    /// Add, edit, or delete a comment on a pull request.
    Comment(pr_comment::CommentCmd),
    /// Upload file attachments and link them in the PR description.
    Attach(AttachArgs),
    /// Update the PR branch by merging (or rebasing) its base branch in.
    UpdateBranch(UpdateBranchArgs),
    /// Submit a review (approve / request changes / comment).
    Review(ReviewArgs),
    /// Request reviews from specific users.
    RequestReview(ReviewerArgs),
    /// Remove pending reviewer requests.
    UnrequestReview(ReviewerArgs),
    /// Cross-repo dashboard of your PRs: created, review-requested, mentions.
    Status(StatusArgs),
    /// Check out a pull request locally.
    Checkout(CheckoutArgs),
    /// Merge a pull request.
    Merge(MergeArgs),
    /// Close a pull request without merging.
    Close(SimpleArgs),
    /// Reopen a closed pull request.
    Reopen(SimpleArgs),
    /// Lock a pull request conversation.
    Lock(LockArgs),
    /// Unlock a pull request conversation.
    Unlock(SimpleArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Filter by state
    #[arg(short = 's', long, value_enum, default_value_t = StateFilter::Open)]
    pub state: StateFilter,
    /// Maximum number of pull requests to fetch (default 30; results may be capped unless --paginate)
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Fetch all pages instead of stopping at --limit
    #[arg(long)]
    pub paginate: bool,
    /// Page of results to fetch
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// Filter by label. Repeat or use commas for multiple (matches any).
    #[arg(
        short = 'l',
        long = "label",
        value_name = "NAME",
        value_delimiter = ','
    )]
    pub label: Vec<String>,
    /// Filter by author (PR creator) login.
    #[arg(short = 'A', long = "author", value_name = "USERNAME")]
    pub author: Option<String>,
    /// Filter by base branch.
    #[arg(short = 'B', long = "base", value_name = "BRANCH")]
    pub base: Option<String>,
    /// Filter by head branch.
    #[arg(short = 'H', long = "head", value_name = "BRANCH")]
    pub head: Option<String>,
    /// Filter to draft pull requests.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_draft")]
    pub draft: bool,
    /// Filter to ready pull requests.
    #[arg(long = "no-draft", action = ArgAction::SetTrue)]
    pub no_draft: bool,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Open the pull request list in your browser
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Include the comment thread
    #[arg(long, default_value_t = false)]
    pub comments: bool,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
    /// Open the pull request in your browser
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Base branch the PR targets. Defaults to the repository default branch.
    #[arg(short = 'B', long)]
    pub base: Option<String>,
    /// Head branch. Either a branch name on the same repo, or `owner:branch`.
    #[arg(short = 'H', long)]
    pub head: Option<String>,
    /// Body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(short = 'F', long = "body-file", value_name = "FILE")]
    pub body_file: Option<String>,
    /// Preload the pull request body from a repository PR template.
    #[arg(long, value_name = "NAME", num_args = 0..=1)]
    pub template: Option<Option<String>>,
    /// Fill title and body from the commits in `base..head` (no API call).
    /// Single commit: its subject and message body. Multiple: the first
    /// subject as the title, a bullet list of subjects as the body.
    /// `--title` / `--body` / `--body-file` still override.
    #[arg(long, conflicts_with = "fill_first")]
    pub fill: bool,
    /// Like `--fill`, but use only the first (oldest) commit's subject and body.
    #[arg(long = "fill-first")]
    pub fill_first: bool,
    /// Attach a file and link it at the end of the body (repeatable).
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<PathBuf>,
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
    /// Mark the PR as draft.
    #[arg(long)]
    pub draft: bool,
    /// Open the new PR in your browser after creating.
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Pull request title
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Body (use - for stdin, omit to open $EDITOR)
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Read body from file (use `-` for stdin).
    #[arg(short = 'F', long = "body-file", value_name = "FILE")]
    pub body_file: Option<String>,
    /// Open `$EDITOR` for the body (replaces the current body).
    #[arg(long)]
    pub body_editor: bool,
    /// Retarget the pull request onto this base branch. Only sent when given:
    /// an edit of anything else never touches the base.
    #[arg(long, value_name = "BRANCH")]
    pub base: Option<String>,
    /// Attach a file and link it at the end of the body (repeatable).
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<PathBuf>,
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
pub struct AttachArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// File(s) to upload and link in the PR description (repeatable).
    #[arg(value_name = "FILE", required = true)]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Review action. Optional only when line comments are given: they are
    /// then posted on their own as a plain comment, with no verdict.
    #[arg(
        value_enum,
        long,
        short = 'e',
        required_unless_present_any = ["line_comment", "line_comments_file"]
    )]
    pub event: Option<ReviewEventArg>,
    /// Review body. Use `-` for stdin. Omit to open `$EDITOR` (unless the
    /// event is `approve` and you have nothing to say).
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Comment on one line of the diff (repeatable). All line comments are
    /// sent with the verdict in a single review. LINE is `N` or `new:N` for
    /// a line of the pull request's version of the file, `old:N` for a line
    /// of the base version (a removed line). One line only, no ranges.
    #[arg(
        long = "line-comment",
        num_args = 3,
        value_names = ["PATH", "LINE", "BODY"],
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    pub line_comment: Vec<String>,
    /// Read line comments from a JSON file (`-` for stdin): an array of
    /// `{"path": "...", "line": N, "side": "new"|"old", "body": "..."}`.
    /// `side` defaults to `new`. Adds to any `--line-comment`.
    #[arg(long = "line-comments-file", value_name = "FILE")]
    pub line_comments_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReviewerArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Reviewer usernames (repeatable, comma-separated also allowed).
    #[arg(value_name = "USERNAME")]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Local branch name to check out into (defaults to pr-<number>)
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    /// Git remote to fetch the PR ref from
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Merge method to use
    #[arg(long, value_enum)]
    pub style: Option<MergeStyleArg>,
    /// Title for the merge commit
    #[arg(long)]
    pub title: Option<String>,
    /// Message body for the merge commit
    #[arg(long)]
    pub message: Option<String>,
    /// Delete the head branch after the merge succeeds.
    #[arg(long)]
    pub delete_branch: bool,
    /// Schedule the merge to run once all required checks pass. Merges
    /// immediately if the PR is already green. Honors --style / --title /
    /// --message / --delete-branch.
    #[arg(long, conflicts_with = "disable_auto")]
    pub auto: bool,
    /// Cancel a previously scheduled auto-merge on this PR.
    #[arg(long)]
    pub disable_auto: bool,
}

#[derive(Debug, Args)]
pub struct SimpleArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
}

#[derive(Debug, Args)]
pub struct LockArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Reason for locking the conversation.
    #[arg(long, value_enum)]
    pub reason: Option<LockReasonArg>,
}

/// Args for the read-only inspect subcommands (diff / commits / files /
/// checks). Identical to `SimpleArgs` but carries `--json` so automation can
/// route the already-typed structs through `output::print_json`.
#[derive(Debug, Args)]
pub struct InspectArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UpdateBranchArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
    /// Update strategy: merge (default) or rebase.
    #[arg(long, value_enum, default_value_t = UpdateStyle::Merge)]
    pub style: UpdateStyle,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: PrCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        PrSub::List(args) => list(args, host).await,
        PrSub::View(args) => view(args, host).await,
        PrSub::Create(args) => create(args, host).await,
        PrSub::Edit(args) => edit(args, host).await,
        PrSub::Diff(args) => pr_inspect::diff(args, host).await,
        PrSub::Commits(args) => pr_inspect::commits(args, host).await,
        PrSub::Files(args) => pr_inspect::files(args, host).await,
        PrSub::Checks(args) => pr_inspect::checks(args, host).await,
        PrSub::Ready(args) => ready(args, host).await,
        PrSub::Comment(args) => pr_comment::run(args, host).await,
        PrSub::Attach(args) => attach(args, host).await,
        PrSub::UpdateBranch(args) => update_branch(args, host).await,
        PrSub::Review(args) => pr_review::review(args, host).await,
        PrSub::RequestReview(args) => pr_review::request_reviewers(args, host).await,
        PrSub::UnrequestReview(args) => pr_review::unrequest_reviewers(args, host).await,
        PrSub::Status(args) => pr_status::status(args, host).await,
        PrSub::Checkout(args) => checkout(args, host).await,
        PrSub::Merge(args) => merge(args, host).await,
        PrSub::Close(args) => set_state(args, host, "closed").await,
        PrSub::Reopen(args) => set_state(args, host, "open").await,
        PrSub::Lock(args) => lock(args, host).await,
        PrSub::Unlock(args) => unlock(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.web {
        return web::open(&repo_web_url(&ctx, "pulls")?);
    }
    // Forgejo's /pulls endpoint filters labels by ID, so resolve names first;
    // an unknown name surfaces a clear "no label named ..." error here.
    let label_names = clean_csv_values(args.label);
    let labels = if label_names.is_empty() {
        None
    } else {
        Some(
            api::label::resolve_names_to_ids(&ctx.client, &ctx.owner, &ctx.name, &label_names)
                .await?,
        )
    };
    let author = args
        .author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let draft = if args.draft {
        Some(true)
    } else if args.no_draft {
        Some(false)
    } else {
        None
    };
    let page = api::pull::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        ListOptions {
            state: args.state.into(),
            limit: args.limit,
            page: args.page,
            paginate: args.paginate,
            labels,
            author,
            base: clean_optional(args.base),
            head: clean_optional(args.head),
            draft,
        },
    )
    .await?;
    if args.json {
        return list_output::print_json(&page);
    }
    if page.items.is_empty() {
        println!("(no pull requests)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|p| {
            vec![
                format!("#{}", p.number),
                output::state_pill(&p.state, p.merged),
                output::truncate(&p.title, 55),
                output::dim(&format!(
                    "{} → {}",
                    branch_label(&p.head),
                    branch_label(&p.base)
                )),
                output::dim(&output::relative_time(p.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["", "STATE", "TITLE", "BRANCHES", "UPDATED"], &rows)
    );
    if !args.paginate {
        list_output::print_truncation_footer(&page, args.limit);
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if args.web {
        return web::open(&pr.html_url);
    }
    if args.json {
        let mut v = serde_json::to_value(&pr)?;
        if args.comments {
            let comments =
                api::issue::list_comments(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            v["comments_list"] = serde_json::to_value(comments)?;
            let reviews =
                api::pull::list_reviews(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            v["reviews"] = serde_json::to_value(reviews)?;
        }
        return output::print_json(&v);
    }
    println!(
        "{}  {}  {}",
        output::bold(&format!("#{} {}", pr.number, pr.title)),
        output::state_pill(&pr.state, pr.merged),
        output::dim(&output::relative_time(pr.updated_at)),
    );
    println!("{}", output::dim(&pr.html_url));
    println!();
    println!(
        "Branches: {} → {}",
        branch_label(&pr.head),
        branch_label(&pr.base)
    );
    if pr.draft {
        println!("Draft:    yes");
    }
    if let Some(line) = pr_merge_check::mergeable_line(&pr) {
        println!("{line}");
    }
    // Best-effort: Forgejo exposes the scheduled state only on the timeline,
    // not the PR object, and never the chosen style. A failed lookup just omits
    // the line rather than aborting the view.
    if !pr.merged && pr.state == "open" {
        if let Ok(true) =
            api::pull::auto_merge_pending(&ctx.client, &ctx.owner, &ctx.name, args.number).await
        {
            println!("Auto-merge: scheduled");
        }
    }
    if !pr.body.is_empty() {
        println!();
        println!("{}", pr.body);
    }
    if args.comments {
        println!();
        let reviews =
            api::pull::list_reviews(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
        for r in reviews {
            let when = r
                .submitted_at
                .map(output::relative_time)
                .unwrap_or_else(|| "—".into());
            println!(
                "── {} {} {}",
                output::bold(&r.user.login),
                output::state_pill(&r.state.to_lowercase(), false),
                output::dim(&when),
            );
            if !r.body.is_empty() {
                println!("{}", r.body);
                println!();
            }
        }
        let comments =
            api::issue::list_comments(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
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
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(args: CreateArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let head = match args.head {
        Some(h) => h,
        None => detect_current_branch()
            .ok_or_else(|| anyhow!("could not determine head branch; pass --head"))?,
    };
    let base = resolve_create_base(&ctx, args.base.as_deref()).await?;
    // Resolve --fill / --fill-first up front so both title and body can draw
    // from the branch's commits. None when neither flag was passed.
    let filled = match fill_mode(args.fill, args.fill_first) {
        Some(mode) => {
            let commits = git::log_commits(&base, &local_head_ref(&head))?;
            Some(pr_fill::fill_from_commits(&commits, mode)?)
        }
        None => None,
    };
    let title = match args.title {
        Some(t) => t,
        None => match &filled {
            Some(f) => f.title.clone(),
            None => editor::prompt_line("Title")?,
        },
    };
    if title.trim().is_empty() {
        return Err(anyhow!("title is required"));
    }
    let template_body = create_template::resolve(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        TemplateKind::Pull,
        args.template,
    )
    .await?;
    // Body precedence: an explicit --body / --body-file wins; otherwise the
    // filled body (if any); otherwise open the editor seeded by --template.
    let body = match &filled {
        Some(f) if args.body.is_none() && args.body_file.is_none() && f.body.is_some() => {
            f.body.clone()
        }
        _ => editor::resolve_body_input(
            args.body.as_deref(),
            args.body_file.as_deref(),
            Some(("PR_BODY.md", template_body.as_deref().unwrap_or(""))),
        )?,
    };
    let label_names = clean_csv_values(args.label);
    let requested_labels = if label_names.is_empty() {
        Vec::new()
    } else {
        api::label::resolve_names(&ctx.client, &ctx.owner, &ctx.name, &label_names).await?
    };
    let assignees = clean_csv_values(args.assignee);
    let pr = api::pull::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreatePull {
            title: &title,
            head: &head,
            base: &base,
            body: body.as_deref(),
            draft: args.draft,
        },
    )
    .await?;
    println!("✓ Created PR #{}: {}", pr.number, pr.title);
    if !requested_labels.is_empty() {
        crate::cli::label_assign::add_resolved(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            pr.number,
            requested_labels,
        )
        .await?;
    }
    if !assignees.is_empty() {
        api::issue::add_assignees(&ctx.client, &ctx.owner, &ctx.name, pr.number, assignees).await?;
    }
    if !args.attach.is_empty() {
        let refs = upload_attachments(&ctx, pr.number, &args.attach).await?;
        let new_body = append_attachments(&pr.body, &refs);
        api::pull::edit(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            pr.number,
            &EditPull {
                title: None,
                body: Some(&new_body),
                state: None,
                base: None,
            },
        )
        .await?;
    }
    println!("{}", pr.html_url);
    if args.web {
        web::open(&pr.html_url)?;
    }
    Ok(())
}

async fn resolve_create_base(
    ctx: &crate::cli::context::RepoContext,
    explicit: Option<&str>,
) -> Result<String> {
    match explicit.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => Ok(base.to_string()),
        None => {
            let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
            if repo.default_branch.trim().is_empty() {
                return Err(anyhow!("repository default branch is empty; pass --base"));
            }
            Ok(repo.default_branch)
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    run_edit(&ctx, args).await
}

/// `fj pr edit` against a resolved repository. Only the fields given on the
/// command line are sent (CLS-1063): a body-only edit never carries `base`.
async fn run_edit(ctx: &crate::cli::context::RepoContext, args: EditArgs) -> Result<()> {
    let add_labels = clean_csv_values(args.add_label);
    let remove_labels = clean_csv_values(args.remove_label);
    let add_assignees = clean_csv_values(args.add_assignee);
    let remove_assignees = clean_csv_values(args.remove_assignee);
    let has_additive_edits = !add_labels.is_empty()
        || !remove_labels.is_empty()
        || !add_assignees.is_empty()
        || !remove_assignees.is_empty();
    let base = clean_optional(args.base.clone());
    let should_patch = args.title.is_some()
        || base.is_some()
        || args.body.is_some()
        || args.body_file.is_some()
        || args.body_editor
        || !args.attach.is_empty()
        || !has_additive_edits;

    let mut updated_pr = None;
    if should_patch {
        let body = if args.body_editor {
            let existing = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            Some(editor::edit_text("PR_BODY.md", &existing.body)?)
        } else {
            editor::resolve_body_input(args.body.as_deref(), args.body_file.as_deref(), None)?
        };
        let pr = api::pull::edit_keeping_base(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            &EditPull {
                title: args.title.as_deref(),
                body: body.as_deref(),
                state: None,
                base: base.as_deref(),
            },
        )
        .await?;
        updated_pr = Some(pr);
    }
    if !args.attach.is_empty() {
        let pr = match updated_pr.as_ref() {
            Some(pr) => pr.clone(),
            None => api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?,
        };
        let refs = upload_attachments(&ctx, pr.number, &args.attach).await?;
        let new_body = append_attachments(&pr.body, &refs);
        let pr = api::pull::edit_keeping_base(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            pr.number,
            &EditPull {
                title: None,
                body: Some(&new_body),
                state: None,
                base: None,
            },
        )
        .await?;
        updated_pr = Some(pr);
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
        api::issue::add_assignees(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            add_assignees,
        )
        .await?;
    }
    if !remove_assignees.is_empty() {
        api::issue::remove_assignees(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            remove_assignees,
        )
        .await?;
    }
    if let Some(pr) = updated_pr {
        println!("✓ Updated PR #{}: {}", pr.number, pr.title);
    } else {
        println!("✓ Updated PR #{}", args.number);
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn attach(args: AttachArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let refs = upload_attachments(&ctx, args.number, &args.files).await?;
    // Append the links to the live body so they land in the description.
    let existing = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let new_body = append_attachments(&existing.body, &refs);
    api::pull::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        &EditPull {
            title: None,
            body: Some(&new_body),
            state: None,
            base: None,
        },
    )
    .await?;
    println!(
        "✓ Attached {} file(s) to PR #{}",
        args.files.len(),
        args.number
    );
    Ok(())
}

/// Upload each file as an attachment on PR `number`, returning a markdown
/// reference for each in input order. Shared by `pr attach` and the `--attach`
/// flag on create/edit.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn upload_attachments(
    ctx: &crate::cli::context::RepoContext,
    number: u64,
    files: &[PathBuf],
) -> Result<Vec<String>> {
    let mut refs = Vec::with_capacity(files.len());
    for path in files {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("attachment has no usable filename: {}", path.display()))?;
        let att = api::issue::upload_asset(&ctx.client, &ctx.owner, &ctx.name, number, name, bytes)
            .await?;
        println!("✓ Uploaded {} ({} bytes)", name, att.size);
        refs.push(attachment_markdown(name, &att.browser_download_url));
    }
    Ok(refs)
}

/// A markdown reference for an uploaded attachment: an inline image embed for
/// image extensions, a plain link otherwise.
fn attachment_markdown(name: &str, url: &str) -> String {
    const IMG_EXTS: &[&str] = &[
        "png", "apng", "jpg", "jpeg", "gif", "webp", "avif", "svg", "bmp",
    ];
    let is_image = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMG_EXTS.contains(&e.to_ascii_lowercase().as_str()));
    if is_image {
        format!("![{name}]({url})")
    } else {
        format!("[{name}]({url})")
    }
}

/// Append attachment references to a body as a trailing block, separated by a
/// blank line. An empty body just becomes the references.
fn append_attachments(body: &str, refs: &[String]) -> String {
    if refs.is_empty() {
        return body.to_string();
    }
    let block = refs.join("\n");
    if body.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{}", body.trim_end(), block)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn ready(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if api::pull::ready(&ctx.client, &ctx.owner, &ctx.name, args.number).await? {
        println!("✓ PR #{} is ready for review", args.number);
    } else {
        println!("PR #{} is already ready for review", args.number);
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn update_branch(args: UpdateBranchArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::pull::update_branch(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        args.style.as_str(),
    )
    .await?;
    println!("✓ Updated PR #{} from its base branch", args.number);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn checkout(args: CheckoutArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let branch = args.branch.unwrap_or_else(|| format!("pr-{}", pr.number));
    git::fetch_pr(&args.remote, pr.number, &branch)?;
    git::checkout(&branch)?;
    println!("✓ Checked out #{} into {branch}", pr.number);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn merge(args: MergeArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;

    // `--disable-auto` only cancels a pending schedule; style/title are ignored.
    if args.disable_auto {
        return match api::pull::cancel_auto_merge(&ctx.client, &ctx.owner, &ctx.name, args.number)
            .await
        {
            Ok(()) => {
                println!("✓ Cancelled scheduled auto-merge for PR #{}", args.number);
                Ok(())
            }
            // The endpoint 404s when nothing is scheduled; say so plainly
            // rather than surfacing a bare "not found".
            Err(e) => match e.downcast_ref::<crate::client::ApiError>() {
                Some(api_err) if api_err.status == reqwest::StatusCode::NOT_FOUND => Err(anyhow!(
                    "PR #{} has no scheduled auto-merge to cancel.",
                    args.number
                )),
                _ => Err(e),
            },
        };
    }

    let style = match args.style {
        Some(style) => style.into(),
        None => repo_default_merge_style(&ctx).await?,
    };
    // Read what the base branch's rule requires before asking the server to
    // merge, so a blocker that is already certain is explained up front. A PR
    // that cannot be read, or is not open, goes straight to the server, which
    // answers with its own error. Unknown evidence never blocks here.
    let preflight = match api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await {
        Ok(pr) if !pr.merged && pr.state == "open" => {
            Some(pr_preflight::gather(&ctx.client, &ctx.owner, &ctx.name, &pr, None).await)
        }
        _ => None,
    };
    if let Some(blocker) = preflight
        .as_ref()
        .and_then(|p| p.pre_merge_blocker(args.auto))
    {
        return Err(anyhow!("{blocker}"));
    }
    let res = api::pull::merge(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        style,
        &api::pull::MergeOptions {
            title: args.title.as_deref(),
            message: args.message.as_deref(),
            delete_branch: args.delete_branch,
            auto: args.auto,
        },
    )
    .await;
    match res {
        // With --auto the same 200 covers "merged now" (already green) and
        // "scheduled" (checks still pending); re-read the PR to report which.
        Ok(()) if args.auto => {
            let merged_now = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number)
                .await
                .map(|pr| pr.merged)
                .unwrap_or(false);
            if merged_now {
                println!("✓ Merged PR #{} (already green)", args.number);
            } else {
                println!(
                    "✓ Auto-merge scheduled for PR #{} ({}); it will merge once required checks pass.\n  Cancel with: fj pr merge {} --disable-auto",
                    args.number,
                    style.as_str(),
                    args.number,
                );
            }
            Ok(())
        }
        Ok(()) => {
            println!("✓ Merged PR #{}", args.number);
            Ok(())
        }
        // Forgejo returns an opaque 405/500 for every non-mergeable cause.
        // Inspect the PR's mergeability detail and report the specific reason;
        // fall back to the raw API error when nothing specific is found.
        Err(e) => {
            if let Ok(pr) = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await {
                if let Some(reason) = pr_merge_check::diagnose_merge_failure(
                    &ctx.client,
                    &ctx.owner,
                    &ctx.name,
                    &pr,
                    style.as_str(),
                    Some(&e.to_string()),
                    preflight.as_ref(),
                )
                .await
                {
                    // A host without scheduled-merge support silently ignores
                    // the flag and rejects a not-yet-green PR with the same
                    // opaque error a normal merge would (a supported host would
                    // have scheduled it and returned success). Flag that case.
                    if args.auto && reason.contains("required check") {
                        return Err(anyhow!(
                            "{reason}\n  note: --auto did not schedule. This host build may not support scheduled auto-merge; merge once checks pass instead."
                        ));
                    }
                    return Err(anyhow!("{reason}"));
                }
            }
            // Nothing specific: keep the server's own words, and say which
            // evidence was left unread so the gap is not read as a pass.
            match preflight.as_ref().and_then(|p| p.refusal_note()) {
                Some(note) if is_refusal(&e) => {
                    let message = format!("{e}\n  note: {note}");
                    Err(e.context(message))
                }
                _ => Err(e),
            }
        }
    }
}

/// True when the server answered and said no (a 4xx), as opposed to a network
/// failure or a server error, where a note about branch rules would mislead.
fn is_refusal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<crate::client::ApiError>()
        .is_some_and(|api| api.status.is_client_error())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn repo_default_merge_style(
    ctx: &crate::cli::context::RepoContext,
) -> Result<api::pull::MergeStyle> {
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name)
        .await
        .map_err(|e| {
            anyhow!(
                "no --style given and the repo default_merge_style could not be read for {}/{}: {e}",
                ctx.owner,
                ctx.name
            )
        })?;
    let Some(raw) = repo.default_merge_style.as_deref() else {
        return Err(anyhow!(
            "no --style given and the repo default_merge_style could not be read for {}/{}",
            ctx.owner,
            ctx.name
        ));
    };
    api::pull::MergeStyle::from_str(raw).ok_or_else(|| {
        anyhow!(
            "no --style given and {}/{} reports unsupported default_merge_style {:?}; supported styles: merge, rebase, rebase-merge, squash, fast-forward-only",
            ctx.owner,
            ctx.name,
            raw
        )
    })
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_state(args: SimpleArgs, host: Option<&str>, state: &str) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::pull::edit(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        &EditPull {
            state: Some(state),
            ..Default::default()
        },
    )
    .await?;
    println!(
        "✓ PR #{} is now {}",
        args.number,
        output::state_pill(state, false)
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn lock(args: LockArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if pr.is_locked {
        println!("PR #{} is already locked", args.number);
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
    println!("✓ Locked PR #{}", args.number);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn unlock(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    if !pr.is_locked {
        println!("PR #{} is already unlocked", args.number);
        return Ok(());
    }
    api::issue::unlock(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ Unlocked PR #{}", args.number);
    Ok(())
}

/// Render a branch name for display. Forgejo populates `head.ref` with
/// `refs/pull/<n>/head` (the synthetic PR ref) when the source branch is gone
/// or the PR comes from a fork; in that case the real name is in `label`.
/// For same-repo PRs `ref` is `refs/heads/<branch>` and `label` is the bare
/// branch name. Prefer `label` whenever it's non-empty.
fn branch_label(b: &crate::api::pull::Branch) -> String {
    if !b.label.is_empty() {
        return b.label.clone();
    }
    b.ref_
        .strip_prefix("refs/heads/")
        .unwrap_or(&b.ref_)
        .to_string()
}

/// The fill mode requested on `pr create`, if any. `--fill-first` and `--fill`
/// are mutually exclusive at the clap layer, so checking first is unambiguous.
fn fill_mode(fill: bool, fill_first: bool) -> Option<FillMode> {
    if fill_first {
        Some(FillMode::First)
    } else if fill {
        Some(FillMode::All)
    } else {
        None
    }
}

/// The local ref to read commits from. A cross-repo head is passed as
/// `owner:branch`, but its commits live under the bare branch name locally, so
/// strip any `owner:` prefix before running `git log`.
fn local_head_ref(head: &str) -> String {
    head.rsplit_once(':')
        .map(|(_, branch)| branch)
        .unwrap_or(head)
        .to_string()
}

fn detect_current_branch() -> Option<String> {
    use std::process::Command;
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::pull::Branch;

    fn br(label: &str, r: &str) -> Branch {
        Branch {
            label: label.into(),
            ref_: r.into(),
            sha: String::new(),
        }
    }

    #[test]
    fn prefers_label_when_present() {
        let b = br("feature/foo", "refs/pull/3/head");
        assert_eq!(branch_label(&b), "feature/foo");
    }

    #[test]
    fn strips_refs_heads_when_no_label() {
        let b = br("", "refs/heads/main");
        assert_eq!(branch_label(&b), "main");
    }

    #[test]
    fn passes_through_synthetic_when_no_label_no_prefix() {
        let b = br("", "refs/pull/7/head");
        assert_eq!(branch_label(&b), "refs/pull/7/head");
    }

    #[test]
    fn merge_parses_style_and_delete_branch() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj",
            "pr",
            "merge",
            "12",
            "--style",
            "squash",
            "--delete-branch",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Merge(a),
            }) => {
                assert_eq!(a.number, 12);
                assert!(a.delete_branch);
                assert!(matches!(a.style, Some(MergeStyleArg::Squash)));
            }
            other => panic!("expected pr merge, got {other:?}"),
        }
        // defaults: no delete, style resolved from the repository at runtime.
        let cli = Cli::try_parse_from(["fj", "pr", "merge", "3"]).expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::Merge(a),
        }) = cli.command
        {
            assert!(!a.delete_branch);
            assert!(a.style.is_none());
        }
    }

    #[test]
    fn merge_parses_auto_and_disable_auto() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "merge", "12", "--auto", "--style", "squash"])
            .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Merge(a),
            }) => {
                assert!(a.auto);
                assert!(!a.disable_auto);
                assert!(matches!(a.style, Some(MergeStyleArg::Squash)));
            }
            other => panic!("expected pr merge, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["fj", "pr", "merge", "12", "--disable-auto"])
            .expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::Merge(a),
        }) = cli.command
        {
            assert!(a.disable_auto);
            assert!(!a.auto);
        }
    }

    #[test]
    fn merge_auto_and_disable_auto_conflict() {
        use crate::cli::Cli;
        use clap::Parser;
        // The two flags are mutually exclusive.
        assert!(
            Cli::try_parse_from(["fj", "pr", "merge", "12", "--auto", "--disable-auto"]).is_err()
        );
    }

    #[tokio::test]
    async fn omitted_merge_style_uses_repo_default() {
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "name": "r",
                "full_name": "o/r",
                "owner": { "id": 1, "login": "o" },
                "html_url": "https://x/o/r",
                "clone_url": "https://x/o/r.git",
                "ssh_url": "git@x:o/r.git",
                "default_branch": "main",
                "default_merge_style": "squash",
                "updated_at": "2026-05-30T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
        let ctx = crate::cli::context::RepoContext {
            client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
            host: "mock".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        let style = repo_default_merge_style(&ctx).await.expect("style");
        assert!(matches!(style, api::pull::MergeStyle::Squash));
    }

    #[tokio::test]
    async fn omitted_merge_style_accepts_fast_forward_only_repo_default() {
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "name": "r",
                "full_name": "o/r",
                "owner": { "id": 1, "login": "o" },
                "html_url": "https://x/o/r",
                "clone_url": "https://x/o/r.git",
                "ssh_url": "git@x:o/r.git",
                "default_branch": "main",
                "default_merge_style": "fast-forward-only",
                "updated_at": "2026-05-30T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
        let ctx = crate::cli::context::RepoContext {
            client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
            host: "mock".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        let style = repo_default_merge_style(&ctx).await.expect("style");
        assert!(matches!(style, api::pull::MergeStyle::FastForwardOnly));
    }

    #[tokio::test]
    async fn omitted_merge_style_fails_when_repo_default_is_absent() {
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "name": "r",
                "full_name": "o/r",
                "owner": { "id": 1, "login": "o" },
                "html_url": "https://x/o/r",
                "clone_url": "https://x/o/r.git",
                "ssh_url": "git@x:o/r.git",
                "default_branch": "main",
                "updated_at": "2026-05-30T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
        let ctx = crate::cli::context::RepoContext {
            client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
            host: "mock".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        let err = repo_default_merge_style(&ctx)
            .await
            .expect_err("missing default must fail");
        assert!(
            err.to_string()
                .contains("no --style given and the repo default_merge_style could not be read"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn omitted_merge_style_fails_when_repo_default_is_unsupported() {
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for default_merge_style in ["manually-merged"] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/r"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 1,
                    "name": "r",
                    "full_name": "o/r",
                    "owner": { "id": 1, "login": "o" },
                    "html_url": "https://x/o/r",
                    "clone_url": "https://x/o/r.git",
                    "ssh_url": "git@x:o/r.git",
                    "default_branch": "main",
                    "default_merge_style": default_merge_style,
                    "updated_at": "2026-05-30T00:00:00Z"
                })))
                .expect(1)
                .mount(&server)
                .await;
            let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
            let ctx = crate::cli::context::RepoContext {
                client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
                host: "mock".into(),
                web_base: None,
                owner: "o".into(),
                name: "r".into(),
            };
            let err = repo_default_merge_style(&ctx)
                .await
                .expect_err("unsupported default must fail");
            let msg = err.to_string();
            assert!(
                msg.contains(&format!(
                    "unsupported default_merge_style {default_merge_style:?}"
                )),
                "got: {msg}"
            );
            assert!(
                msg.contains(
                    "supported styles: merge, rebase, rebase-merge, squash, fast-forward-only"
                ),
                "got: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn omitted_merge_style_fails_when_repo_default_cannot_be_fetched() {
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r"))
            .respond_with(ResponseTemplate::new(500).set_body_string("temporary failure"))
            .expect(3)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
        let ctx = crate::cli::context::RepoContext {
            client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
            host: "mock".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        let err = repo_default_merge_style(&ctx)
            .await
            .expect_err("repo fetch failure must fail");
        let msg = err.to_string();
        assert!(
            msg.contains(
                "no --style given and the repo default_merge_style could not be read for o/r"
            ),
            "got: {msg}"
        );
        assert!(msg.contains("temporary failure"), "got: {msg}");
    }

    #[test]
    fn attachment_markdown_embeds_images_links_others() {
        assert_eq!(
            attachment_markdown("diagram.PNG", "https://x/a"),
            "![diagram.PNG](https://x/a)"
        );
        assert_eq!(
            attachment_markdown("notes.pdf", "https://x/b"),
            "[notes.pdf](https://x/b)"
        );
        // No extension -> treated as a plain link.
        assert_eq!(
            attachment_markdown("LICENSE", "https://x/c"),
            "[LICENSE](https://x/c)"
        );
    }

    #[test]
    fn append_attachments_separates_and_handles_empties() {
        let refs = vec!["![a](u1)".to_string(), "[b](u2)".to_string()];
        assert_eq!(
            append_attachments("Fixes the thing.", &refs),
            "Fixes the thing.\n\n![a](u1)\n[b](u2)"
        );
        // Empty body becomes just the block.
        assert_eq!(append_attachments("   ", &refs), "![a](u1)\n[b](u2)");
        // No refs leaves the body untouched.
        assert_eq!(append_attachments("body", &[]), "body");
    }

    #[test]
    fn attach_parses_number_and_multiple_files() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "attach", "9", "a.png", "b.txt"])
            .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Attach(a),
            }) => {
                assert_eq!(a.number, 9);
                assert_eq!(a.files.len(), 2);
            }
            other => panic!("expected pr attach, got {other:?}"),
        }
    }

    #[test]
    fn create_collects_repeated_attach_flags() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj", "pr", "create", "--title", "t", "--attach", "a.png", "--attach", "b.png",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => assert_eq!(a.attach.len(), 2),
            other => panic!("expected pr create, got {other:?}"),
        }
    }

    #[test]
    fn list_parses_label_and_author_filters() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj",
            "pr",
            "list",
            "--label",
            "needs-review,p1",
            "--label",
            "blocked",
            "--author",
            "alice",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::List(a),
            }) => {
                assert_eq!(a.label, vec!["needs-review", "p1", "blocked"]);
                assert_eq!(a.author.as_deref(), Some("alice"));
                assert!(a.base.is_none());
                assert!(a.head.is_none());
                assert!(!a.draft);
                assert!(!a.no_draft);
            }
            other => panic!("expected pr list, got {other:?}"),
        }
        // Filters are optional: bare `pr list` leaves them empty.
        let cli = Cli::try_parse_from(["fj", "pr", "list"]).expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::List(a),
        }) = cli.command
        {
            assert!(a.label.is_empty());
            assert!(a.author.is_none());
        }
    }

    #[test]
    fn list_parses_branch_and_draft_filters() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj", "pr", "list", "--base", "main", "--head", "feature", "--draft",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::List(a),
            }) => {
                assert_eq!(a.base.as_deref(), Some("main"));
                assert_eq!(a.head.as_deref(), Some("feature"));
                assert!(a.draft);
                assert!(!a.no_draft);
            }
            other => panic!("expected pr list, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "pr", "list", "--no-draft"]).expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::List(a),
            }) => {
                assert!(!a.draft);
                assert!(a.no_draft);
            }
            other => panic!("expected pr list, got {other:?}"),
        }
    }

    #[test]
    fn lock_and_unlock_parse() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "lock", "9", "--reason", "spam"])
            .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Lock(a),
            }) => {
                assert_eq!(a.number, 9);
                assert!(matches!(a.reason, Some(LockReasonArg::Spam)));
            }
            other => panic!("expected pr lock, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "pr", "unlock", "9"]).expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Unlock(a),
            }) => assert_eq!(a.number, 9),
            other => panic!("expected pr unlock, got {other:?}"),
        }
    }

    #[test]
    fn create_base_is_optional_at_parse_time() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "create", "--title", "t"])
            .expect("base should be optional");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => assert!(a.base.is_none()),
            other => panic!("expected pr create, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_base_defaults_to_repo_default_branch() {
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "name": "r",
                "full_name": "o/r",
                "owner": { "id": 1, "login": "o" },
                "html_url": "https://forge.example/o/r",
                "clone_url": "https://forge.example/o/r.git",
                "ssh_url": "git@forge.example:o/r.git",
                "default_branch": "develop",
                "updated_at": "2026-01-01T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::client::Client::for_base_url(
            Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
            "tok".into(),
        )
        .unwrap();
        let ctx = crate::cli::context::RepoContext {
            client,
            host: "forge.example".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        assert_eq!(resolve_create_base(&ctx, None).await.unwrap(), "develop");
    }

    #[tokio::test]
    async fn create_base_uses_explicit_base_without_repo_lookup() {
        let client = crate::client::Client::for_base_url(
            "https://forge.example/api/v1/".parse().unwrap(),
            "tok".into(),
        )
        .unwrap();
        let ctx = crate::cli::context::RepoContext {
            client,
            host: "forge.example".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        };
        assert_eq!(
            resolve_create_base(&ctx, Some("release")).await.unwrap(),
            "release"
        );
    }

    #[test]
    fn create_parses_labels_and_assignees() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj",
            "pr",
            "create",
            "--title",
            "t",
            "--label",
            "area/api,needs-review",
            "--label",
            "wip",
            "--assignee",
            "alice,bob",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => {
                assert_eq!(a.label, vec!["area/api", "needs-review", "wip"]);
                assert_eq!(a.assignee, vec!["alice", "bob"]);
            }
            other => panic!("expected pr create, got {other:?}"),
        }
    }

    #[test]
    fn create_parses_template_with_and_without_name() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "create", "--template", "default"])
            .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => assert_eq!(a.template, Some(Some("default".into()))),
            other => panic!("expected pr create, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["fj", "pr", "create", "--template"]).expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => assert_eq!(a.template, Some(None)),
            other => panic!("expected pr create, got {other:?}"),
        }
    }

    #[test]
    fn create_parses_fill_flags() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["fj", "pr", "create", "--fill"]).expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Create(a),
            }) => {
                assert!(a.fill);
                assert!(!a.fill_first);
                assert!(matches!(
                    fill_mode(a.fill, a.fill_first),
                    Some(FillMode::All)
                ));
            }
            other => panic!("expected pr create, got {other:?}"),
        }
        let cli =
            Cli::try_parse_from(["fj", "pr", "create", "--fill-first"]).expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::Create(a),
        }) = cli.command
        {
            assert!(a.fill_first);
            assert!(matches!(
                fill_mode(a.fill, a.fill_first),
                Some(FillMode::First)
            ));
        }
        // Bare create requests no fill.
        let cli = Cli::try_parse_from(["fj", "pr", "create"]).expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::Create(a),
        }) = cli.command
        {
            assert!(fill_mode(a.fill, a.fill_first).is_none());
        }
    }

    #[test]
    fn create_rejects_both_fill_flags() {
        use crate::cli::Cli;
        use clap::Parser;
        let err = Cli::try_parse_from(["fj", "pr", "create", "--fill", "--fill-first"]);
        assert!(err.is_err(), "--fill and --fill-first must conflict");
    }

    #[test]
    fn local_head_ref_strips_owner_prefix() {
        assert_eq!(local_head_ref("feature/x"), "feature/x");
        assert_eq!(local_head_ref("alice:feature/x"), "feature/x");
    }

    #[test]
    fn edit_parses_label_and_assignee_mutations() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "fj",
            "pr",
            "edit",
            "12",
            "--add-label",
            "needs-review",
            "--remove-label",
            "wip,blocked",
            "--add-assignee",
            "alice",
            "--remove-assignee",
            "bob,carol",
        ])
        .expect("should parse");
        match cli.command {
            Command::Pr(PrCmd {
                command: PrSub::Edit(a),
            }) => {
                assert_eq!(a.number, 12);
                assert_eq!(a.add_label, vec!["needs-review"]);
                assert_eq!(a.remove_label, vec!["wip", "blocked"]);
                assert_eq!(a.add_assignee, vec!["alice"]);
                assert_eq!(a.remove_assignee, vec!["bob", "carol"]);
            }
            other => panic!("expected pr edit, got {other:?}"),
        }
    }
    // CLS-1063: `fj pr edit` sends only the fields it was given.

    mod edit_requests {
        use super::*;
        use crate::cli::{Cli, Command};
        use clap::Parser;
        use serde_json::{json, Value};
        use url::Url;
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const PULL_PATH: &str = "/api/v1/repos/o/r/pulls/7";

        fn pull(base: &str, body: &str) -> Value {
            json!({
                "id": 1,
                "number": 7,
                "title": "A change",
                "body": body,
                "state": "open",
                "user": { "id": 1, "login": "o" },
                "labels": [],
                "html_url": "https://x/o/r/pulls/7",
                "head": { "label": "o:topic", "ref": "topic", "sha": "a" },
                "base": { "label": format!("o:{base}"), "ref": base, "sha": "b" },
                "created_at": "2026-10-31T00:00:00Z",
                "updated_at": "2026-10-31T00:00:00Z"
            })
        }

        fn ctx(server: &MockServer) -> crate::cli::context::RepoContext {
            let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base");
            crate::cli::context::RepoContext {
                client: crate::client::Client::for_base_url(base, "test-token".into()).unwrap(),
                host: "mock".into(),
                web_base: None,
                owner: "o".into(),
                name: "r".into(),
            }
        }

        fn args(argv: &[&str]) -> EditArgs {
            let mut full = vec!["fj", "pr", "edit", "7"];
            full.extend_from_slice(argv);
            match Cli::try_parse_from(full).expect("parses").command {
                Command::Pr(PrCmd {
                    command: PrSub::Edit(a),
                }) => a,
                other => panic!("expected pr edit, got {other:?}"),
            }
        }

        async fn patch_bodies(server: &MockServer) -> Vec<Value> {
            server
                .received_requests()
                .await
                .expect("recorded")
                .into_iter()
                .filter(|r| r.method.as_str() == "PATCH")
                .map(|r| serde_json::from_slice(&r.body).expect("json body"))
                .collect()
        }

        /// The request that retargeted #1345 from `develop` to `main`: a body-only
        /// edit of a PR that is not on the default branch.
        #[tokio::test]
        async fn a_body_only_edit_of_a_pr_on_develop_sends_only_the_body() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(PULL_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "old")))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "body": "new body" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "new body")))
                .expect(1)
                .mount(&server)
                .await;

            let file = tempfile::NamedTempFile::new().expect("file");
            std::fs::write(file.path(), "new body").expect("write");
            let path_arg = file.path().to_string_lossy().to_string();
            run_edit(&ctx(&server), args(&["--body-file", &path_arg]))
                .await
                .expect("edit");

            let bodies = patch_bodies(&server).await;
            assert_eq!(bodies, vec![json!({ "body": "new body" })]);
            assert!(
                bodies[0].get("base").is_none(),
                "a body-only edit never sends base"
            );
        }

        #[tokio::test]
        async fn a_title_only_edit_sends_only_the_title() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(PULL_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "old")))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "title": "Better" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "old")))
                .expect(1)
                .mount(&server)
                .await;

            run_edit(&ctx(&server), args(&["--title", "Better"]))
                .await
                .expect("edit");

            assert_eq!(
                patch_bodies(&server).await,
                vec![json!({ "title": "Better" })]
            );
        }

        #[tokio::test]
        async fn base_is_sent_only_when_asked_for() {
            let server = MockServer::start().await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "base": "release" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("release", "old")))
                .expect(1)
                .mount(&server)
                .await;

            run_edit(&ctx(&server), args(&["--base", "release"]))
                .await
                .expect("edit");

            assert_eq!(
                patch_bodies(&server).await,
                vec![json!({ "base": "release" })]
            );
        }

        /// Whatever moves a PR's base during an edit that did not ask for it, the
        /// edit fails and the base is put back, rather than leaving a PR aimed at
        /// the default branch.
        #[tokio::test]
        async fn a_base_the_forge_moved_anyway_is_put_back_and_reported() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(PULL_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "old")))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "body": "new body" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("main", "new body")))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "base": "develop" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "new body")))
                .expect(1)
                .mount(&server)
                .await;

            let file = tempfile::NamedTempFile::new().expect("file");
            std::fs::write(file.path(), "new body").expect("write");
            let path_arg = file.path().to_string_lossy().to_string();
            let error = run_edit(&ctx(&server), args(&["--body-file", &path_arg]))
                .await
                .expect_err("a moved base is an error");

            let message = format!("{error:#}");
            assert!(
                message.contains("from base `develop` to `main`"),
                "{message}"
            );
            assert!(message.contains("put back"), "{message}");
            assert_eq!(
                patch_bodies(&server).await,
                vec![json!({ "body": "new body" }), json!({ "base": "develop" })]
            );
        }

        #[tokio::test]
        async fn a_base_that_cannot_be_put_back_says_how_to_set_it() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(PULL_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("develop", "old")))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "title": "Better" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull("main", "old")))
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path(PULL_PATH))
                .and(body_json(json!({ "base": "develop" })))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;

            let error = run_edit(&ctx(&server), args(&["--title", "Better"]))
                .await
                .expect_err("a moved base is an error");

            let message = format!("{error:#}");
            assert!(message.contains("could not be put back"), "{message}");
            assert!(message.contains("--base develop"), "{message}");
        }
    }
}
