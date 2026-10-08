//! `fj auth group`: user-named groups over the configured hosts.
//!
//! A group is an organizational label and nothing more. It is stored next to
//! the hosts in hosts.toml, shown by `fj auth list` and `fj auth status`, and
//! read by nothing that authenticates, resolves, or switches a host.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::config::hosts::Hosts;
use crate::output;

#[derive(Debug, Args)]
pub struct GroupCmd {
    #[command(subcommand)]
    pub command: GroupSub,
}

#[derive(Debug, Subcommand)]
pub enum GroupSub {
    /// List groups and the hosts assigned to each.
    List(ListArgs),
    /// Create an empty group.
    Create(CreateArgs),
    /// Rename a group, keeping its hosts.
    Rename(RenameArgs),
    /// Delete a group. Its hosts become ungrouped and are otherwise unchanged.
    Delete(DeleteArgs),
    /// Put a host in a group. A host is in at most one group, so this
    /// replaces any group it was in before.
    Assign(AssignArgs),
    /// Take a host out of its group.
    Unassign(UnassignArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Group name
    pub name: String,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Current group name
    pub name: String,
    /// New group name
    pub new_name: String,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Group name
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct AssignArgs {
    /// Group to put the host in
    pub group: String,
    /// Host to assign (defaults to the current host)
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Debug, Args)]
pub struct UnassignArgs {
    /// Host to take out of its group (defaults to the current host)
    #[arg(long)]
    pub host: Option<String>,
}

pub fn run(cmd: GroupCmd, global_host: Option<&str>) -> Result<()> {
    match cmd.command {
        GroupSub::List(args) => list(args),
        GroupSub::Create(args) => create(args),
        GroupSub::Rename(args) => rename(args),
        GroupSub::Delete(args) => delete(args),
        GroupSub::Assign(args) => assign(args, global_host),
        GroupSub::Unassign(args) => unassign(args, global_host),
    }
}

/// `--json` shape for `fj auth group list`: one object per group, in name
/// order, each with its hosts.
fn groups_json(hosts: &Hosts) -> serde_json::Value {
    serde_json::Value::Array(
        hosts
            .groups
            .iter()
            .map(|(name, members)| serde_json::json!({ "name": name, "hosts": members }))
            .collect(),
    )
}

fn group_rows(hosts: &Hosts) -> Vec<Vec<String>> {
    hosts
        .groups
        .iter()
        .map(|(name, members)| vec![name.clone(), members.join(", ")])
        .collect()
}

/// Headers and rows for `fj auth list`. The GROUP column appears only once a
/// group exists, so the output is unchanged for anyone who never made one.
pub(crate) fn host_table(hosts: &Hosts) -> (Vec<&'static str>, Vec<Vec<String>>) {
    let grouped = !hosts.groups.is_empty();
    let mut headers = vec!["", "HOST", "USER", "AUTH", "PROTOCOL"];
    if grouped {
        headers.push("GROUP");
    }
    let rows = hosts
        .hosts
        .iter()
        .map(|(name, h)| {
            let current = if hosts.current.as_deref() == Some(name) {
                "*"
            } else {
                ""
            };
            let scheme = match h.auth_kind {
                crate::config::hosts::AuthKind::Pat => "pat",
                crate::config::hosts::AuthKind::FjordAccount => "fjord-account",
            };
            let mut row = vec![
                current.into(),
                name.clone(),
                h.user.clone().unwrap_or_default(),
                scheme.into(),
                h.git_protocol.clone(),
            ];
            if grouped {
                row.push(hosts.group_of(name).unwrap_or_default().to_string());
            }
            row
        })
        .collect();
    (headers, rows)
}

/// The `fj auth status` line for a host's group, or `None` when ungrouped.
pub(crate) fn status_line(hosts: &Hosts, host: &str) -> Option<String> {
    hosts
        .group_of(host)
        .map(|group| format!("  Group: {group}"))
}

fn list(args: ListArgs) -> Result<()> {
    let hosts = Hosts::load()?;
    if args.json {
        return output::print_json(&groups_json(&hosts));
    }
    if hosts.groups.is_empty() {
        println!("(no groups; create one with `fj auth group create <name>`)");
        return Ok(());
    }
    print!(
        "{}",
        output::render_table(&["GROUP", "HOSTS"], &group_rows(&hosts))
    );
    Ok(())
}

fn create(args: CreateArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let name = hosts.create_group(&args.name)?;
    hosts.save()?;
    println!("✓ Created group '{name}'");
    Ok(())
}

fn rename(args: RenameArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let new = hosts.rename_group(&args.name, &args.new_name)?;
    hosts.save()?;
    println!("✓ Renamed group '{}' to '{new}'", args.name.trim());
    Ok(())
}

fn delete(args: DeleteArgs) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let name = args.name.trim().to_string();
    if !hosts.groups.contains_key(&name) {
        return Err(anyhow!("no group named '{name}'"));
    }
    if !args.yes {
        let answer = crate::cli::editor::prompt_line(&format!(
            "Delete group '{name}'? Type the name to confirm"
        ))?;
        if answer != name {
            return Err(anyhow!("aborted"));
        }
    }
    let freed = hosts.delete_group(&name)?;
    hosts.save()?;
    println!("{}", deleted_message(&name, &freed));
    Ok(())
}

fn deleted_message(name: &str, freed: &[String]) -> String {
    if freed.is_empty() {
        format!("✓ Deleted group '{name}'")
    } else {
        format!(
            "✓ Deleted group '{name}'. Now ungrouped: {}",
            freed.join(", ")
        )
    }
}

