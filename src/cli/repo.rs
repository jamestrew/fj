use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::client::Client;
use crate::config::hosts::Hosts;
use crate::git;
use crate::output;

#[derive(Debug, Args)]
pub struct RepoCmd {
    #[command(subcommand)]
    pub command: RepoSub,
}

#[derive(Debug, Subcommand)]
pub enum RepoSub {
    /// List repositories you have access to on the current host.
    List(ListArgs),
    /// View a repository.
    View(ViewArgs),
    /// Clone a repository over git.
    Clone(CloneArgs),
    /// Create a new repository.
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long, default_value_t = 1)]
    pub page: u32,
    /// Search query (uses `/repos/search`).
    #[arg(short, long)]
    pub search: Option<String>,
    /// Emit raw JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// `owner/name` slug.
    pub repo: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CloneArgs {
    /// `owner/name` slug.
    pub repo: String,
    /// Optional destination directory.
    pub dir: Option<String>,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// `[owner/]name`. Without an owner, the authenticated user is used.
    pub repo: String,
    #[arg(short, long)]
    pub description: Option<String>,
    #[arg(long, default_value_t = false)]
    pub private: bool,
    #[arg(long, default_value_t = false)]
    pub init: bool,
}

pub async fn run(cmd: RepoCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        RepoSub::List(args) => list(&client, args).await,
        RepoSub::View(args) => view(&client, args).await,
        RepoSub::Clone(args) => clone(&client, host, args).await,
        RepoSub::Create(args) => create(&client, args).await,
    }
}

async fn list(client: &Client, args: ListArgs) -> Result<()> {
    let opts = api::repo::ListOptions {
        limit: args.limit,
        page: args.page,
        query: args.search.as_deref(),
    };
    let page = if args.search.is_some() {
        api::repo::search(client, opts).await?
    } else {
        api::repo::list_for_user(client, opts).await?
    };
    if args.json {
        let v = serde_json::to_value(&page.items)?;
        return output::print_json(&v);
    }
    if page.items.is_empty() {
        println!("(no repositories)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|r| {
            let visibility = if r.private { "private" } else { "public" };
            vec![
                r.full_name.clone(),
                truncate(&r.description, 60),
                visibility.into(),
                output::dim(&output::relative_time(r.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "DESCRIPTION", "VISIBILITY", "UPDATED"], &rows)
    );
    Ok(())
}

async fn view(client: &Client, args: ViewArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let repo = api::repo::get(client, owner, name).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&repo)?);
    }
    let header = output::bold(&repo.full_name);
    let vis = if repo.private { "private" } else { "public" };
    println!("{header}  {}", output::dim(vis));
    if !repo.description.is_empty() {
        println!("{}", repo.description);
    }
    println!();
    println!("Default branch:  {}", repo.default_branch);
    println!("Stars:           {}", repo.stars_count);
    println!("Forks:           {}", repo.forks_count);
    println!("Open issues:     {}", repo.open_issues_count);
    println!("Updated:         {}", output::relative_time(repo.updated_at));
    println!();
    println!("URL:        {}", repo.html_url);
    println!("Clone URL:  {}", repo.clone_url);
    println!("SSH URL:    {}", repo.ssh_url);
    Ok(())
}

async fn clone(client: &Client, host_flag: Option<&str>, args: CloneArgs) -> Result<()> {
    let (owner, name) = api::split_repo(&args.repo)?;
    let repo = api::repo::get(client, owner, name).await?;
    let hosts = Hosts::load()?;
    let hostname = hosts.resolve_host(host_flag)?;
    let proto = hosts
        .hosts
        .get(hostname)
        .map(|h| h.git_protocol.clone())
        .unwrap_or_else(|| "https".into());
    let url = if proto == "ssh" {
        repo.ssh_url
    } else {
        repo.clone_url
    };
    git::clone(&url, args.dir.as_deref())
}

async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    let (owner, name) = match args.repo.split_once('/') {
        Some((o, n)) => (Some(o.to_string()), n.to_string()),
        None => (None, args.repo.clone()),
    };
    let body = api::repo::CreateRepo {
        name: &name,
        description: args.description.as_deref(),
        private: args.private,
        default_branch: None,
        auto_init: args.init,
    };
    let repo = match owner {
        Some(o) => {
            // Try as org first; if 404, fall through to user-namespaced.
            match api::repo::create_for_org(client, &o, &body).await {
                Ok(r) => r,
                Err(_) => api::repo::create_for_current_user(client, &body).await?,
            }
        }
        None => api::repo::create_for_current_user(client, &body).await?,
    };
    println!("✓ Created {}", repo.full_name);
    println!("{}", repo.html_url);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
