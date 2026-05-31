//! `fj repo deploy-key` — list/add/delete repository deploy keys.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::editor;
use crate::output;

#[derive(Debug, Args)]
pub struct DeployKeyCmd {
    #[command(subcommand)]
    pub command: DeployKeySub,
}

#[derive(Debug, Subcommand)]
pub enum DeployKeySub {
    /// List deploy keys on the repo.
    List(DeployKeyListArgs),
    /// Add a deploy key (read-only by default).
    Add(DeployKeyAddArgs),
    /// Delete a deploy key by id.
    Delete(DeployKeyDeleteArgs),
}

#[derive(Debug, Args)]
pub struct DeployKeyListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DeployKeyAddArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub title: String,
    /// Public key string. Use `--from-file path` (or `-` for stdin) instead for files.
    #[arg(long)]
    pub key: Option<String>,
    #[arg(long = "from-file")]
    pub from_file: Option<String>,
    /// Allow the key to push (default is read-only).
    #[arg(long)]
    pub allow_write: bool,
}

#[derive(Debug, Args)]
pub struct DeployKeyDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    pub id: u64,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub(crate) async fn deploy_key(cmd: DeployKeyCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        DeployKeySub::List(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            let keys = api::repo::list_keys(&ctx.client, &ctx.owner, &ctx.name).await?;
            if args.json {
                return output::print_json(&serde_json::to_value(&keys)?);
            }
            if keys.is_empty() {
                println!("(no deploy keys)");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = keys
                .iter()
                .map(|k| {
                    vec![
                        k.id.to_string(),
                        k.title.clone(),
                        if k.read_only {
                            "read-only"
                        } else {
                            "read-write"
                        }
                        .into(),
                        output::dim(&output::relative_time(k.created_at)),
                    ]
                })
                .collect();
            print!(
                "{}",
                output::render_table(&["ID", "TITLE", "ACCESS", "CREATED"], &rows)
            );
            Ok(())
        }
        DeployKeySub::Add(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            let key = read_key_value(args.key.as_deref(), args.from_file.as_deref())?;
            let k = api::repo::add_key(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                &args.title,
                &key,
                !args.allow_write,
            )
            .await?;
            println!(
                "✓ Added deploy key '{}' (id {}, {})",
                k.title,
                k.id,
                if k.read_only {
                    "read-only"
                } else {
                    "read-write"
                }
            );
            Ok(())
        }
        DeployKeySub::Delete(args) => {
            let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
            if !args.yes {
                let ans = editor::prompt_line(&format!(
                    "Delete deploy key {}? Type the id to confirm",
                    args.id
                ))?;
                if ans != args.id.to_string() {
                    return Err(anyhow!("aborted"));
                }
            }
            api::repo::delete_key(&ctx.client, &ctx.owner, &ctx.name, args.id).await?;
            println!("✓ Deleted deploy key {}", args.id);
            Ok(())
        }
    }
}

fn read_key_value(value: Option<&str>, from_file: Option<&str>) -> Result<String> {
    if let Some(v) = value {
        return Ok(v.trim().to_string());
    }
    if let Some(path) = from_file {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf.trim().to_string());
        }
        return Ok(std::fs::read_to_string(path)?.trim().to_string());
    }
    Err(anyhow!("pass --key or --from-file (use `-` for stdin)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_key_value_trims_inline_value() {
        assert_eq!(
            read_key_value(Some("  ssh-ed25519 AAAA  "), None).unwrap(),
            "ssh-ed25519 AAAA"
        );
    }

    #[test]
    fn read_key_value_requires_a_source() {
        assert!(read_key_value(None, None).is_err());
    }
}
