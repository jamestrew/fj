//! `fj work` — working-context / session-resumption commands.
//!
//! Scaffolding only: the structure, help text, argument parsing, JSON output,
//! and dry-run plumbing are wired, but the mutating handlers are placeholders
//! that fail with a structured "not yet implemented" error.
//!
//! To register in `src/cli/mod.rs`:
//!   pub mod work;
//!   // in `enum Command`:
//!   /// Work with the current working context / session.
//!   Work(work::WorkCmd),
//!   // in `dispatch`:
//!   Command::Work(cmd) => work::run(cmd, host).await,
//! To register in `src/main.rs` KNOWN_SUBCOMMANDS: add "work".
//!
//! Because this file carries a `#[cfg(test)]` module it must NOT be added to
//! COV_IGNORE in the Makefile.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct WorkCmd {
    #[command(subcommand)]
    pub command: WorkSub,
}

#[derive(Debug, Subcommand)]
pub enum WorkSub {
    /// Show the current working context (branch, stack, in-flight changes).
    Context(ContextArgs),
    /// Resume a previously saved work session.
    Resume(ResumeArgs),
    /// Continue the next step of the current work session.
    Continue(ContinueArgs),
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ContinueArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: WorkCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        WorkSub::Context(args) => context(args, host).await,
        WorkSub::Resume(args) => resume(args, host).await,
        WorkSub::Continue(args) => continue_(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn context(args: ContextArgs, host: Option<&str>) -> Result<()> {
    let _ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.json {
        return not_implemented_json("work context");
    }
    not_implemented("work context")
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn resume(args: ResumeArgs, host: Option<&str>) -> Result<()> {
    let _ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.dry_run {
        println!("would: resume the saved work session");
        return Ok(());
    }
    if args.json {
        return not_implemented_json("work resume");
    }
    not_implemented("work resume")
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn continue_(args: ContinueArgs, host: Option<&str>) -> Result<()> {
    let _ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.dry_run {
        println!("would: continue the next step of the current work session");
        return Ok(());
    }
    not_implemented("work continue")
}

/// Structured "not yet implemented" error for a placeholder handler.
#[cfg_attr(coverage_nightly, coverage(off))]
fn not_implemented(command: &str) -> Result<()> {
    bail!("`fj {command}` is not yet implemented");
}

/// JSON placeholder describing an unimplemented command, routed through
/// `output::print_json` so the global `--json-fields` projection applies.
#[cfg_attr(coverage_nightly, coverage(off))]
fn not_implemented_json(command: &str) -> Result<()> {
    output::print_json(&serde_json::json!({
        "command": command,
        "status": "not_implemented",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::{CommandFactory, Parser};

    #[test]
    fn work_subtree_is_internally_consistent() {
        // Structural smoke check over the whole derived tree, including this
        // group. clap only runs these checks behind debug_assert.
        Cli::command().debug_assert();
    }

    #[test]
    fn context_parses_json_flag_and_repo() {
        let cli = Cli::try_parse_from(["fj", "work", "context", "--json", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Work(WorkCmd {
                command: WorkSub::Context(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected work context, got {other:?}"),
        }
    }

    #[test]
    fn resume_parses_dry_run_and_json_flags() {
        let cli = Cli::try_parse_from(["fj", "work", "resume", "--dry-run", "--json"])
            .expect("should parse");
        match cli.command {
            Command::Work(WorkCmd {
                command: WorkSub::Resume(a),
            }) => {
                assert!(a.dry_run);
                assert!(a.json);
            }
            other => panic!("expected work resume, got {other:?}"),
        }
    }

    #[test]
    fn continue_parses_dry_run_flag() {
        // The on-the-wire command stays `continue` even though the clap
        // variant is named `Continue`.
        let cli =
            Cli::try_parse_from(["fj", "work", "continue", "--dry-run"]).expect("should parse");
        match cli.command {
            Command::Work(WorkCmd {
                command: WorkSub::Continue(a),
            }) => {
                assert!(a.dry_run);
            }
            other => panic!("expected work continue, got {other:?}"),
        }
    }
}
