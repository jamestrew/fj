use std::io::Read;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::State;
use crate::api::pull::{CreatePull, EditPull, ListOptions, MergeStyle};
use crate::client::Client;
use crate::git;
use crate::output;

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
    /// Check out a pull request locally.
    Checkout(CheckoutArgs),
    /// Merge a pull request.
    Merge(MergeArgs),
    /// Close a pull request without merging.
    Close(NumberOnly),
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

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    #[arg(short = 's', long, value_enum, default_value_t = StateFilter::Open)]
    pub state: StateFilter,
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    #[arg(short = 't', long)]
    pub title: String,
    #[arg(short = 'B', long, default_value = "main")]
    pub base: String,
    /// Head branch. Either a branch name on the same repo, or `owner:branch`.
    #[arg(short = 'H', long)]
    pub head: String,
    /// Body. Use `-` to read from stdin.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
}

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
    /// Override the local branch name.
    #[arg(short = 'b', long)]
    pub branch: Option<String>,
    /// Git remote to fetch from.
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
    #[arg(long, value_enum, default_value_t = MergeStyleArg::Merge)]
    pub style: MergeStyleArg,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub message: Option<String>,
}

#[derive(Debug, Args)]
pub struct NumberOnly {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
}

pub async fn run(cmd: PrCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        PrSub::List(args) => list(&client, args).await,
        PrSub::View(args) => view(&client, args).await,
        PrSub::Create(args) => create(&client, args).await,
        PrSub::Checkout(args) => checkout(&client, args).await,
        PrSub::Merge(args) => merge(&client, args).await,
        PrSub::Close(args) => close(&client, args).await,
    }
}

async fn list(client: &Client, args: ListArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let page = api::pull::list(
        client,
        owner,
        name,
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
                truncate(&p.title, 60),
                output::dim(&format!("{} → {}", branch_label(&p.head), branch_label(&p.base))),
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

async fn view(client: &Client, args: ViewArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let pr = api::pull::get(client, owner, name, args.number).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&pr)?);
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
    Ok(())
}

async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Some(buf)
        }
        other => other.map(|s| s.to_string()),
    };
    let pr = api::pull::create(
        client,
        owner,
        name,
        &CreatePull {
            title: &args.title,
            head: &args.head,
            base: &args.base,
            body: body.as_deref(),
        },
    )
    .await?;
    println!("✓ Created PR #{}: {}", pr.number, pr.title);
    println!("{}", pr.html_url);
    Ok(())
}

async fn checkout(client: &Client, args: CheckoutArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let pr = api::pull::get(client, owner, name, args.number).await?;
    let branch = args.branch.unwrap_or_else(|| format!("pr-{}", pr.number));
    git::fetch_pr(&args.remote, pr.number, &branch)?;
    git::checkout(&branch)?;
    println!("✓ Checked out #{} into {branch}", pr.number);
    Ok(())
}

async fn merge(client: &Client, args: MergeArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    api::pull::merge(
        client,
        owner,
        name,
        args.number,
        args.style.into(),
        args.title.as_deref(),
        args.message.as_deref(),
    )
    .await?;
    println!("✓ Merged PR #{}", args.number);
    Ok(())
}

async fn close(client: &Client, args: NumberOnly) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    api::pull::edit(
        client,
        owner,
        name,
        args.number,
        &EditPull {
            state: Some("closed"),
            ..Default::default()
        },
    )
    .await?;
    println!("✓ Closed PR #{}", args.number);
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

