//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Scaffolding only: every handler resolves the repo context (so the
//! `-R`/auto-detect plumbing is exercised) and then either prints a dry-run
//! plan, emits a `not_implemented` JSON stub, or bails with a structured
//! "not yet implemented" error. No subcommand is wired to a real endpoint.
//!
//! Because this file carries a `#[cfg(test)]` module it must NOT be added to
//! COV_IGNORE in the Makefile.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::cli::context::{resolve_repo, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct StackCmd {
    #[command(subcommand)]
    pub command: StackSub,
}

#[derive(Debug, Subcommand)]
pub enum StackSub {
    /// Start a new stack.
    New(NewArgs),
    /// Sync the stack with its base branch.
    Sync(SyncArgs),
    /// Split the current change into multiple stacked entries.
    Split(SplitArgs),
    /// Show the stack and its review state.
    Review(ReviewArgs),
    /// Absorb staged changes into the appropriate stack entries.
    Absorb(AbsorbArgs),
    /// Ship (merge) the ready entries of the stack.
    Ship(ShipArgs),
}

#[derive(Debug, Args)]
pub struct NewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Name for the new stack.
    pub name: Option<String>,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SplitArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
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
pub struct AbsorbArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ShipArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: StackCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        StackSub::New(args) => new(args, host).await,
        StackSub::Sync(args) => sync(args, host).await,
        StackSub::Split(args) => split(args, host).await,
        StackSub::Review(args) => review(args, host).await,
        StackSub::Absorb(args) => absorb(args, host).await,
        StackSub::Ship(args) => ship(args, host).await,
    }
}

/// Shared "scaffolding only" exit for a mutating subcommand: print a dry-run
/// plan and stop when `--dry-run` is set, otherwise bail with a structured
/// not-yet-implemented error. The `plan` is the human-readable action line.
fn placeholder_mutation(plan: &str, command: &str, dry_run: bool) -> Result<()> {
    println!("would: {plan}");
    if dry_run {
        return Ok(());
    }
    bail!("`fj stack {command}` is not yet implemented");
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn new(args: NewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let name = args.name.as_deref().unwrap_or("(unnamed)");
    let plan = format!("start a new stack '{}' in {}/{}", name, ctx.owner, ctx.name);
    placeholder_mutation(&plan, "new", args.dry_run)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn sync(args: SyncArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let plan = format!(
        "sync the stack in {}/{} with its base branch",
        ctx.owner, ctx.name
    );
    placeholder_mutation(&plan, "sync", args.dry_run)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn split(args: SplitArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let plan = format!(
        "split the current change in {}/{} into stacked entries",
        ctx.owner, ctx.name
    );
    placeholder_mutation(&plan, "split", args.dry_run)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    if args.json {
        return output::print_json(&serde_json::json!({
            "command": "stack review",
            "repo": format!("{}/{}", ctx.owner, ctx.name),
            "status": "not_implemented",
        }));
    }
    bail!("`fj stack review` is not yet implemented");
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn absorb(args: AbsorbArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let plan = format!(
        "absorb staged changes into the stack entries in {}/{}",
        ctx.owner, ctx.name
    );
    placeholder_mutation(&plan, "absorb", args.dry_run)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn ship(args: ShipArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let plan = format!(
        "ship the ready entries of the stack in {}/{}",
        ctx.owner, ctx.name
    );
    placeholder_mutation(&plan, "ship", args.dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::{CommandFactory, Parser};

    #[test]
    fn stack_subtree_is_internally_consistent() {
        // Structural smoke check over the whole derived tree, including this
        // group. clap only runs these checks behind debug_assert.
        Cli::command().debug_assert();
    }

    #[test]
    fn new_parses_optional_name_and_dry_run() {
        let cli = Cli::try_parse_from(["fj", "stack", "new", "feature", "--dry-run", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::New(a),
            }) => {
                assert!(a.dry_run);
                assert_eq!(a.name.as_deref(), Some("feature"));
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected stack new, got {other:?}"),
        }
    }

    #[test]
    fn new_name_is_optional() {
        let cli = Cli::try_parse_from(["fj", "stack", "new"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::New(a),
            }) => {
                assert!(a.name.is_none());
                assert!(!a.dry_run);
            }
            other => panic!("expected stack new, got {other:?}"),
        }
    }

    #[test]
    fn sync_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "stack", "sync", "--dry-run"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Sync(a),
            }) => assert!(a.dry_run),
            other => panic!("expected stack sync, got {other:?}"),
        }
    }

    #[test]
    fn split_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "stack", "split", "--dry-run"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Split(a),
            }) => assert!(a.dry_run),
            other => panic!("expected stack split, got {other:?}"),
        }
    }

    #[test]
    fn review_parses_json_flag_and_repo() {
        let cli = Cli::try_parse_from(["fj", "stack", "review", "--json", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Review(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected stack review, got {other:?}"),
        }
    }

    #[test]
    fn absorb_parses_dry_run_flag() {
        let cli =
            Cli::try_parse_from(["fj", "stack", "absorb", "--dry-run"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Absorb(a),
            }) => assert!(a.dry_run),
            other => panic!("expected stack absorb, got {other:?}"),
        }
    }

    #[test]
    fn ship_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "stack", "ship", "--dry-run"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Ship(a),
            }) => assert!(a.dry_run),
            other => panic!("expected stack ship, got {other:?}"),
        }
    }
}

// To register in src/cli/mod.rs:
//   pub mod stack;
//   // in `enum Command`:
//   /// Manage stacked branches / stacked PRs.
//   Stack(stack::StackCmd),
//   // in `dispatch`:
//   Command::Stack(cmd) => stack::run(cmd, host).await,
// To register in src/main.rs KNOWN_SUBCOMMANDS: add "stack".
