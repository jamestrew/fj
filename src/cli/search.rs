use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct SearchCmd {
    #[command(subcommand)]
    pub command: SearchSub,
}

#[derive(Debug, Subcommand)]
pub enum SearchSub {
    /// Search repositories.
    Repos(QueryArgs),
    /// Search issues across all repos you can see.
    Issues(QueryArgs),
    /// Search pull requests across all repos you can see.
    Prs(QueryArgs),
    /// Search users.
    Users(QueryArgs),
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    pub query: String,
    #[arg(short = 'L', long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
}

pub async fn run(cmd: SearchCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        SearchSub::Repos(args) => search_repos(&client, args).await,
        SearchSub::Issues(args) => search_issues(&client, args, "issues").await,
        SearchSub::Prs(args) => search_issues(&client, args, "pulls").await,
        SearchSub::Users(args) => search_users(&client, args).await,
    }
}

async fn search_repos(client: &Client, args: QueryArgs) -> Result<()> {
    let hits = api::search::repos(client, &args.query, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&hits)?);
    }
    if hits.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|r| {
            let vis = if r.private { "private" } else { "public" };
            vec![
                r.full_name.clone(),
                output::truncate(&r.description, 60),
                vis.into(),
                output::dim(&output::relative_time(r.updated_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "DESCRIPTION", "VIS", "UPDATED"], &rows)
    );
    Ok(())
}

async fn search_issues(client: &Client, args: QueryArgs, type_: &str) -> Result<()> {
    let hits = api::search::issues(client, &args.query, type_, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&hits)?);
    }
    if hits.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|i| {
            vec![
                format!("#{}", i.number),
                output::state_pill(&i.state, false),
                output::truncate(&i.title, 60),
                output::dim(&i.html_url),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["", "STATE", "TITLE", "URL"], &rows)
    );
    Ok(())
}

async fn search_users(client: &Client, args: QueryArgs) -> Result<()> {
    let hits = api::search::users(client, &args.query, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&hits)?);
    }
    if hits.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|u| vec![u.login.clone(), u.full_name.clone(), u.email.clone()])
        .collect();
    print!(
        "{}",
        output::render_table(&["LOGIN", "NAME", "EMAIL"], &rows)
    );
    Ok(())
}
