//! `fj secret` — Forgejo Actions secrets (list/set/delete).
//!
//! Hosts the shared `SecretListArgs` / `SecretSetArgs` / `SecretDeleteArgs`
//! and `read_value`, reused by the variable group in `workflow_variable`.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct SecretCmd {
    #[command(subcommand)]
    pub command: SecretSub,
}

#[derive(Debug, Subcommand)]
pub enum SecretSub {
    /// List secret names (values are write-only).
    List(SecretListArgs),
    /// Set or replace a secret.
    Set(SecretSetArgs),
    /// Delete a secret.
    Delete(SecretDeleteArgs),
}

#[derive(Debug, Args)]
pub struct SecretListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope for shared Actions secrets/variables.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SecretSetArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope for shared Actions secrets/variables.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Secret name
    pub name: String,
    /// Value (raw string). For multi-line secrets, prefer `--from-file -` and pipe stdin.
    #[arg(short = 'v', long)]
    pub value: Option<String>,
    /// Read value from this file. Use `-` for stdin.
    #[arg(short = 'f', long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct SecretDeleteArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope for shared Actions secrets/variables.
    #[arg(long, value_name = "ORG", conflicts_with = "repo")]
    pub org: Option<String>,
    /// Secret name
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub(crate) enum WorkflowScope {
    Repo(RepoContext),
    Org { client: Client, org: String },
}

pub(crate) fn resolve_workflow_scope(
    repo: Option<&str>,
    org: Option<&str>,
    host: Option<&str>,
) -> Result<WorkflowScope> {
    if let Some(org) = org {
        if repo.is_some() {
            bail!("--org cannot be used with --repo/-R");
        }
        return Ok(WorkflowScope::Org {
            client: Client::connect(host)?,
            org: org.to_string(),
        });
    }

    Ok(WorkflowScope::Repo(resolve_repo(repo, host)?))
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run_secret(cmd: SecretCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        SecretSub::List(args) => secret_list(args, host).await,
        SecretSub::Set(args) => secret_set(args, host).await,
        SecretSub::Delete(args) => secret_delete(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_list(args: SecretListArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    let s = match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::list_secrets(&ctx.client, &ctx.owner, &ctx.name).await?
        }
        WorkflowScope::Org { client, org } => api::workflow::list_org_secrets(client, org).await?,
    };
    if args.json {
        return output::print_json(&serde_json::to_value(&s)?);
    }
    if s.is_empty() {
        println!("(no secrets)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = s
        .iter()
        .map(|x| {
            vec![
                x.name.clone(),
                output::dim(&output::relative_time(x.created_at)),
            ]
        })
        .collect();
    print!("{}", output::render_table(&["NAME", "CREATED"], &rows));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_set(args: SecretSetArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    let value = read_value(args.value.as_deref(), args.from_file.as_deref())?;
    match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::set_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name, &value)
                .await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::set_org_secret(client, org, &args.name, &value).await?
        }
    }
    println!("✓ Set secret {}", args.name);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn secret_delete(args: SecretDeleteArgs, host: Option<&str>) -> Result<()> {
    let scope = resolve_workflow_scope(args.r.repo.as_deref(), args.org.as_deref(), host)?;
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete secret '{}'? Type name to confirm",
            args.name
        ))?;
        if ans != args.name {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    match &scope {
        WorkflowScope::Repo(ctx) => {
            api::workflow::delete_secret(&ctx.client, &ctx.owner, &ctx.name, &args.name).await?
        }
        WorkflowScope::Org { client, org } => {
            api::workflow::delete_org_secret(client, org, &args.name).await?
        }
    }
    println!("✓ Deleted secret {}", args.name);
    Ok(())
}

pub(crate) fn read_value(value: Option<&str>, from_file: Option<&str>) -> Result<String> {
    if let Some(v) = value {
        return Ok(v.to_string());
    }
    if let Some(path) = from_file {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf.trim_end().to_string());
        }
        return Ok(std::fs::read_to_string(path)?.trim_end().to_string());
    }
    Err(anyhow::anyhow!(
        "pass --value or --from-file (use `-` for stdin)"
    ))
}

