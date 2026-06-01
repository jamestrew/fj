//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Stack state is local and git-native (`<git-dir>/fj/stack.json`, see
//! [`stack_state`](super::stack_state)) — no server state, offline-capable.
//! `stack new` (records a stack rooted at the current branch) and `stack
//! review` (shows the stack + each branch's PR state) are implemented.
//! `sync` / `split` / `absorb` / `ship` are still placeholders (dry-run plan,
//! else a structured "not yet implemented" error); the chain rebases land in
//! follow-up PRs.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::stack_state::{self, Stack};
use crate::{git, output};

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
    // No `-R`: stack state is local to the current git dir.
    /// Name for the new stack (defaults to the current branch name).
    pub name: Option<String>,
    /// Branch the stack targets (its trunk).
    #[arg(long, default_value = "main")]
    pub base: String,
    /// Print the planned actions and exit without writing anything.
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
    /// Stack to review (defaults to the current stack).
    #[arg(long)]
    pub name: Option<String>,
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
async fn new(args: NewArgs, _host: Option<&str>) -> Result<()> {
    // Local-only: record metadata under <git-dir>/fj/stack.json. No network,
    // no git branch creation yet (that comes with sync/split).
    let branch = git::current_branch()
        .context("not on a branch (detached HEAD); check out a branch first")?;
    let name = args.name.clone().unwrap_or_else(|| branch.clone());

    let mut state = stack_state::load()?;
    if state.find(&name).is_some() {
        bail!("a stack named '{name}' already exists");
    }

    if args.dry_run {
        println!(
            "would: create stack '{name}' (base {}) tracking branch '{branch}'",
            args.base
        );
        return Ok(());
    }

    state.add(Stack {
        name: name.clone(),
        base: args.base.clone(),
        branches: vec![branch.clone()],
        created_at: Utc::now().to_rfc3339(),
    })?;
    stack_state::save(&state)?;
    println!(
        "✓ Created stack '{name}' (base {}) tracking '{branch}'",
        args.base
    );
    Ok(())
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

/// A stack plus the PR state of each of its branches. Serializes for `--json`.
#[derive(Debug, Serialize)]
struct StackReview {
    name: String,
    base: String,
    current: bool,
    entries: Vec<StackEntryView>,
}

#[derive(Debug, Serialize)]
struct StackEntryView {
    branch: String,
    pull_request: Option<EntryPr>,
}

#[derive(Debug, Serialize)]
struct EntryPr {
    number: u64,
    title: String,
    state: String,
    draft: bool,
    url: String,
}

/// Build the review view by matching each stack branch to its open PR (by
/// head ref). Pure — takes the already-fetched PR list.
fn build_review(stack: &Stack, pulls: &[api::pull::Pull], is_current: bool) -> StackReview {
    let entries = stack
        .branches
        .iter()
        .map(|b| {
            let pull_request = pulls.iter().find(|p| p.head.ref_ == *b).map(|p| EntryPr {
                number: p.number,
                title: p.title.clone(),
                state: p.state.clone(),
                draft: p.draft,
                url: p.html_url.clone(),
            });
            StackEntryView {
                branch: b.clone(),
                pull_request,
            }
        })
        .collect();
    StackReview {
        name: stack.name.clone(),
        base: stack.base.clone(),
        current: is_current,
        entries,
    }
}

/// Plain-text render of a stack review (no ANSI, so it's easy to test).
fn render_review(r: &StackReview) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let marker = if r.current { "  (current)" } else { "" };
    let _ = writeln!(s, "Stack: {}{}   base: {}", r.name, marker, r.base);
    if r.entries.is_empty() {
        let _ = writeln!(s, "  (no branches tracked yet)");
        return s;
    }
    for e in &r.entries {
        match &e.pull_request {
            Some(pr) => {
                let draft = if pr.draft { " (draft)" } else { "" };
                let _ = writeln!(s, "  {}  #{} [{}]{}", e.branch, pr.number, pr.state, draft);
            }
            None => {
                let _ = writeln!(s, "  {}  (no PR)", e.branch);
            }
        }
    }
    s
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let state = stack_state::load()?;
    if state.stacks.is_empty() {
        bail!("no stacks yet. Run `fj stack new` to start one.");
    }
    let name = match args.name.clone().or_else(|| state.current.clone()) {
        Some(n) => n,
        None => bail!("no current stack; pass --name or run `fj stack new`"),
    };
    let stack = state
        .find(&name)
        .with_context(|| format!("no stack named '{name}'"))?;

    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let page = api::pull::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        api::pull::ListOptions::default(),
    )
    .await?;
    let is_current = state.current.as_deref() == Some(name.as_str());
    let review = build_review(stack, &page.items, is_current);

    if args.json {
        return output::print_json(&serde_json::to_value(&review)?);
    }
    print!("{}", render_review(&review));
    Ok(())
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
    fn new_parses_name_base_and_dry_run() {
        let cli = Cli::try_parse_from([
            "fj",
            "stack",
            "new",
            "feature",
            "--base",
            "develop",
            "--dry-run",
        ])
        .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::New(a),
            }) => {
                assert!(a.dry_run);
                assert_eq!(a.name.as_deref(), Some("feature"));
                assert_eq!(a.base, "develop");
            }
            other => panic!("expected stack new, got {other:?}"),
        }
    }

    #[test]
    fn new_name_optional_and_base_defaults_to_main() {
        let cli = Cli::try_parse_from(["fj", "stack", "new"]).expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::New(a),
            }) => {
                assert!(a.name.is_none());
                assert!(!a.dry_run);
                assert_eq!(a.base, "main");
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

    #[test]
    fn review_parses_name_and_json() {
        let cli = Cli::try_parse_from(["fj", "stack", "review", "--name", "feat", "--json"])
            .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Review(a),
            }) => {
                assert_eq!(a.name.as_deref(), Some("feat"));
                assert!(a.json);
            }
            other => panic!("expected stack review, got {other:?}"),
        }
    }

    fn pull_with_head(number: u64, head: &str, state: &str) -> api::pull::Pull {
        serde_json::from_value(serde_json::json!({
            "id": number, "number": number, "title": format!("PR {number}"),
            "state": state, "user": { "id": 1, "login": "alice" },
            "html_url": format!("https://x/pulls/{number}"),
            "head": { "ref": head, "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "draft": false,
            "created_at": "2026-06-01T00:00:00Z",
            "updated_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn sample_stack() -> Stack {
        Stack {
            name: "feat".into(),
            base: "main".into(),
            branches: vec!["feature/a".into(), "feature/b".into()],
            created_at: "2026-06-01T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn build_review_matches_branches_to_prs() {
        let pulls = vec![
            pull_with_head(7, "feature/a", "open"),
            pull_with_head(9, "other", "open"),
        ];
        let r = build_review(&sample_stack(), &pulls, true);
        assert_eq!(r.name, "feat");
        assert_eq!(r.base, "main");
        assert!(r.current);
        assert_eq!(r.entries.len(), 2);
        // feature/a has PR #7; feature/b has none.
        assert_eq!(r.entries[0].pull_request.as_ref().unwrap().number, 7);
        assert!(r.entries[1].pull_request.is_none());
    }

    #[test]
    fn render_review_shows_branches_and_pr_state() {
        let pulls = vec![pull_with_head(7, "feature/a", "open")];
        let out = render_review(&build_review(&sample_stack(), &pulls, true));
        assert!(out.contains("Stack: feat  (current)   base: main"));
        assert!(out.contains("feature/a  #7 [open]"));
        assert!(out.contains("feature/b  (no PR)"));
    }

    #[test]
    fn build_review_json_shape() {
        let pulls = vec![pull_with_head(7, "feature/a", "open")];
        let v = serde_json::to_value(build_review(&sample_stack(), &pulls, false)).unwrap();
        assert_eq!(v["name"], "feat");
        assert_eq!(v["current"], false);
        assert_eq!(v["entries"][0]["branch"], "feature/a");
        assert_eq!(v["entries"][0]["pull_request"]["number"], 7);
        assert!(v["entries"][1]["pull_request"].is_null());
    }
}
