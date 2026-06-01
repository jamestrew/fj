//! `fj agent` — AI-assisted code operations (scaffolding).
//!
//! Three subcommands: two inspect (`review`, `explain`) and one mutating
//! (`fix`). All handlers are placeholders that emit a structured plan or a
//! `not_implemented` JSON stub, and otherwise surface a structured
//! not-yet-implemented error. Once the real endpoints exist, replace the
//! `not_implemented` helper calls with the typed `crate::api::agent::*`
//! wrappers.
//!
//! Because this file carries a `#[cfg(test)]` module it must NOT be added to
//! COV_IGNORE in the Makefile.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct AgentCmd {
    #[command(subcommand)]
    pub command: AgentSub,
}

#[derive(Debug, Subcommand)]
pub enum AgentSub {
    /// Run an automated review of the current change.
    Review(ReviewArgs),
    /// Apply automated fixes to the current change.
    Fix(FixArgs),
    /// Explain a piece of code or a change.
    Explain(ExplainArgs),
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct FixArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: AgentCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        AgentSub::Review(args) => review(args, host).await,
        AgentSub::Fix(args) => fix(args, host).await,
        AgentSub::Explain(args) => explain(args, host).await,
    }
}

/// Emit a `not_implemented` JSON stub for an inspect command and return Ok.
/// Routes through `output::print_json` so the global `--json-fields`
/// projection applies uniformly.
fn json_stub(command: &str) -> Result<()> {
    output::print_json(&serde_json::json!({
        "command": command,
        "status": "not_implemented",
    }))
}

/// The single shared not-yet-implemented error for the agent group.
fn not_implemented(command: &str) -> Result<()> {
    bail!("`fj agent {command}` is not yet implemented");
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let _ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.json {
        return json_stub("review");
    }
    not_implemented("review")
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fix(args: FixArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.dry_run {
        println!(
            "would: apply automated fixes to the current change in {}/{}",
            ctx.owner, ctx.name
        );
        return Ok(());
    }
    not_implemented("fix")
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(args: ExplainArgs, host: Option<&str>) -> Result<()> {
    let _ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.json {
        return json_stub("explain");
    }
    not_implemented("explain")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::{CommandFactory, Parser};

    #[test]
    fn agent_subtree_is_internally_consistent() {
        // Structural smoke check over the whole derived tree, including this
        // group. clap only runs these checks behind debug_assert.
        Cli::command().debug_assert();
    }

    #[test]
    fn review_parses_json_flag_and_repo() {
        let cli = Cli::try_parse_from(["fj", "agent", "review", "--json", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Review(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected agent review, got {other:?}"),
        }
    }

    #[test]
    fn fix_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "agent", "fix", "--dry-run", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Fix(a),
            }) => {
                assert!(a.dry_run);
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected agent fix, got {other:?}"),
        }
    }

    #[test]
    fn explain_parses_json_flag() {
        let cli = Cli::try_parse_from(["fj", "agent", "explain", "--json"]).expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Explain(a),
            }) => {
                assert!(a.json);
            }
            other => panic!("expected agent explain, got {other:?}"),
        }
    }
}

// To register in src/cli/mod.rs:
//   pub mod agent;
//   // in `enum Command`:
//   /// AI-assisted code operations.
//   Agent(agent::AgentCmd),
//   // in `dispatch`:
//   Command::Agent(cmd) => agent::run(cmd, host).await,
// To register in src/main.rs KNOWN_SUBCOMMANDS: add "agent".