/// The line `fj variable set` prints. Everything in it is server truth: the
/// name is the one the server stored (Forgejo upper-cases variable names),
/// and the scope is the one its `repo_id` / `owner_id` record. `target` is
/// the requested `owner/name` or org, used only to name that scope, and
/// `org_requested` says which of the two it is. If the server recorded a
/// different scope than the one asked for, the line says so instead of
/// repeating the request back.
pub(crate) fn variable_set_message(
    outcome: &api::workflow::VariableOutcome,
    org_requested: bool,
    target: &str,
) -> String {
    use api::workflow::{VariableScope, VariableWrite};
    let verb = match outcome.write {
        VariableWrite::Created => "Created",
        VariableWrite::Updated => "Updated",
    };
    let scope = match (outcome.variable.scope(), org_requested) {
        (VariableScope::Repository, false) => {
            format!("repository scope: {target} only")
        }
        (VariableScope::Owner, true) => {
            format!("organization scope: every repository in {target}")
        }
        (VariableScope::Repository, true) => format!(
            "repository scope, although organization {target} was requested; check `fj variable list --org {target}`"
        ),
        (VariableScope::Owner, false) => format!(
            "owner scope (shared across the owner's repositories), although repository {target} was requested"
        ),
        (VariableScope::Unreported, true) => {
            format!("scope not reported by the server; requested organization {target}")
        }
        (VariableScope::Unreported, false) => {
            format!("scope not reported by the server; requested repository {target}")
        }
    };
    format!(
        "✓ {verb} variable {} ({scope}). Stored readable, not as a secret.",
        outcome.variable.name
    )
}

/// Table rows for `fj variable list`: name, value preview, server scope.
pub(crate) fn variable_rows(vars: &[api::workflow::ActionVariable]) -> Vec<Vec<String>> {
    use api::workflow::VariableScope;
    vars.iter()
        .map(|v| {
            vec![
                v.name.clone(),
                value_preview(&v.data),
                match v.scope() {
                    VariableScope::Repository => "repository".to_string(),
                    VariableScope::Owner => "owner".to_string(),
                    VariableScope::Unreported => String::new(),
                },
            ]
        })
        .collect()
}

