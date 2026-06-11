use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::pull::{CreatePull, EditPull, ListOptions};
use crate::cli::context::{repo_web_url, resolve_repo, RepoFlag};
use crate::cli::pr_args::{MergeStyleArg, ReviewEventArg, StateFilter, UpdateStyle};
use crate::cli::pr_status::StatusArgs;
use crate::cli::{list as list_output, pr_comment, pr_inspect, pr_review, pr_status};
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
    /// Base branch the PR targets
    #[arg(short = 'B', long, default_value = "main")]
    pub base: String,
    /// Head branch. Either a branch name on the same repo, or `owner:branch`.
    #[arg(short = 'H', long)]
    pub head: Option<String>,
    /// Body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
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
    /// Open `$EDITOR` for the body (replaces the current body).
    #[arg(long)]
    pub body_editor: bool,
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
    /// Review action.
    #[arg(value_enum, long, short = 'e')]
    pub event: ReviewEventArg,
    /// Review body. Use `-` for stdin. Omit to open `$EDITOR` (unless the
    /// event is `approve` and you have nothing to say).
    #[arg(short = 'b', long)]
    pub body: Option<String>,
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
    #[arg(long, value_enum, default_value_t = MergeStyleArg::Merge)]
    pub style: MergeStyleArg,
    /// Title for the merge commit
    #[arg(long)]
    pub title: Option<String>,
    /// Message body for the merge commit
    #[arg(long)]
    pub message: Option<String>,
    /// Delete the head branch after the merge succeeds.
    #[arg(long)]
    pub delete_branch: bool,
}

#[derive(Debug, Args)]
pub struct SimpleArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Pull request number
    pub number: u64,
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
    let title = match args.title {
        Some(t) => t,
        None => editor::prompt_line("Title")?,
    };
    if title.trim().is_empty() {
        return Err(anyhow!("title is required"));
    }
    let body = editor::resolve_body(args.body.as_deref(), "PR_BODY.md", "")?;
    let label_names = clean_csv_values(args.label);
    let label_ids = if label_names.is_empty() {
        None
    } else {
        Some(
            api::label::resolve_names_to_ids(&ctx.client, &ctx.owner, &ctx.name, &label_names)
                .await?,
        )
    };
    let assignees = clean_csv_values(args.assignee);
    let pr = api::pull::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreatePull {
            title: &title,
            head: &head,
            base: &args.base,
            body: body.as_deref(),
            draft: args.draft,
        },
    )
    .await?;
    println!("✓ Created PR #{}: {}", pr.number, pr.title);
    if let Some(label_ids) = label_ids {
        api::label::add_to_issue(&ctx.client, &ctx.owner, &ctx.name, pr.number, label_ids).await?;
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
        || args.body_editor
        || !args.attach.is_empty()
        || !has_additive_edits;

    let mut updated_pr = None;
    if should_patch {
        let body = if args.body_editor {
            let existing = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
            Some(editor::edit_text("PR_BODY.md", &existing.body)?)
        } else {
            editor::read_body(args.body.as_deref())?
        };
        let pr = api::pull::edit(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            &EditPull {
                title: args.title.as_deref(),
                body: body.as_deref(),
                state: None,
                base: None,
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
        let pr = api::pull::edit(
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
    api::pull::ready(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ PR #{} is ready for review", args.number);
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
    api::pull::merge(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        args.style.into(),
        &api::pull::MergeOptions {
            title: args.title.as_deref(),
            message: args.message.as_deref(),
            delete_branch: args.delete_branch,
        },
    )
    .await?;
    println!("✓ Merged PR #{}", args.number);
    Ok(())
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
                assert!(matches!(a.style, MergeStyleArg::Squash));
            }
            other => panic!("expected pr merge, got {other:?}"),
        }
        // defaults: no delete, merge-commit style
        let cli = Cli::try_parse_from(["fj", "pr", "merge", "3"]).expect("should parse");
        if let Command::Pr(PrCmd {
            command: PrSub::Merge(a),
        }) = cli.command
        {
            assert!(!a.delete_branch);
            assert!(matches!(a.style, MergeStyleArg::Merge));
        }
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
}
