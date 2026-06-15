//! `fj repo collaborator` — list/add/remove repository collaborators.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
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

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn collaborator(cmd: CollaboratorCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        CollaboratorSub::List(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            let collaborators =
                api::repo::list_collaborators(&ctx.client, &ctx.owner, &ctx.name).await?;
            if args.json {
                return output::print_json(&serde_json::to_value(&collaborators)?);
            }
            if collaborators.is_empty() {
                println!("(no collaborators)");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = collaborators
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
            println!(
                "✓ Added {} to {}/{} with {} permission",
                args.user,
                ctx.owner,
                ctx.name,
                args.permission.as_api_value()
            );
            Ok(())
        }
        CollaboratorSub::Remove(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            api::repo::remove_collaborator(&ctx.client, &ctx.owner, &ctx.name, &args.user).await?;
            println!("✓ Removed {} from {}/{}", args.user, ctx.owner, ctx.name);
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
}