fn assign(args: AssignArgs, global_host: Option<&str>) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let explicit = args.host.as_deref().or(global_host);
    let host = hosts.resolve_host(explicit)?.to_string();
    let group = args.group.trim().to_string();
    let previous = hosts.assign_group(&host, &group)?;
    hosts.save()?;
    println!("{}", assigned_message(&host, &group, previous.as_deref()));
    Ok(())
}

fn assigned_message(host: &str, group: &str, previous: Option<&str>) -> String {
    match previous {
        Some(previous) => format!("✓ {host} is now in group '{group}' (was in '{previous}')"),
        None => format!("✓ {host} is now in group '{group}'"),
    }
}

fn unassign(args: UnassignArgs, global_host: Option<&str>) -> Result<()> {
    let mut hosts = Hosts::load()?;
    let explicit = args.host.as_deref().or(global_host);
    let host = hosts.resolve_host(explicit)?.to_string();
    match hosts.unassign_group(&host) {
        Some(previous) => {
            hosts.save()?;
            println!("✓ {host} is no longer in group '{previous}'");
        }
        None => println!("{host} is not in a group"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::auth::{AuthCmd, AuthSub};
    use crate::cli::{Cli, Command};
    use crate::config::hosts::Host;
    use clap::Parser;

    fn group_sub(argv: &[&str]) -> GroupSub {
        let cli = Cli::try_parse_from(argv).expect("parses");
        match cli.command {
            Command::Auth(AuthCmd {
                command: AuthSub::Group(cmd),
            }) => cmd.command,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn group_subcommands_parse() {
        assert!(matches!(
            group_sub(&["fj", "auth", "group", "list", "--json"]),
            GroupSub::List(ListArgs { json: true })
        ));
        match group_sub(&["fj", "auth", "group", "create", "work"]) {
            GroupSub::Create(args) => assert_eq!(args.name, "work"),
            other => panic!("{other:?}"),
        }
        match group_sub(&["fj", "auth", "group", "rename", "work", "clients"]) {
            GroupSub::Rename(args) => {
                assert_eq!(args.name, "work");
                assert_eq!(args.new_name, "clients");
            }
            other => panic!("{other:?}"),
        }
        match group_sub(&["fj", "auth", "group", "delete", "work", "-y"]) {
            GroupSub::Delete(args) => assert!(args.yes),
            other => panic!("{other:?}"),
        }
        match group_sub(&[
            "fj",
            "auth",
            "group",
            "assign",
            "work",
            "--host",
            "a.example",
        ]) {
            GroupSub::Assign(args) => {
                assert_eq!(args.group, "work");
                assert_eq!(args.host.as_deref(), Some("a.example"));
            }
            other => panic!("{other:?}"),
        }
        match group_sub(&["fj", "auth", "group", "unassign"]) {
            GroupSub::Unassign(args) => assert!(args.host.is_none()),
            other => panic!("{other:?}"),
        }
    }

    fn hosts_with_groups() -> Hosts {
        let mut hosts = Hosts::default();
        hosts.upsert("a.example", Host::default());
        hosts.upsert("b.example", Host::default());
        hosts.create_group("work").unwrap();
        hosts.create_group("empty").unwrap();
        hosts.assign_group("a.example", "work").unwrap();
        hosts.assign_group("b.example", "work").unwrap();
        hosts
    }

    #[test]
    fn group_rows_list_every_group_including_empty_ones() {
        let rows = group_rows(&hosts_with_groups());
        assert_eq!(
            rows,
            vec![
                vec!["empty".to_string(), String::new()],
                vec!["work".to_string(), "a.example, b.example".to_string()],
            ]
        );
    }

    #[test]
    fn groups_json_shape() {
        let json = groups_json(&hosts_with_groups());
        assert_eq!(json[0]["name"], "empty");
        assert_eq!(json[0]["hosts"], serde_json::json!([]));
        assert_eq!(json[1]["name"], "work");
        assert_eq!(
            json[1]["hosts"],
            serde_json::json!(["a.example", "b.example"])
        );
        assert_eq!(groups_json(&Hosts::default()), serde_json::json!([]));
    }

    #[test]
    fn auth_list_table_is_unchanged_until_a_group_exists() {
        let mut hosts = Hosts::default();
        hosts.upsert("a.example", Host::default());
        let (headers, rows) = host_table(&hosts);
        assert_eq!(headers, vec!["", "HOST", "USER", "AUTH", "PROTOCOL"]);
        assert_eq!(rows[0].len(), 5);
        assert_eq!(status_line(&hosts, "a.example"), None);
    }

    #[test]
    fn auth_list_and_status_show_the_group() {
        let mut hosts = hosts_with_groups();
        hosts.unassign_group("b.example");
        let (headers, rows) = host_table(&hosts);
        assert_eq!(headers.last(), Some(&"GROUP"));
        assert_eq!(rows[0][0], "*");
        assert_eq!(rows[0][1], "a.example");
        assert_eq!(rows[0][5], "work");
        assert_eq!(rows[1][5], "");
        assert_eq!(
            status_line(&hosts, "a.example").as_deref(),
            Some("  Group: work")
        );
        assert_eq!(status_line(&hosts, "b.example"), None);
    }

    #[test]
    fn messages_report_what_changed() {
        assert_eq!(deleted_message("work", &[]), "✓ Deleted group 'work'");
        assert_eq!(
            deleted_message("work", &["a.example".into(), "b.example".into()]),
            "✓ Deleted group 'work'. Now ungrouped: a.example, b.example"
        );
        assert_eq!(
            assigned_message("a.example", "work", None),
            "✓ a.example is now in group 'work'"
        );
        assert_eq!(
            assigned_message("a.example", "work", Some("personal")),
            "✓ a.example is now in group 'work' (was in 'personal')"
        );
    }
}
