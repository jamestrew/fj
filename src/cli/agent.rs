//! `fj agent` — AI-assisted code operations (review / fix / explain).
//!
//! All three route through the [`agent_provider`](super::agent_provider)
//! abstraction: config comes from `FJ_AGENT_PROVIDER` / `FJ_AGENT_API_KEY` /
//! `FJ_AGENT_MODEL` (the Fjord token is never reused). With nothing
//! configured, every command reports a clear "agent backend not configured"
//! error; the built-in `mock` provider echoes for development.
//!
//! The task-specific context is still generic ("the current change"); PR6
//! enriches `explain` and PR7 enriches `review` with the real diff/target.
//! `fix` emits a patch only (never mutates); `--write` / `--pr` come later.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::cli::agent_provider::{self, AgentConfig, AgentRequest, AgentTask};
use crate::cli::context::RepoFlag;
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

/// Resolve the configured provider, run a task, and print the result.
/// Shared by every agent command. With no backend configured this surfaces
/// the provider's "not configured" error; with `mock` it echoes the request.
/// `input` is the context the command gathered (PR6/PR7 enrich it per task).
#[cfg_attr(coverage_nightly, coverage(off))]
fn run_task(task: AgentTask, input: String, json: bool) -> Result<()> {
    let cfg = AgentConfig::from_env();
    let provider = agent_provider::resolve(&cfg)?;
    let resp = provider.complete(&AgentRequest {
        task,
        input,
        model: cfg.model.clone(),
    })?;
    if json {
        return output::print_json(&serde_json::json!({
            "task": task.as_str(),
            "provider": resp.provider,
            "model": resp.model,
            "output": resp.output,
        }));
    }
    println!("{}", resp.output);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn review(args: ReviewArgs, _host: Option<&str>) -> Result<()> {
    // PR7 enriches this with the actual PR/working diff to review.
    run_task(
        AgentTask::Review,
        "the current change".to_string(),
        args.json,
    )
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fix(args: FixArgs, _host: Option<&str>) -> Result<()> {
    // Default contract: emit a patch only, never mutate. --write / --pr come
    // later; for now --dry-run just reports the plan without calling out.
    if args.dry_run {
        println!(
            "would: ask the configured agent to propose a patch and print it (no changes applied)"
        );
        return Ok(());
    }
    run_task(AgentTask::Fix, "the current change".to_string(), false)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(args: ExplainArgs, _host: Option<&str>) -> Result<()> {
    // PR6 enriches this with the actual target (a ref / file / question).
    run_task(
        AgentTask::Explain,
        "the current change".to_string(),
        args.json,
    )
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
