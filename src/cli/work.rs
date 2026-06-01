//! `fj work` — working-context / session-resumption commands.
//!
//! `work context` is implemented: it summarizes the resolved repo, the local
//! git state (branch, dirty files, ahead/behind upstream), and any open PR for
//! the current branch, as a table or `--json`.
//!
//! `work context` also persists a local-only session to `<git-dir>/fj/
//! session.json` (see [`work_session`](super::work_session)). `work resume`
//! reads it back, shows the saved snapshot, and diffs it against a freshly
//! gathered context. `work continue` advances the session's suggested-step
//! cursor by one and reports what's next — advisory only, it never runs the
//! step for you.

use anyhow::{bail, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::cli::work_session::{self, PrRef, Session};
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
    // No `-R`: `continue` walks the local session under the current git dir,
    // so a repo target is meaningless here.
    /// Show which step would advance, without moving the cursor.
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

/// Gather the live working context: local git state + the repo's default
/// branch + the open PR (if any) for the current branch.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn gather(ctx: &RepoContext) -> Result<WorkContext> {
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    let branch = git::current_branch();
    let (dirty, changed_files) = git::working_tree_status().unwrap_or((false, 0));
    let (ahead, behind) = match git::ahead_behind_upstream() {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
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
    Ok(WorkContext {
        host: ctx.host.clone(),
        repo: format!("{}/{}", ctx.owner, ctx.name),
        default_branch: repo.default_branch,
        branch,
        dirty,
        changed_files,
        ahead,
        behind,
        pull_request,
    })
}

/// Build the persisted session snapshot from a live context. `now` is the
/// RFC3339 save timestamp; `remote` the primary remote URL, if any.
fn to_session(wc: &WorkContext, remote: Option<String>, now: String) -> Session {
    let next_steps = work_session::derive_next_steps(
        wc.branch.as_deref(),
        wc.dirty,
        wc.changed_files,
        wc.ahead,
        wc.pull_request.is_some(),
    );
    Session {
        saved_at: now,
        host: wc.host.clone(),
        repo: wc.repo.clone(),
        remote,
        branch: wc.branch.clone(),
        dirty: wc.dirty,
        changed_files: wc.changed_files,
        pull_request: wc.pull_request.as_ref().map(|p| PrRef {
            number: p.number,
            title: p.title.clone(),
            state: p.state.clone(),
            url: p.url.clone(),
        }),
        next_steps,
        cursor: 0,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn context(args: ContextArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let wc = gather(&ctx).await?;

    // Persist the session as a convenience for `work resume` / `continue`.
    // A failure to write the cache must never sink the inspect command.
    let session = to_session(&wc, git::remote_url(), Utc::now().to_rfc3339());
    let _ = work_session::save(&session);

    if args.json {
        return output::print_json(&serde_json::to_value(&wc)?);
    }
    print!("{}", render_human(&wc));
    Ok(())
}

/// Render the saved session: the snapshot fields plus the suggested next
/// step at the cursor. Plain text for easy testing.
fn render_session(s: &Session) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Saved session ({})", s.saved_at);
    let _ = writeln!(out, "  Repo:    {} ({})", s.repo, s.host);
    let _ = writeln!(
        out,
        "  Branch:  {}",
        s.branch.as_deref().unwrap_or("(detached)")
    );
    let state = if s.dirty {
        format!("{} uncommitted file(s)", s.changed_files)
    } else {
        "clean".to_string()
    };
    let _ = writeln!(out, "  Tree:    {state}");
    match &s.pull_request {
        Some(pr) => {
            let _ = writeln!(out, "  PR:      #{} {} [{}]", pr.number, pr.title, pr.state);
        }
        None => {
            let _ = writeln!(out, "  PR:      (none saved)");
        }
    }
    match s.next_steps.get(s.cursor) {
        Some(step) => {
            let _ = writeln!(out, "  Next:    {step}");
        }
        None => {
            let _ = writeln!(out, "  Next:    (no further steps)");
        }
    }
    out
}

/// Human-readable lines describing what changed between the saved session and
/// the freshly-gathered context. Empty when nothing notable changed.
fn diff_session_vs_context(s: &Session, wc: &WorkContext) -> Vec<String> {
    let mut changes = Vec::new();
    if s.branch != wc.branch {
        changes.push(format!(
            "branch: {} -> {}",
            s.branch.as_deref().unwrap_or("(detached)"),
            wc.branch.as_deref().unwrap_or("(detached)")
        ));
    }
    if (s.dirty, s.changed_files) != (wc.dirty, wc.changed_files) {
        let fmt = |dirty: bool, n: usize| {
            if dirty {
                format!("{n} file(s)")
            } else {
                "clean".to_string()
            }
        };
        changes.push(format!(
            "working tree: {} -> {}",
            fmt(s.dirty, s.changed_files),
            fmt(wc.dirty, wc.changed_files)
        ));
    }
    let saved_pr_state = s
        .pull_request
        .as_ref()
        .map(|p| (p.number, p.state.as_str()));
    let fresh_pr_state = wc
        .pull_request
        .as_ref()
        .map(|p| (p.number, p.state.as_str()));
    if saved_pr_state != fresh_pr_state {
        match (s.pull_request.as_ref(), wc.pull_request.as_ref()) {
            (Some(a), Some(b)) => {
                changes.push(format!("PR #{}: {} -> {}", a.number, a.state, b.state))
            }
            (None, Some(b)) => changes.push(format!("PR: (none) -> #{} {}", b.number, b.state)),
            (Some(a), None) => changes.push(format!("PR: #{} {} -> (none)", a.number, a.state)),
            (None, None) => {}
        }
    }
    changes
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn resume(args: ResumeArgs, host: Option<&str>) -> Result<()> {
    let Some(session) = work_session::load()? else {
        bail!("no saved work session for this repo. Run `fj work context` first.");
    };

    if args.dry_run {
        println!("would: resume the session saved at {}", session.saved_at);
        return Ok(());
    }

    // Recompute the live context so we can show what moved since the save.
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let fresh = gather(&ctx).await?;
    let changes = diff_session_vs_context(&session, &fresh);

    if args.json {
        return output::print_json(&serde_json::json!({
            "session": session,
            "current": fresh,
            "changes": changes,
        }));
    }

    print!("{}", render_session(&session));
    println!();
    if changes.is_empty() {
        println!("No changes since you saved.");
    } else {
        println!("Changes since saved:");
        for c in &changes {
            println!("  - {c}");
        }
    }
    Ok(())
}

/// Outcome of advancing the session's step cursor. Pure, so it's testable
/// without touching disk.
#[derive(Debug, PartialEq)]
enum ContinueOutcome {
    /// The cursor was already past the last step.
    Done,
    Advanced {
        completed: String,
        next: Option<String>,
        /// 1-based position of the completed step.
        position: usize,
        total: usize,
    },
}

/// Advance the cursor one step and report what was completed and what's next.
/// Only moves the in-memory cursor; never runs the step.
fn advance(session: &mut Session) -> ContinueOutcome {
    let total = session.next_steps.len();
    if session.cursor >= total {
        return ContinueOutcome::Done;
    }
    let completed = session.next_steps[session.cursor].clone();
    let position = session.cursor + 1;
    session.cursor += 1;
    let next = session.next_steps.get(session.cursor).cloned();
    ContinueOutcome::Advanced {
        completed,
        next,
        position,
        total,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn continue_(args: ContinueArgs, _host: Option<&str>) -> Result<()> {
    let Some(mut session) = work_session::load()? else {
        bail!("no saved work session for this repo. Run `fj work context` first.");
    };

    if args.dry_run {
        match session.next_steps.get(session.cursor) {
            Some(step) => {
                println!(
                    "would: advance past step {} of {}: {step}",
                    session.cursor + 1,
                    session.next_steps.len()
                );
            }
            None => println!("would: nothing — all suggested steps are already done"),
        }
        return Ok(());
    }

    match advance(&mut session) {
        ContinueOutcome::Done => {
            println!("All suggested steps are done. Run `fj work context` to refresh.");
        }
        ContinueOutcome::Advanced {
            completed,
            next,
            position,
            total,
        } => {
            // Persist the advanced cursor. `continue` is advisory — it never
            // runs the step for you, it just moves the checklist forward.
            work_session::save(&session)?;
            println!("Step {position} of {total}: {completed}");
            println!("(advisory — fj will not run this for you)");
            match next {
                Some(n) => println!("Next: {n}"),
                None => println!("That was the last suggested step."),
            }
        }
    }
    Ok(())
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

    fn sample_session() -> Session {
        to_session(
            &sample_context(),
            Some("git@git.example.com:o/n.git".into()),
            "2026-06-01T08:00:00+00:00".into(),
        )
    }

    #[test]
    fn to_session_carries_fields_and_derives_steps() {
        let s = sample_session();
        assert_eq!(s.repo, "o/n");
        assert_eq!(s.branch.as_deref(), Some("feature/x"));
        assert!(s.dirty);
        assert_eq!(s.pull_request.as_ref().unwrap().number, 7);
        assert_eq!(s.cursor, 0);
        // dirty + ahead(2) + has PR -> commit, push, review steps.
        assert!(!s.next_steps.is_empty());
        assert!(s.next_steps.iter().any(|x| x.contains("uncommitted")));
    }

    #[test]
    fn render_session_shows_snapshot_and_next_step() {
        let out = render_session(&sample_session());
        assert!(out.contains("Saved session (2026-06-01T08:00:00+00:00)"));
        assert!(out.contains("Branch:  feature/x"));
        assert!(out.contains("#7 Add feature [open]"));
        assert!(out.contains("Next:"));
    }

    #[test]
    fn diff_reports_branch_tree_and_pr_changes() {
        let saved = sample_session();
        // Fresh: switched branch, now clean, PR merged.
        let fresh = WorkContext {
            branch: Some("feature/y".into()),
            dirty: false,
            changed_files: 0,
            pull_request: Some(PrSummary {
                number: 7,
                title: "Add feature".into(),
                state: "merged".into(),
                draft: false,
                url: "https://x/pulls/7".into(),
            }),
            ..sample_context()
        };
        let changes = diff_session_vs_context(&saved, &fresh);
        assert!(changes
            .iter()
            .any(|c| c.contains("branch: feature/x -> feature/y")));
        assert!(changes
            .iter()
            .any(|c| c.contains("working tree: 3 file(s) -> clean")));
        assert!(changes.iter().any(|c| c.contains("PR #7: open -> merged")));
    }

    #[test]
    fn diff_is_empty_when_nothing_changed() {
        let saved = sample_session();
        let fresh = sample_context(); // same branch/tree/PR as saved derived from
        assert!(diff_session_vs_context(&saved, &fresh).is_empty());
    }

    fn session_with_steps(steps: &[&str], cursor: usize) -> Session {
        let mut s = sample_session();
        s.next_steps = steps.iter().map(|x| x.to_string()).collect();
        s.cursor = cursor;
        s
    }

    #[test]
    fn advance_walks_steps_then_reports_done() {
        let mut s = session_with_steps(&["commit", "push"], 0);

        let first = advance(&mut s);
        assert_eq!(
            first,
            ContinueOutcome::Advanced {
                completed: "commit".into(),
                next: Some("push".into()),
                position: 1,
                total: 2,
            }
        );
        assert_eq!(s.cursor, 1);

        let second = advance(&mut s);
        assert_eq!(
            second,
            ContinueOutcome::Advanced {
                completed: "push".into(),
                next: None,
                position: 2,
                total: 2,
            }
        );
        assert_eq!(s.cursor, 2);

        // Past the end: Done, and the cursor does not move further.
        assert_eq!(advance(&mut s), ContinueOutcome::Done);
        assert_eq!(s.cursor, 2);
    }
}
