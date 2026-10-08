//! `fj repo collaborator` — list/add/remove repository collaborators.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::list as list_output;
use crate::output;

#[derive(Debug, Args)]
pub struct CollaboratorCmd {
    #[command(subcommand)]
    pub command: CollaboratorSub,
}

#[derive(Debug, Subcommand)]
pub enum CollaboratorSub {
    /// List repository collaborators.
    List(CollaboratorListArgs),
    /// Add or update a repository collaborator.
    Add(CollaboratorAddArgs),
    /// Remove a repository collaborator.
    Remove(CollaboratorRemoveArgs),
}

#[derive(Debug, Args)]
pub struct CollaboratorListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Maximum number of collaborators to fetch
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CollaboratorAddArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Username to add or update.
    pub user: String,
    /// Permission to grant.
    #[arg(long, value_enum, default_value_t = api::repo::CollaboratorPermission::Write)]
    pub permission: api::repo::CollaboratorPermission,
}

#[derive(Debug, Args)]
pub struct CollaboratorRemoveArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Username to remove.
    pub user: String,
}

/// The line printed after an add: the access the forge confirms, with a note
/// when that differs from what was asked for (an owner or an org team can
/// already hold more than the grant).
pub(crate) fn added_line(user: &str, slug: &str, requested: &str, confirmed: &str) -> String {
    if confirmed == requested {
        format!("✓ {user} has {confirmed} access to {slug}")
    } else {
        format!("✓ {user} has {confirmed} access to {slug} (requested {requested})")
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn collaborator(cmd: CollaboratorCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        CollaboratorSub::List(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            let mut page = api::repo::list_collaborators_page(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                Some(args.limit),
            )
            .await?;
            let undisclosed = api::repo::resolve_collaborator_permissions(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                &mut page.items,
            )
            .await?;
            if undisclosed > 0 {
                eprintln!(
                    "note: the forge would not disclose the permission of {undisclosed} collaborator(s) to this account (repository admin access shows them all); they are listed as unknown"
                );
            }
            if args.json {
                return output::print_json(&serde_json::to_value(&page.items)?);
            }
            if page.items.is_empty() {
                println!("(no collaborators)");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = page
                .items
                .iter()
                .map(|c| {
                    vec![
                        c.user.login.clone(),
                        c.permission_level().to_string(),
                        c.user.full_name.clone(),
                    ]
                })
                .collect();
            print!(
                "{}",
                output::render_table(&["USER", "PERMISSION", "NAME"], &rows)
            );
            list_output::print_truncation_footer(&page, args.limit);
            Ok(())
        }
        CollaboratorSub::Add(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            api::repo::add_collaborator(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                &args.user,
                args.permission,
            )
            .await?;
            // Report what the forge now holds, not what was asked for.
            let access =
                api::repo::collaborator_permission(&ctx.client, &ctx.owner, &ctx.name, &args.user)
                    .await?;
            println!(
                "{}",
                added_line(
                    &args.user,
                    &ctx.slug(),
                    args.permission.as_api_value(),
                    &access.permission
                )
            );
            Ok(())
        }
        CollaboratorSub::Remove(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            api::repo::remove_collaborator(&ctx.client, &ctx.owner, &ctx.name, &args.user).await?;
            if api::repo::is_collaborator(&ctx.client, &ctx.owner, &ctx.name, &args.user).await? {
                return Err(anyhow::anyhow!(
                    "the forge accepted the removal but {} is still a collaborator on {}",
                    args.user,
                    ctx.slug()
                ));
            }
            println!("✓ Removed {} from {}", args.user, ctx.slug());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn collaborator_add_defaults_to_write_permission() {
        let cli = Cli::try_parse_from(["fj", "repo", "collaborator", "add", "alice"])
            .expect("should parse");
        match cli.command {
            Command::Repo(crate::cli::repo::RepoCmd {
                command:
                    crate::cli::repo::RepoSub::Collaborator(CollaboratorCmd {
                        command: CollaboratorSub::Add(args),
                    }),
            }) => assert_eq!(args.permission, api::repo::CollaboratorPermission::Write),
            other => panic!("expected repo collaborator add, got {other:?}"),
        }
    }

    #[test]
    fn collaborator_add_parses_admin_permission() {
        let cli = Cli::try_parse_from([
            "fj",
            "repo",
            "collaborator",
            "add",
            "alice",
            "--permission",
            "admin",
        ])
        .expect("should parse");
        match cli.command {
            Command::Repo(crate::cli::repo::RepoCmd {
                command:
                    crate::cli::repo::RepoSub::Collaborator(CollaboratorCmd {
                        command: CollaboratorSub::Add(args),
                    }),
            }) => assert_eq!(args.permission, api::repo::CollaboratorPermission::Admin),
            other => panic!("expected repo collaborator add, got {other:?}"),
        }
    }

    #[test]
    fn collaborator_list_takes_a_limit() {
        let cli = Cli::try_parse_from(["fj", "repo", "collaborator", "list", "-L", "5"])
            .expect("should parse");
        match cli.command {
            Command::Repo(crate::cli::repo::RepoCmd {
                command:
                    crate::cli::repo::RepoSub::Collaborator(CollaboratorCmd {
                        command: CollaboratorSub::List(args),
                    }),
            }) => assert_eq!(args.limit, 5),
            other => panic!("expected repo collaborator list, got {other:?}"),
        }
    }

    #[test]
    fn added_line_reports_the_confirmed_permission() {
        assert_eq!(
            added_line("alice", "o/r", "read", "read"),
            "✓ alice has read access to o/r"
        );
        assert_eq!(
            added_line("alice", "o/r", "read", "owner"),
            "✓ alice has owner access to o/r (requested read)"
        );
    }
}
