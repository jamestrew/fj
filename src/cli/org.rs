use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::org_team;
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
    /// Create an organization on the selected host.
    Create(CreateArgs),
    /// Create a team and manage its members and repositories.
    Team(org_team::TeamCmd),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Organization name (letters, digits, '-', '_' and '.')
    pub name: String,
    /// Display name
    #[arg(long)]
    pub full_name: Option<String>,
    /// Description
    #[arg(long)]
    pub description: Option<String>,
    /// Contact email
    #[arg(long)]
    pub email: Option<String>,
    /// Location
    #[arg(long)]
    pub location: Option<String>,
    /// Website URL
    #[arg(long)]
    pub website: Option<String>,
    /// Who can see the organization. The forge default is public.
    #[arg(long, value_enum)]
    pub visibility: Option<api::org::OrgVisibility>,
    /// Emit JSON instead of the profile view
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Organization name
    pub org: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TeamsArgs {
    /// Organization name
    pub org: String,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

pub async fn run(cmd: OrgCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        OrgSub::List(args) => list(&client, args).await,
        OrgSub::View(args) => view(&client, args).await,
        OrgSub::Teams(args) => teams(&client, args).await,
        OrgSub::Create(args) => create(&client, args).await,
        OrgSub::Team(cmd) => org_team::run(&client, cmd).await,
    }
}

/// Validates the fields locally, creates the org, then prints the profile the
/// server confirmed (the same view as `fj org view`).
#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    api::org::validate_org_name(&args.name)?;
    api::org::validate_org_fields(args.email.as_deref(), args.website.as_deref())?;
    let body = api::org::CreateOrg {
        username: &args.name,
        full_name: args.full_name.as_deref(),
        description: args.description.as_deref(),
        email: args.email.as_deref(),
        location: args.location.as_deref(),
        website: args.website.as_deref(),
        visibility: args.visibility.map(|v| v.as_api_value()),
    };
    let o = api::org::create(client, &body).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&o)?);
    }
    println!("✓ Created organization {} on {}", o.username, client.host());
    print_org(&o);
    Ok(())
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
    print_org(&o);
    Ok(())
}

fn print_org(o: &api::org::Organization) {
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
