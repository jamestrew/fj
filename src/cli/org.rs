use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct OrgCmd {
    #[command(subcommand)]
    pub command: OrgSub,
}

#[derive(Debug, Subcommand)]
pub enum OrgSub {
    /// List orgs you belong to.
    List(ListArgs),
    /// Show details about a specific org.
    View(ViewArgs),
    /// List teams in an org.
    Teams(TeamsArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    pub org: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TeamsArgs {
    pub org: String,
    #[arg(long)]
    pub json: bool,
}

pub async fn run(cmd: OrgCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        OrgSub::List(args) => list(&client, args).await,
        OrgSub::View(args) => view(&client, args).await,
        OrgSub::Teams(args) => teams(&client, args).await,
    }
}

async fn list(client: &Client, args: ListArgs) -> Result<()> {
    let items = api::org::list_for_user(client).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no orgs)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|o| {
            vec![
                o.username.clone(),
                o.full_name.clone(),
                o.visibility.clone(),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["LOGIN", "NAME", "VISIBILITY"], &rows)
    );
    Ok(())
}

async fn view(client: &Client, args: ViewArgs) -> Result<()> {
    let o = api::org::get(client, &args.org).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&o)?);
    }
    println!("{}", output::bold(&o.username));
    if !o.full_name.is_empty() {
        println!("{}", o.full_name);
    }
    if !o.description.is_empty() {
        println!();
        println!("{}", o.description);
    }
    println!();
    println!("Visibility: {}", o.visibility);
    Ok(())
}

async fn teams(client: &Client, args: TeamsArgs) -> Result<()> {
    let teams = api::org::list_teams(client, &args.org).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&teams)?);
    }
    if teams.is_empty() {
        println!("(no teams)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = teams
        .iter()
        .map(|t| vec![t.name.clone(), t.permission.clone(), t.description.clone()])
        .collect();
    print!(
        "{}",
        output::render_table(&["TEAM", "PERMISSION", "DESCRIPTION"], &rows)
    );
    Ok(())
}
