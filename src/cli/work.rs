//! `fj work` — working-context / session-resumption commands.
//!
//! `work context` is implemented: it summarizes the resolved repo, the local
//! git state (branch, dirty files, ahead/behind upstream), and any open PR for
//! the current branch, as a table or `--json`.
//!
//! `work resume` / `work continue` are still placeholders (a saved-session
//! model is TBD): they honor `--dry-run`/`--json` and otherwise return a
//! structured "not yet implemented" error.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::{git, output};

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

/// A snapshot of "where am I right now": the resolved repo, the local git
/// state, and any open PR for the current branch. Serializes directly for
/// `--json` (the global `--json-fields` projection still applies).
#[derive(Debug, Serialize)]
struct WorkContext {
    host: String,
    repo: String,
    default_branch: String,
    /// None when HEAD is detached or we're not inside a git repo.
    branch: Option<String>,
    dirty: bool,
    changed_files: usize,
    /// ahead/behind the branch's upstream; None when there is no upstream.
    ahead: Option<u32>,
    behind: Option<u32>,
    /// The open PR whose head is the current branch, if any.
    pull_request: Option<PrSummary>,
}

#[derive(Debug, Serialize)]
struct PrSummary {
    number: u64,
    title: String,
    state: String,
    draft: bool,
    url: String,
}

impl PrSummary {
    fn from_pull(p: &api::pull::Pull) -> Self {
        Self {
            number: p.number,
            title: p.title.clone(),
            state: p.state.clone(),
            draft: p.draft,
            url: p.html_url.clone(),
        }
    }
}

/// Find the open PR whose head ref matches `branch`.
fn find_pr_for_branch<'a>(
    pulls: &'a [api::pull::Pull],
    branch: &str,
) -> Option<&'a api::pull::Pull> {
    pulls.iter().find(|p| p.head.ref_ == branch)
}

/// Render the human-readable summary (plain text, no ANSI, so it's trivially
/// testable). `--json` takes a different path through `print_json`.
fn render_human(wc: &WorkContext) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "Repo:     {} ({})", wc.repo, wc.host);
    let _ = writeln!(s, "Default:  {}", wc.default_branch);
    match &wc.branch {
        Some(b) => {
            let state = if wc.dirty {
                format!("{} uncommitted file(s)", wc.changed_files)
            } else {
                "clean".to_string()
            };
            let _ = writeln!(s, "Branch:   {b} ({state})");
        }
        None => {
            let _ = writeln!(s, "Branch:   (detached HEAD or not a git repo)");
        }
    }
    if let (Some(a), Some(b)) = (wc.ahead, wc.behind) {
        let _ = writeln!(s, "Tracking: ahead {a}, behind {b} (vs upstream)");
    }
    match &wc.pull_request {
        Some(pr) => {
            let draft = if pr.draft { " (draft)" } else { "" };
            let _ = writeln!(
                s,
                "PR:       #{} {} [{}]{}  {}",
                pr.number, pr.title, pr.state, draft, pr.url
            );
        }
        None => {
            let _ = writeln!(s, "PR:       (none open for this branch)");
        }
    }
    s
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn context(args: ContextArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;

    let branch = git::current_branch();
    let (dirty, changed_files) = git::working_tree_status().unwrap_or((false, 0));
    let (ahead, behind) = match git::ahead_behind_upstream() {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };

    // Only look up a PR when we actually know the local branch.
    let pull_request = match branch.as_deref() {
        Some(b) => {
            let page = api::pull::list(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                api::pull::ListOptions::default(),
            )
            .await?;
            find_pr_for_branch(&page.items, b).map(PrSummary::from_pull)
        }
        None => None,
    };

    let wc = WorkContext {
        host: ctx.host.clone(),
        repo: format!("{}/{}", ctx.owner, ctx.name),
        default_branch: repo.default_branch,
        branch,
        dirty,
        changed_files,
        ahead,
        behind,
        pull_request,
    };

    if args.json {
        return output::print_json(&serde_json::to_value(&wc)?);
    }
    print!("{}", render_human(&wc));
    Ok(())
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

    fn pull_with_head(number: u64, head: &str) -> api::pull::Pull {
        serde_json::from_value(serde_json::json!({
            "id": number, "number": number, "title": format!("PR {number}"),
            "state": "open", "user": { "id": 1, "login": "alice" },
            "html_url": format!("https://x/pulls/{number}"),
            "head": { "ref": head, "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "draft": false,
            "created_at": "2026-06-01T00:00:00Z",
            "updated_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn find_pr_for_branch_matches_head_ref() {
        let pulls = vec![pull_with_head(1, "main"), pull_with_head(2, "feature/x")];
        assert_eq!(find_pr_for_branch(&pulls, "feature/x").unwrap().number, 2);
        assert!(find_pr_for_branch(&pulls, "nope").is_none());
    }

    fn sample_context() -> WorkContext {
        WorkContext {
            host: "git.example.com".into(),
            repo: "o/n".into(),
            default_branch: "main".into(),
            branch: Some("feature/x".into()),
            dirty: true,
            changed_files: 3,
            ahead: Some(2),
            behind: Some(1),
            pull_request: Some(PrSummary {
                number: 7,
                title: "Add feature".into(),
                state: "open".into(),
                draft: false,
                url: "https://x/pulls/7".into(),
            }),
        }
    }

    #[test]
    fn render_human_includes_branch_tracking_and_pr() {
        let out = render_human(&sample_context());
        assert!(out.contains("o/n (git.example.com)"));
        assert!(out.contains("Branch:   feature/x (3 uncommitted file(s))"));
        assert!(out.contains("ahead 2, behind 1"));
        assert!(out.contains("#7 Add feature [open]"));
    }

    #[test]
    fn render_human_handles_clean_detached_and_no_pr() {
        let wc = WorkContext {
            branch: None,
            dirty: false,
            changed_files: 0,
            ahead: None,
            behind: None,
            pull_request: None,
            ..sample_context()
        };
        let out = render_human(&wc);
        assert!(out.contains("detached HEAD or not a git repo"));
        assert!(out.contains("none open for this branch"));
        // No tracking line when ahead/behind are unknown.
        assert!(!out.contains("Tracking:"));
    }

    #[test]
    fn work_context_json_shape() {
        let v = serde_json::to_value(sample_context()).unwrap();
        assert_eq!(v["repo"], "o/n");
        assert_eq!(v["default_branch"], "main");
        assert_eq!(v["branch"], "feature/x");
        assert_eq!(v["dirty"], true);
        assert_eq!(v["changed_files"], 3);
        assert_eq!(v["ahead"], 2);
        assert_eq!(v["pull_request"]["number"], 7);
        assert_eq!(v["pull_request"]["draft"], false);
    }
}