/// One table cell for a variable value: the first line, capped at 60 chars,
/// with an ellipsis when anything was dropped. `--json` carries the whole
/// value.
fn value_preview(value: &str) -> String {
    const MAX: usize = 60;
    let first = value.lines().next().unwrap_or_default();
    let mut out: String = first.chars().take(MAX).collect();
    if first.chars().count() > MAX || value.lines().count() > 1 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn variable(
        name: &str,
        data: &str,
        owner_id: i64,
        repo_id: i64,
    ) -> api::workflow::ActionVariable {
        api::workflow::ActionVariable {
            name: name.into(),
            data: data.into(),
            owner_id,
            repo_id,
            created_at: None,
        }
    }

    #[test]
    fn variable_set_message_shows_server_name_and_repository_scope() {
        // The user typed `region`; the server stored REGION on the repo.
        let msg = variable_set_message(
            &api::workflow::VariableOutcome {
                write: api::workflow::VariableWrite::Created,
                variable: variable("REGION", "us-west-2", 3, 9),
            },
            false,
            "o/r",
        );
        assert_eq!(
            msg,
            "✓ Created variable REGION (repository scope: o/r only). Stored readable, not as a secret."
        );
    }

    #[test]
    fn variable_set_message_shows_organization_scope_on_update() {
        let msg = variable_set_message(
            &api::workflow::VariableOutcome {
                write: api::workflow::VariableWrite::Updated,
                variable: variable("IMAGE", "x", 4, 0),
            },
            true,
            "team",
        );
        assert!(msg.starts_with(
            "✓ Updated variable IMAGE (organization scope: every repository in team)"
        ));
    }

    #[test]
    fn variable_set_message_never_echoes_a_scope_the_server_did_not_confirm() {
        let unreported = variable_set_message(
            &api::workflow::VariableOutcome {
                write: api::workflow::VariableWrite::Created,
                variable: variable("X", "1", 0, 0),
            },
            false,
            "o/r",
        );
        assert!(unreported.contains("scope not reported by the server"));
        assert!(!unreported.contains("repository scope: o/r"));

        let mismatch = variable_set_message(
            &api::workflow::VariableOutcome {
                write: api::workflow::VariableWrite::Created,
                variable: variable("X", "1", 4, 0),
            },
            false,
            "o/r",
        );
        assert!(mismatch.contains("owner scope"));
        assert!(mismatch.contains("although repository o/r was requested"));
    }

    #[test]
    fn variable_set_message_does_not_print_the_value() {
        let msg = variable_set_message(
            &api::workflow::VariableOutcome {
                write: api::workflow::VariableWrite::Created,
                variable: variable("X", "the-value", 0, 9),
            },
            false,
            "o/r",
        );
        assert!(!msg.contains("the-value"));
    }

    #[test]
    fn variable_rows_show_value_and_scope() {
        let rows = variable_rows(&[
            variable("REGION", "us-west-2", 3, 9),
            variable("IMAGE", "a\nb", 4, 0),
            variable("OLD", "", 0, 0),
        ]);
        assert_eq!(rows[0], vec!["REGION", "us-west-2", "repository"]);
        assert_eq!(rows[1], vec!["IMAGE", "a…", "owner"]);
        assert_eq!(rows[2], vec!["OLD", "", ""]);
    }

    #[test]
    fn value_preview_caps_long_values_by_char() {
        let long = "é".repeat(80);
        let got = value_preview(&long);
        assert_eq!(got.chars().count(), 61);
        assert!(got.ends_with('…'));
        assert_eq!(value_preview("short"), "short");
    }

    #[test]
    fn variable_help_separates_variables_from_secrets() {
        use clap::CommandFactory;
        let mut cmd = crate::cli::Cli::command();
        let var = cmd.find_subcommand_mut("variable").expect("variable");
        let set = var.find_subcommand_mut("set").expect("set");
        let help = set.render_long_help().to_string();
        assert!(help.contains("Variable name"));
        assert!(!help.contains("Secret name"));
        assert!(help.contains("fj secret set"));
    }

    #[test]
    fn read_value_passes_inline_value_through_verbatim() {
        // Inline values are intentionally not trimmed: a secret may have
        // meaningful leading/trailing whitespace.
        assert_eq!(read_value(Some("  s3cret  "), None).unwrap(), "  s3cret  ");
    }

    #[test]
    fn read_value_reads_file_and_trims_trailing_newline() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "tokenvalue").unwrap();
        let got = read_value(None, Some(f.path().to_str().unwrap())).unwrap();
        assert_eq!(got, "tokenvalue");
    }

    #[test]
    fn read_value_requires_a_source() {
        assert!(read_value(None, None).is_err());
    }

    #[test]
    fn read_value_prefers_inline_over_file() {
        let got = read_value(Some("inline"), Some("/nonexistent/path")).unwrap();
        assert_eq!(got, "inline");
    }

    #[test]
    fn org_scope_rejects_repo_scope() {
        let err = match resolve_workflow_scope(Some("o/r"), Some("team"), Some("example.com")) {
            Ok(_) => panic!("org and repo are exclusive"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("--org cannot be used"));
    }

    #[test]
    fn secret_org_scope_parses_and_conflicts_with_repo() {
        use clap::Parser;

        assert!(crate::cli::Cli::try_parse_from(["fj", "secret", "list", "--org", "team"]).is_ok());
        assert!(crate::cli::Cli::try_parse_from([
            "fj", "secret", "list", "--org", "team", "-R", "o/r",
        ])
        .is_err());
    }

    #[test]
    fn variable_org_scope_parses_and_conflicts_with_repo() {
        use clap::Parser;

        assert!(
            crate::cli::Cli::try_parse_from(["fj", "variable", "list", "--org", "team"]).is_ok()
        );
        assert!(crate::cli::Cli::try_parse_from([
            "fj", "variable", "list", "--org", "team", "-R", "o/r",
        ])
        .is_err());
    }
}
