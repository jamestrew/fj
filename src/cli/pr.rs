use std::io::Read;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::pull::{CreatePull, EditPull, ListOptions};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::pr_args::{MergeStyleArg, ReviewEventArg, StateFilter, UpdateStyle};
use crate::cli::pr_status::StatusArgs;
use crate::cli::{pr_inspect, pr_review, pr_status};
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
    /// Show the unified diff for a pull request.
    Diff(SimpleArgs),
    /// List commits in a pull request.
    Commits(SimpleArgs),
    /// Show the file change list for a pull request.
    Files(SimpleArgs),
    /// Show CI / commit status checks for a pull request's head SHA.
    Checks(SimpleArgs),
    /// Convert a draft pull request to ready-for-review.
    Ready(SimpleArgs),
    /// Add a comment to a pull request.
    Comment(CommentArgs),
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
    #[arg(short = 's', long, value_enum, default_value_t = StateFilter::Open)]
    pub state: StateFilter,
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long)]
    pub json: bool,
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
    #[arg(long)]
    pub web: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[arg(short = 'B', long, default_value = "main")]
    pub base: String,
    /// Head branch. Either a branch name on the same repo, or `owner:branch`.
    #[arg(short = 'H', long)]
    pub head: Option<String>,
    /// Body. Use `-` to read from stdin. Omit to open `$EDITOR`.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
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
    pub number: u64,
    #[arg(short = 't', long)]
    pub title: Option<String>,
    #[arg(short = 'b', long)]
    pub body: Option<String>,
    /// Open `$EDITOR` for the body (replaces the current body).
    #[arg(long)]
    pub body_editor: bool,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
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
    pub number: u64,
    /// Reviewer usernames (repeatable, comma-separated also allowed).
    #[arg(value_name = "USERNAME")]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
    #[arg(long, value_enum, default_value_t = MergeStyleArg::Merge)]
    pub style: MergeStyleArg,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub message: Option<String>,
}

#[derive(Debug, Args)]
pub struct SimpleArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub number: u64,
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
pub struct UpdateBranchArgs {
    #[command(flatten)]
    pub r: RepoFlag,
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
        PrSub::Comment(args) => comment(args, host).await,
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
        return web::open(&format!(
            "https://{}/{}/{}/pulls",
            ctx.host, ctx.owner, ctx.name
        ));
    }
    let page = api::pull::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        ListOptions {
            state: args.state.into(),
            limit: args.limit,
            page: args.page,
        },
    )
    .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&page.items)?);
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
    println!("{}", pr.html_url);
    if args.web {
        web::open(&pr.html_url)?;
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(args: EditArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
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
        },
    )
    .await?;
    println!("✓ Updated PR #{}: {}", pr.number, pr.title);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn ready(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::pull::ready(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ PR #{} is ready for review", args.number);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn comment(args: CommentArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        Some(s) => s.to_string(),
        None => editor::edit_text("PR_COMMENT.md", "")?,
    };
    if body.trim().is_empty() {
        return Err(anyhow!("comment body is empty"));
    }
    // PRs are issues in Forgejo, so the comment endpoint is the issue one.
    let c = api::issue::comment(&ctx.client, &ctx.owner, &ctx.name, args.number, &body).await?;
    println!("✓ Commented on #{} ({})", args.number, c.html_url);
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
        args.title.as_deref(),
        args.message.as_deref(),
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
}
