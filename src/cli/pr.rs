use std::io::Read;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::State;
use crate::api::pull::{CreatePull, EditPull, ListOptions, MergeStyle, ReviewEvent};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::client::Client;
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MergeStyleArg {
    Merge,
    Rebase,
    RebaseMerge,
    Squash,
}

impl From<MergeStyleArg> for MergeStyle {
    fn from(value: MergeStyleArg) -> Self {
        match value {
            MergeStyleArg::Merge => MergeStyle::Merge,
            MergeStyleArg::Rebase => MergeStyle::Rebase,
            MergeStyleArg::RebaseMerge => MergeStyle::RebaseMerge,
            MergeStyleArg::Squash => MergeStyle::Squash,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReviewEventArg {
    Approve,
    RequestChanges,
    Comment,
}

impl From<ReviewEventArg> for ReviewEvent {
    fn from(value: ReviewEventArg) -> Self {
        match value {
            ReviewEventArg::Approve => ReviewEvent::Approve,
            ReviewEventArg::RequestChanges => ReviewEvent::RequestChanges,
            ReviewEventArg::Comment => ReviewEvent::Comment,
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
pub struct StatusArgs {
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
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

pub async fn run(cmd: PrCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        PrSub::List(args) => list(args, host).await,
        PrSub::View(args) => view(args, host).await,
        PrSub::Create(args) => create(args, host).await,
        PrSub::Edit(args) => edit(args, host).await,
        PrSub::Diff(args) => diff(args, host).await,
        PrSub::Commits(args) => commits(args, host).await,
        PrSub::Files(args) => files(args, host).await,
        PrSub::Checks(args) => checks(args, host).await,
        PrSub::Ready(args) => ready(args, host).await,
        PrSub::Review(args) => review(args, host).await,
        PrSub::RequestReview(args) => request_reviewers(args, host).await,
        PrSub::UnrequestReview(args) => unrequest_reviewers(args, host).await,
        PrSub::Status(args) => status(args, host).await,
        PrSub::Checkout(args) => checkout(args, host).await,
        PrSub::Merge(args) => merge(args, host).await,
        PrSub::Close(args) => set_state(args, host, "closed").await,
        PrSub::Reopen(args) => set_state(args, host, "open").await,
    }
}

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
                truncate(&p.title, 55),
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

async fn diff(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let text = api::pull::diff_text(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    print!("{text}");
    Ok(())
}

async fn commits(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let cs = api::pull::commits(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let rows: Vec<Vec<String>> = cs
        .iter()
        .map(|c| {
            let short = c.sha.get(..7).unwrap_or(&c.sha).to_string();
            let subject = c.commit.message.lines().next().unwrap_or("").to_string();
            let author = c
                .commit
                .author
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_default();
            vec![short, truncate(&subject, 70), output::dim(&author)]
        })
        .collect();
    if rows.is_empty() {
        println!("(no commits)");
    } else {
        print!(
            "{}",
            output::render_table(&["SHA", "SUBJECT", "AUTHOR"], &rows)
        );
    }
    Ok(())
}

async fn files(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let fs = api::pull::files(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let rows: Vec<Vec<String>> = fs
        .iter()
        .map(|f| {
            vec![
                f.status.clone(),
                f.filename.clone(),
                output::dim(&format!("+{} -{}", f.additions, f.deletions)),
            ]
        })
        .collect();
    if rows.is_empty() {
        println!("(no file changes)");
    } else {
        print!(
            "{}",
            output::render_table(&["STATUS", "FILE", "DIFF"], &rows)
        );
    }
    Ok(())
}

async fn checks(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let cs = api::pull::combined_status(&ctx.client, &ctx.owner, &ctx.name, &pr.head.sha).await?;
    println!(
        "{}  {}",
        output::bold(&format!(
            "Combined: {}",
            if cs.state.is_empty() {
                "—"
            } else {
                &cs.state
            }
        )),
        output::dim(&format!(
            "{} checks on {}",
            cs.total_count,
            &pr.head.sha[..7.min(pr.head.sha.len())]
        )),
    );
    if cs.statuses.is_empty() {
        println!("(no per-check statuses reported)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = cs
        .statuses
        .iter()
        .map(|s| {
            let when = s
                .updated_at
                .map(output::relative_time)
                .unwrap_or_else(|| "—".into());
            vec![
                output::state_pill(&s.status.to_lowercase(), false),
                s.context.clone(),
                truncate(&s.description, 50),
                output::dim(&when),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["STATE", "CHECK", "DESCRIPTION", "WHEN"], &rows)
    );
    Ok(())
}

async fn ready(args: SimpleArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    api::pull::ready(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    println!("✓ PR #{} is ready for review", args.number);
    Ok(())
}

async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body_required = matches!(args.event, ReviewEventArg::RequestChanges);
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Some(buf.trim_end().to_string())
        }
        Some(s) => Some(s.to_string()),
        None => {
            if body_required {
                Some(editor::edit_text("PR_REVIEW.md", "")?)
            } else {
                None
            }
        }
    };
    let body = body.filter(|s| !s.trim().is_empty());
    let r = api::pull::submit_review(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        args.event.into(),
        body.as_deref(),
    )
    .await?;
    println!("✓ Submitted {} review on #{}", r.state, args.number);
    println!("{}", r.html_url);
    Ok(())
}

async fn request_reviewers(args: ReviewerArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let reviewers = expand_reviewers(args.reviewers)?;
    api::pull::request_reviewers(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        reviewers.clone(),
    )
    .await?;
    println!(
        "✓ Requested review from {} on #{}",
        reviewers.join(", "),
        args.number
    );
    Ok(())
}

async fn unrequest_reviewers(args: ReviewerArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let reviewers = expand_reviewers(args.reviewers)?;
    api::pull::unrequest_reviewers(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        reviewers.clone(),
    )
    .await?;
    println!(
        "✓ Removed review request from {} on #{}",
        reviewers.join(", "),
        args.number
    );
    Ok(())
}

/// Accept reviewers as either repeated args or comma-separated within one
/// arg, since both forms come up naturally in shell invocations.
fn expand_reviewers(raw: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        for piece in r.split(',') {
            let p = piece.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            "no reviewers provided (pass one or more usernames)"
        ));
    }
    Ok(out)
}

async fn status(args: StatusArgs, host: Option<&str>) -> Result<()> {
    // Cross-repo dashboard: PRs authored by you, and PRs where you're listed
    // as a reviewer or where you're mentioned.
    let client = Client::connect(host)?;
    let me = api::user::current(&client).await?;
    let limit = args.limit.clamp(1, 50);

    let q_authored: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("created_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];
    let q_review: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("reviewed_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];
    let q_mentioned: Vec<(String, String)> = vec![
        ("type".into(), "pulls".into()),
        ("state".into(), "open".into()),
        ("mentioned_by".into(), me.login.clone()),
        ("limit".into(), limit.to_string()),
    ];

    let path = "/api/v1/repos/issues/search";
    let authored: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_authored, None::<&()>)
        .await?;
    let review_requested: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_review, None::<&()>)
        .await?;
    let mentioned: Vec<api::issue::Issue> = client
        .json(reqwest::Method::GET, path, &q_mentioned, None::<&()>)
        .await?;

    if args.json {
        return output::print_json(&serde_json::json!({
            "authored": authored,
            "review_requested": review_requested,
            "mentioned": mentioned,
        }));
    }

    print_status_section("Created by you", &authored);
    print_status_section("Requesting your review", &review_requested);
    print_status_section("Mentioning you", &mentioned);
    Ok(())
}

fn print_status_section(label: &str, items: &[api::issue::Issue]) {
    println!("{}", output::bold(label));
    if items.is_empty() {
        println!("  (none)");
        println!();
        return;
    }
    for i in items {
        println!(
            "  #{}  {}  {}",
            i.number,
            truncate(&i.title, 70),
            output::dim(&i.html_url)
        );
    }
    println!();
}

async fn checkout(args: CheckoutArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let pr = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, args.number).await?;
    let branch = args.branch.unwrap_or_else(|| format!("pr-{}", pr.number));
    git::fetch_pr(&args.remote, pr.number, &branch)?;
    git::checkout(&branch)?;
    println!("✓ Checked out #{} into {branch}", pr.number);
    Ok(())
}

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
    fn truncate_short_string_untouched() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let out = truncate("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }
}
