//! `fj org team`: create a team, and manage its members and repositories.
//!
//! Every mutation is followed by a read of the state it changed, and the
//! output is what that read returned, never what was requested.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::org::{OrgPermissions, Team, TeamPermission};
use crate::cli::{editor, list as list_output};
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct TeamCmd {
    #[command(subcommand)]
    pub command: TeamSub,
}

#[derive(Debug, Subcommand)]
pub enum TeamSub {
    /// Create a team in an org (needs org owner access).
    Create(CreateArgs),
    /// List a team's members.
    Members(TeamListArgs),
    /// Add a user to a team (needs org owner access).
    AddMember(MemberArgs),
    /// Remove a user from a team (needs org owner access).
    RemoveMember(MemberRemoveArgs),
    /// List the repositories a team has access to.
    Repos(TeamListArgs),
    /// Give a team access to one of the org's repositories.
    AddRepo(RepoArgs),
    /// Take a repository away from a team.
    RemoveRepo(RepoRemoveArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Organization name
    pub org: String,
    /// Team name
    pub name: String,
    /// Access the team grants on its repositories.
    #[arg(long, value_enum, default_value_t = TeamPermission::Read)]
    pub permission: TeamPermission,
    /// Team description
    #[arg(long)]
    pub description: Option<String>,
    /// Repository units the team can use, each at the team permission
    /// (comma-separated, e.g. repo.code,repo.issues). A read or write team
    /// with no unit flags gets code, issues, pulls, releases, wiki, projects,
    /// packages and actions.
    #[arg(long, value_delimiter = ',', conflicts_with = "unit_permission")]
    pub units: Vec<String>,
    /// Per-unit access as <unit>=<none|read|write|admin>. Repeatable.
    #[arg(long = "unit-permission", value_name = "UNIT=LEVEL")]
    pub unit_permission: Vec<String>,
    /// Give the team every repository in the org, including future ones.
    #[arg(long)]
    pub all_repos: bool,
    /// Let team members create repositories in the org.
    #[arg(long)]
    pub can_create_org_repo: bool,
    /// Emit JSON instead of the team view
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TeamListArgs {
    /// Organization name
    pub org: String,
    /// Team name or numeric id
    pub team: String,
    /// Maximum number of rows to fetch
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct MemberArgs {
    /// Organization name
    pub org: String,
    /// Team name or numeric id
    pub team: String,
    /// Username
    pub user: String,
}

#[derive(Debug, Args)]
pub struct MemberRemoveArgs {
    #[command(flatten)]
    pub member: MemberArgs,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct RepoArgs {
    /// Organization name
    pub org: String,
    /// Team name or numeric id
    pub team: String,
    /// Repository, as a bare name or `<org>/<name>`
    pub repo: String,
}

#[derive(Debug, Args)]
pub struct RepoRemoveArgs {
    #[command(flatten)]
    pub target: RepoArgs,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(client: &Client, cmd: TeamCmd) -> Result<()> {
    match cmd.command {
        TeamSub::Create(args) => create(client, args).await,
        TeamSub::Members(args) => members(client, args).await,
        TeamSub::AddMember(args) => set_member(client, args, true).await,
        TeamSub::RemoveMember(args) => {
            if !args.yes {
                confirm(
                    &format!(
                        "Remove {} from team '{}' in {}? Type the username to confirm",
                        args.member.user, args.member.team, args.member.org
                    ),
                    &args.member.user,
                )?;
            }
            set_member(client, args.member, false).await
        }
        TeamSub::Repos(args) => repos(client, args).await,
        TeamSub::AddRepo(args) => set_repo(client, args, true).await,
        TeamSub::RemoveRepo(args) => {
            if !args.yes {
                confirm(
                    &format!(
                        "Remove {} from team '{}' in {}? Type the repository to confirm",
                        args.target.repo, args.target.team, args.target.org
                    ),
                    &args.target.repo,
                )?;
            }
            set_repo(client, args.target, false).await
        }
    }
}

fn confirm(prompt: &str, expected: &str) -> Result<()> {
    if editor::prompt_line(prompt)? == expected {
        Ok(())
    } else {
        Err(anyhow!("aborted"))
    }
}

/// Decide whether an owner-only team mutation may be sent. Forgejo lets org
/// owners and site administrators create teams and change membership; anyone
/// else is refused here with the reason, before any mutation. `perms` is
/// `None` when the permission lookup itself failed, in which case the request
/// goes out and the forge's own answer decides.
pub(crate) fn owner_gate(
    org: &str,
    action: &str,
    perms: Option<&OrgPermissions>,
    site_admin: bool,
) -> Result<()> {
    match perms {
        Some(p) if !p.is_owner && !site_admin => Err(anyhow!(
            "permission denied: you are not an owner of org {org}, and only an owner can {action}; nothing was sent"
        )),
        _ => Ok(()),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn require_owner(client: &Client, org: &str, action: &str) -> Result<()> {
    let me = api::user::current(client).await?;
    let perms = api::org::user_permissions(client, &me.login, org)
        .await
        .ok();
    owner_gate(org, action, perms.as_ref(), me.is_admin)
}

fn denied(org: &str, action: &str) -> String {
    format!("permission denied: the forge refused to {action} in org {org}")
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn create(client: &Client, args: CreateArgs) -> Result<()> {
    let name = args.name.trim();
    if name.is_empty() {
        return Err(anyhow!("team name is required"));
    }
    let (units, units_map) =
        api::org::resolve_team_units(args.permission, &args.units, &args.unit_permission)?;
    require_owner(client, &args.org, "create a team").await?;
    let body = api::org::CreateTeam {
        name: name.to_string(),
        description: args.description.clone(),
        permission: args.permission.as_api_value(),
        includes_all_repositories: args.all_repos,
        can_create_org_repo: args.can_create_org_repo,
        units,
        units_map,
    };
    let team = api::org::create_team(client, &args.org, &body)
        .await
        .map_err(|e| api::org::with_forbidden_context(e, denied(&args.org, "create a team")))?;
    if args.json {
        return output::print_json(&serde_json::to_value(&team)?);
    }
    println!("✓ Created team '{}' in {}", team.name, args.org);
    print!("{}", team_view(&team));
    Ok(())
}

/// The confirmed team as the server returned it.
pub(crate) fn team_view(team: &Team) -> String {
    let mut out = format!("Permission: {}\n", team.permission);
    if !team.description.is_empty() {
        out.push_str(&format!("Description: {}\n", team.description));
    }
    out.push_str(&format!(
        "Repositories: {}\n",
        if team.includes_all_repositories {
            "all repositories in the org"
        } else {
            "only repositories assigned to the team"
        }
    ));
    out.push_str(&format!(
        "Can create org repositories: {}\n",
        if team.can_create_org_repo {
            "yes"
        } else {
            "no"
        }
    ));
    if !team.units_map.is_empty() {
        out.push_str("Units:\n");
        for (unit, level) in &team.units_map {
            out.push_str(&format!("  {unit}: {level}\n"));
        }
    } else if !team.units.is_empty() {
        out.push_str(&format!("Units: {}\n", team.units.join(", ")));
    }
    out
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn members(client: &Client, args: TeamListArgs) -> Result<()> {
    let team = api::org::find_team(client, &args.org, &args.team).await?;
    print_members(client, &team, args.limit, args.json).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn print_members(client: &Client, team: &Team, limit: u32, json: bool) -> Result<()> {
    let page = api::org::list_team_members(client, team.id, limit).await?;
    if json {
        return list_output::print_json(&page);
    }
    if page.items.is_empty() {
        println!("(no members)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|u| vec![u.login.clone(), u.full_name.clone()])
        .collect();
    print!("{}", output::render_table(&["USER", "NAME"], &rows));
    list_output::print_truncation_footer(&page, limit);
    Ok(())
}

/// What a membership or assignment change left on the server, as a result:
/// an error when the confirming read contradicts the change that was sent.
pub(crate) fn confirm_state(present: bool, wanted: bool, what: &str) -> Result<()> {
    match (wanted, present) {
        (true, false) => Err(anyhow!(
            "the forge accepted the change but {what} is still missing"
        )),
        (false, true) => Err(anyhow!(
            "the forge accepted the change but {what} is still present"
        )),
        _ => Ok(()),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_member(client: &Client, args: MemberArgs, add: bool) -> Result<()> {
    let action = if add {
        "add a team member"
    } else {
        "remove a team member"
    };
    let team = api::org::find_team(client, &args.org, &args.team).await?;
    require_owner(client, &args.org, action).await?;
    let sent = if add {
        api::org::add_team_member(client, team.id, &args.user).await
    } else {
        api::org::remove_team_member(client, team.id, &args.user).await
    };
    sent.map_err(|e| api::org::with_forbidden_context(e, denied(&args.org, action)))?;
    let present = api::org::is_team_member(client, team.id, &args.user).await?;
    confirm_state(
        present,
        add,
        &format!("{} in team '{}'", args.user, team.name),
    )?;
    println!(
        "✓ {} {} team '{}' in {}",
        if add { "Added" } else { "Removed" },
        if add {
            format!("{} to", args.user)
        } else {
            format!("{} from", args.user)
        },
        team.name,
        args.org
    );
    print_members(client, &team, 50, false).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn repos(client: &Client, args: TeamListArgs) -> Result<()> {
    let team = api::org::find_team(client, &args.org, &args.team).await?;
    print_repos(client, &team, args.limit, args.json).await
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn print_repos(client: &Client, team: &Team, limit: u32, json: bool) -> Result<()> {
    let page = api::org::list_team_repos(client, team.id, limit).await?;
    if json {
        return list_output::print_json(&page);
    }
    if page.items.is_empty() {
        println!("(no repositories)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|r| {
            vec![
                r.full_name.clone(),
                if r.private { "private" } else { "public" }.to_string(),
                output::truncate(&r.description, 60),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["REPOSITORY", "VISIBILITY", "DESCRIPTION"], &rows)
    );
    list_output::print_truncation_footer(&page, limit);
    Ok(())
}

/// A team only holds repositories of its own org. Accepts a bare name or
/// `<org>/<name>` and returns the bare name, refusing another owner's repo.
pub(crate) fn team_repo_name<'a>(org: &str, repo: &'a str) -> Result<&'a str> {
    let name = match repo.split_once('/') {
        Some((owner, name)) => {
            if !owner.eq_ignore_ascii_case(org) {
                return Err(anyhow!(
                    "a team in {org} can only hold {org}'s repositories, not '{repo}'"
                ));
            }
            name
        }
        None => repo,
    };
    if name.is_empty() || name.contains('/') {
        return Err(anyhow!("expected '<name>' or '{org}/<name>', got '{repo}'"));
    }
    Ok(name)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn set_repo(client: &Client, args: RepoArgs, add: bool) -> Result<()> {
    let repo = team_repo_name(&args.org, &args.repo)?;
    let action = if add {
        "assign a repository to a team (needs admin access to the repository)"
    } else {
        "remove a repository from a team (needs admin access to the repository)"
    };
    let team = api::org::find_team(client, &args.org, &args.team).await?;
    let sent = if add {
        api::org::add_team_repo(client, team.id, &args.org, repo).await
    } else {
        api::org::remove_team_repo(client, team.id, &args.org, repo).await
    };
    sent.map_err(|e| api::org::with_forbidden_context(e, denied(&args.org, action)))?;
    let present = api::org::team_has_repo(client, team.id, &args.org, repo).await?;
    confirm_state(
        present,
        add,
        &format!("{}/{repo} in team '{}'", args.org, team.name),
    )?;
    println!(
        "✓ {} {}/{repo} {} team '{}'",
        if add { "Assigned" } else { "Removed" },
        args.org,
        if add { "to" } else { "from" },
        team.name
    );
    print_repos(client, &team, 50, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::org::{OrgCmd, OrgSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn team_sub(argv: &[&str]) -> TeamSub {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Org(OrgCmd {
                command: OrgSub::Team(TeamCmd { command }),
            }) => command,
            other => panic!("expected org team, got {other:?}"),
        }
    }

    #[test]
    fn create_parses_permission_units_and_flags() {
        match team_sub(&[
            "fj",
            "org",
            "team",
            "create",
            "acme",
            "devs",
            "--permission",
            "write",
            "--units",
            "repo.code,repo.issues",
            "--all-repos",
            "--can-create-org-repo",
        ]) {
            TeamSub::Create(a) => {
                assert_eq!((a.org.as_str(), a.name.as_str()), ("acme", "devs"));
                assert_eq!(a.permission, TeamPermission::Write);
                assert_eq!(a.units, vec!["repo.code", "repo.issues"]);
                assert!(a.all_repos && a.can_create_org_repo);
            }
            other => panic!("expected create, got {other:?}"),
        }
    }

    #[test]
    fn create_defaults_to_read_and_rejects_both_unit_flags() {
        match team_sub(&["fj", "org", "team", "create", "acme", "devs"]) {
            TeamSub::Create(a) => assert_eq!(a.permission, TeamPermission::Read),
            other => panic!("expected create, got {other:?}"),
        }
        assert!(Cli::try_parse_from([
            "fj",
            "org",
            "team",
            "create",
            "acme",
            "devs",
            "--units",
            "repo.code",
            "--unit-permission",
            "repo.wiki=read",
        ])
        .is_err());
    }

    #[test]
    fn remove_member_prompts_unless_yes() {
        match team_sub(&["fj", "org", "team", "remove-member", "acme", "devs", "bob"]) {
            TeamSub::RemoveMember(a) => {
                assert!(!a.yes);
                assert_eq!(a.member.user, "bob");
            }
            other => panic!("expected remove-member, got {other:?}"),
        }
        match team_sub(&[
            "fj",
            "org",
            "team",
            "remove-repo",
            "acme",
            "devs",
            "web",
            "-y",
        ]) {
            TeamSub::RemoveRepo(a) => assert!(a.yes),
            other => panic!("expected remove-repo, got {other:?}"),
        }
    }

    #[test]
    fn existing_org_teams_listing_still_parses() {
        let cli = Cli::try_parse_from(["fj", "org", "teams", "acme", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Org(OrgCmd {
                command: OrgSub::Teams(_)
            })
        ));
    }

    #[test]
    fn owner_gate_refuses_a_non_owner_before_sending() {
        let member = OrgPermissions {
            can_read: true,
            ..Default::default()
        };
        let err = owner_gate("acme", "create a team", Some(&member), false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("permission denied"), "{msg}");
        assert!(msg.contains("nothing was sent"), "{msg}");
    }

    #[test]
    fn owner_gate_allows_owner_site_admin_and_unknown() {
        let owner = OrgPermissions {
            is_owner: true,
            ..Default::default()
        };
        let member = OrgPermissions::default();
        assert!(owner_gate("acme", "x", Some(&owner), false).is_ok());
        assert!(owner_gate("acme", "x", Some(&member), true).is_ok());
        // An unreadable permission set defers to the forge's own answer.
        assert!(owner_gate("acme", "x", None, false).is_ok());
    }

    #[test]
    fn confirm_state_flags_a_contradicting_read() {
        assert!(confirm_state(true, true, "bob").is_ok());
        assert!(confirm_state(false, false, "bob").is_ok());
        assert!(confirm_state(false, true, "bob")
            .unwrap_err()
            .to_string()
            .contains("still missing"));
        assert!(confirm_state(true, false, "bob")
            .unwrap_err()
            .to_string()
            .contains("still present"));
    }

    #[test]
    fn team_repo_name_accepts_bare_or_same_org_slug() {
        assert_eq!(team_repo_name("acme", "web").unwrap(), "web");
        assert_eq!(team_repo_name("acme", "Acme/web").unwrap(), "web");
        assert!(team_repo_name("acme", "other/web").is_err());
        assert!(team_repo_name("acme", "acme/").is_err());
        assert!(team_repo_name("acme", "acme/a/b").is_err());
    }

    #[test]
    fn team_view_shows_the_confirmed_units() {
        let team: Team = serde_json::from_value(serde_json::json!({
            "id": 3,
            "name": "devs",
            "permission": "write",
            "includes_all_repositories": true,
            "units_map": { "repo.code": "write", "repo.wiki": "read" }
        }))
        .unwrap();
        let view = team_view(&team);
        assert!(view.contains("Permission: write"), "{view}");
        assert!(view.contains("all repositories in the org"), "{view}");
        assert!(view.contains("  repo.code: write"), "{view}");
        assert!(view.contains("Can create org repositories: no"), "{view}");
    }
}
