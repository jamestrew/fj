//! `fj gist` — Forgejo Gists (snippets).
//!
//! Forgejo doesn't ship a Gist surface separate from regular repos; we expose
//! a thin wrapper that lists/views the current user's "fj-gist-*" repos and
//! creates new ones. Power users should fall through to `fj api`.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct GistCmd {
    #[command(subcommand)]
    pub command: GistSub,
}

#[derive(Debug, Subcommand)]
pub enum GistSub {
    /// List your gist-style repos (any repo prefixed `gist-`).
    List(ListArgs),
    /// Create a new gist (private repo with a single file).
    Create(CreateArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Gist name (used as the repo name, prefixed with `gist-`).
    pub name: String,
    /// Optional description.
    #[arg(short = 'd', long)]
    pub description: Option<String>,
    /// Make it private (the default).
    #[arg(long, default_value_t = true)]
    pub private: bool,
}

pub async fn run(cmd: GistCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        GistSub::List(args) => list(&client, args).await,
        GistSub::Create(args) => create(&client, args).await,
    }
}

async fn list(client: &Client, args: ListArgs) -> Result<()> {
    let page = api::repo::list_for_user(
        client,
        api::repo::ListOptions {
            limit: 50,
            page: 1,
            query: None,
        },
    )
    .await?;
    let gists: Vec<_> = page
        .items
        .into_iter()
        .filter(|r| r.name.starts_with("gist-"))
        .collect();
    if args.json {
        return output::print_json(&serde_json::to_value(&gists)?);
    }
    if gists.is_empty() {
        println!("(no gists)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = gists
        .iter()
        .map(|g| {
            vec![
                g.full_name.clone(),
                g.description.clone(),
                output::dim(&output::relative_time(g.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "DESCRIPTION", "UPDATED"], &rows)
    );
    Ok(())
}

async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    let name = format!("gist-{}", args.name);
    let body = api::repo::CreateRepo {
        name: &name,
        description: args.description.as_deref(),
        private: args.private,
        default_branch: None,
        auto_init: true,
    };
    let repo = api::repo::create_for_current_user(client, &body).await?;
    println!("✓ Created gist {}", repo.full_name);
    println!("{}", repo.html_url);
    Ok(())
}
