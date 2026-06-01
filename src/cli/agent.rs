//! `fj agent` — AI-assisted code operations (review / fix / explain).
//!
//! All three route through the [`agent_provider`](super::agent_provider)
//! abstraction: config comes from `FJ_AGENT_PROVIDER` / `FJ_AGENT_API_KEY` /
//! `FJ_AGENT_MODEL` (the Fjord token is never reused). With nothing
//! configured, every command reports a clear "agent backend not configured"
//! error; the built-in `mock` provider echoes for development.
//!
//! `explain [target]` feeds the provider real context: a file's contents, a
//! git revision (`git show`), or the working-tree diff when omitted. `review`
//! still uses generic context (PR7 wires the PR/working diff). `fix` emits a
//! patch only (never mutates); `--write` / `--pr` come later.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use crate::cli::agent_provider::{self, AgentConfig, AgentRequest, AgentTask};
use crate::cli::context::RepoFlag;
use crate::{git, output};

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
    // No `-R`: explain reads local code/diff via git, not the repo API.
    /// What to explain: a file path, a git revision, or omitted for the
    /// current working-tree diff.
    pub target: Option<String>,
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

/// Where `agent explain` draws its context from.
#[derive(Debug, PartialEq)]
enum ExplainSource {
    /// No target given: explain the working-tree diff.
    WorkingDiff,
    /// An existing file path: explain its contents.
    File(String),
    /// Anything else: treat as a git revision to `git show`.
    Revision(String),
}

/// Decide what a `target` refers to. `file_exists` is whether the target names
/// an existing file (the caller checks the filesystem; this stays pure).
fn classify_target(target: Option<&str>, file_exists: bool) -> ExplainSource {
    match target {
        None => ExplainSource::WorkingDiff,
        Some(t) if file_exists => ExplainSource::File(t.to_string()),
        Some(t) => ExplainSource::Revision(t.to_string()),
    }
}

/// Build the context string to hand the provider for `explain`.
#[cfg_attr(coverage_nightly, coverage(off))]
fn gather_explain_input(target: Option<&str>) -> Result<String> {
    let exists = target.map(|t| Path::new(t).is_file()).unwrap_or(false);
    match classify_target(target, exists) {
        ExplainSource::WorkingDiff => {
            let diff = git::output(&["diff", "HEAD"])?;
            if diff.trim().is_empty() {
                bail!("no uncommitted changes to explain; pass a file path or a git revision");
            }
            Ok(format!("Working-tree diff:\n{diff}"))
        }
        ExplainSource::File(p) => {
            let content =
                std::fs::read_to_string(&p).with_context(|| format!("reading file {p}"))?;
            Ok(format!("File {p}:\n{content}"))
        }
        ExplainSource::Revision(r) => {
            if r.starts_with('-') {
                bail!("revision {r:?} starts with '-'; refusing to pass it to git");
            }
            let diff = git::output(&["show", &r])?;
            Ok(format!("Revision {r}:\n{diff}"))
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(args: ExplainArgs, _host: Option<&str>) -> Result<()> {
    let input = gather_explain_input(args.target.as_deref())?;
    run_task(AgentTask::Explain, input, args.json)
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
    fn explain_parses_target_and_json() {
        let cli = Cli::try_parse_from(["fj", "agent", "explain", "src/main.rs", "--json"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Explain(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.target.as_deref(), Some("src/main.rs"));
            }
            other => panic!("expected agent explain, got {other:?}"),
        }
    }

    #[test]
    fn explain_target_is_optional() {
        let cli = Cli::try_parse_from(["fj", "agent", "explain"]).expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Explain(a),
            }) => assert!(a.target.is_none()),
            other => panic!("expected agent explain, got {other:?}"),
        }
    }

    #[test]
    fn classify_target_picks_source() {
        assert_eq!(classify_target(None, false), ExplainSource::WorkingDiff);
        assert_eq!(
            classify_target(Some("src/main.rs"), true),
            ExplainSource::File("src/main.rs".into())
        );
        // Not an existing file → treated as a revision.
        assert_eq!(
            classify_target(Some("HEAD~1"), false),
            ExplainSource::Revision("HEAD~1".into())
        );
    }
}
