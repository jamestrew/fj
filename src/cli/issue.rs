use std::io::Read;

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::api;
use crate::api::issue::{CreateIssue, EditIssue, ListOptions, State};
use crate::client::Client;
use crate::output;

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
    /// Close an issue.
    Close(NumberOnly),
    /// Reopen a closed issue.
    Reopen(NumberOnly),
    /// Add a comment.
    Comment(CommentArgs),
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
    /// Repository slug `owner/name`.
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
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
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
    #[arg(long, default_value_t = false)]
    pub comments: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    #[arg(short = 't', long)]
    pub title: String,
    /// Body. Use `-` to read from stdin.
    #[arg(short = 'b', long)]
    pub body: Option<String>,
}

#[derive(Debug, Args)]
pub struct NumberOnly {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Args)]
pub struct CommentArgs {
    #[arg(short = 'R', long = "repo")]
    pub repo: String,
    pub number: u64,
    /// Comment body. Use `-` to read from stdin.
    #[arg(short = 'b', long)]
    pub body: String,
}

pub async fn run(cmd: IssueCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        IssueSub::List(args) => list(&client, args).await,
        IssueSub::View(args) => view(&client, args).await,
        IssueSub::Create(args) => create(&client, args).await,
        IssueSub::Close(args) => set_state(&client, args, "closed").await,
        IssueSub::Reopen(args) => set_state(&client, args, "open").await,
        IssueSub::Comment(args) => comment(&client, args).await,
    }
}

async fn list(client: &Client, args: ListArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let opts = ListOptions {
        state: args.state.into(),
        limit: args.limit,
        page: args.page,
        labels: args.label.as_deref(),
        assignee: args.assignee.as_deref(),
        query: args.query.as_deref(),
    };
    let page = api::issue::list(client, owner, name, opts).await?;
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
            vec![
                format!("#{}", i.number),
                output::state_pill(&i.state, false),
                truncate(&i.title, 70),
                output::dim(&output::relative_time(i.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["", "STATE", "TITLE", "UPDATED"], &rows)
    );
    Ok(())
}

async fn view(client: &Client, args: ViewArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let issue = api::issue::get(client, owner, name, args.number).await?;
    if args.json {
        let mut v = serde_json::to_value(&issue)?;
        if args.comments {
            let comments = api::issue::list_comments(client, owner, name, args.number).await?;
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
        let comments = api::issue::list_comments(client, owner, name, args.number).await?;
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

async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let body = read_body(args.body.as_deref())?;
    let payload = CreateIssue {
        title: &args.title,
        body: body.as_deref(),
        assignees: None,
        labels: None,
    };
    let issue = api::issue::create(client, owner, name, &payload).await?;
    println!("✓ Created issue #{}: {}", issue.number, issue.title);
    println!("{}", issue.html_url);
    Ok(())
}

async fn set_state(client: &Client, args: NumberOnly, state: &str) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let issue = api::issue::edit(
        client,
        owner,
        name,
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

async fn comment(client: &Client, args: CommentArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let body = if args.body == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        args.body
    };
    if body.trim().is_empty() {
        return Err(anyhow!("comment body is empty"));
    }
    let c = api::issue::comment(client, owner, name, args.number, &body).await?;
    println!("✓ Commented on #{} ({})", args.number, c.html_url);
    Ok(())
}

fn read_body(arg: Option<&str>) -> Result<Option<String>> {
    match arg {
        None => Ok(None),
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(Some(buf))
        }
        Some(s) => Ok(Some(s.to_string())),
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
