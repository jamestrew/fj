//! `fj work` — working-context / session-resumption commands.
//!
//! `work context` summarizes "where am I": repo + host, Fjord account/auth,
//! branch + working-tree state + ahead/behind, and — best-effort over the
//! Forgejo API — the default branch, the open PR for the branch, and its
//! combined CI/check status, plus the current local stack (`.git/fj/
//! stack.json`). Local data is gathered offline; the API parts degrade to
//! `null` (with `api_reachable: false`) when the server/token is unavailable
//! rather than failing. Table or `--json` (stable shape for integrations).
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
use crate::cli::context::RepoFlag;
use crate::cli::stack_state::{self, Stack};
use crate::cli::work_session::{self, PrRef, Session};
use crate::client::Client;
use crate::config::hosts::{AuthKind, Host, Hosts};
use crate::git::remote::RemoteRepo;
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

/// A snapshot of "where am I right now". Local-only fields are always
/// populated (offline); the Forgejo-derived fields (`default_branch`,
/// `pull_request`, `checks`) are best-effort and left `null` when the API
/// isn't reachable — `api_reachable` says which happened. Serializes directly
/// for `--json` (the global `--json-fields` projection still applies); the
/// shape is intended to be stable for future integrations.
#[derive(Debug, Serialize)]
struct WorkContext {
    /// `owner/name`, or None when not in a repo and no `-R` was given.
    repo: Option<String>,
    host: Option<String>,
    account: AccountInfo,
    /// None when HEAD is detached or we're not inside a git repo.
    branch: Option<String>,
    /// Repo default branch (Forgejo); None when the API wasn't reachable.
    default_branch: Option<String>,
    dirty: bool,
    changed_files: usize,
    /// ahead/behind the branch's upstream; None when there is no upstream.
    ahead: Option<u32>,
    behind: Option<u32>,
    /// The open PR whose head is the current branch (Forgejo, best-effort).
    pull_request: Option<PrSummary>,
    /// Latest combined CI/check status for the PR's head (best-effort).
    checks: Option<CheckSummary>,
    /// The current local stack, if `.git/fj/stack.json` has one.
    stack: Option<StackSummary>,
    /// Whether the Forgejo API was reached for this run.
    api_reachable: bool,
}

/// The host's auth context, including the Fjord account when in Fjord mode.
#[derive(Debug, Serialize, PartialEq)]
struct AccountInfo {
    /// "pat", "fjord", or None when the host isn't configured.
    auth: Option<String>,
    fjord_platform: Option<String>,
    fjord_instance: Option<String>,
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

#[derive(Debug, Serialize, PartialEq)]
struct CheckSummary {
    /// Combined state: success / pending / failure / error / "none".
    state: String,
    total: u64,
    sha: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct StackSummary {
    name: String,
    base: String,
    branches: Vec<String>,
}

/// Repo identity resolved offline (git remote / `-R` / configured default).
#[derive(Debug, PartialEq)]
struct Identity {
    repo: Option<String>,
    owner: Option<String>,
    name: Option<String>,
    host: Option<String>,
}

/// Resolve repo identity without any network or keychain access. `-R` (a
/// `owner/name` slug) wins for the repo; the host comes from `--host`, then
/// the git remote, then the configured default. Pure for testability.
fn resolve_identity(
    repo_flag: Option<&str>,
    host_flag: Option<&str>,
    remote: Option<&RemoteRepo>,
    default_host: Option<&str>,
) -> Identity {
    let (owner, name) = match repo_flag.and_then(|s| api::split_repo(s).ok()) {
        Some((o, n)) => (Some(o.to_string()), Some(n.to_string())),
        None => match remote {
            Some(r) => (Some(r.owner.clone()), Some(r.name.clone())),
            None => (None, None),
        },
    };
    let host = host_flag
        .map(str::to_string)
        .or_else(|| remote.map(|r| r.host.clone()))
        .or_else(|| default_host.map(str::to_string));
    let repo = match (&owner, &name) {
        (Some(o), Some(n)) => Some(format!("{o}/{n}")),
        _ => None,
    };
    Identity {
        repo,
        owner,
        name,
        host,
    }
}

/// Auth/account context from the host config (offline).
fn account_info(host: Option<&Host>) -> AccountInfo {
    match host {
        None => AccountInfo {
            auth: None,
            fjord_platform: None,
            fjord_instance: None,
        },
        Some(h) => match h.auth_kind {
            AuthKind::Pat => AccountInfo {
                auth: Some("pat".into()),
                fjord_platform: None,
                fjord_instance: None,
            },
            AuthKind::FjordAccount => {
                let fa = h.fjord_account.as_ref();
                AccountInfo {
                    auth: Some("fjord".into()),
                    fjord_platform: fa.map(|f| f.platform_url.clone()),
                    fjord_instance: fa.and_then(|f| f.default_instance_display_name.clone()),
                }
            }
        },
    }
}

/// Summarize a combined commit status into the compact check view.
fn summarize_checks(cs: &api::pull::CombinedStatus) -> CheckSummary {
    CheckSummary {
        state: if cs.state.is_empty() {
            "none".to_string()
        } else {
            cs.state.clone()
        },
        total: cs.total_count,
        sha: cs.sha.clone(),
    }
}

/// Compact summary of the repo's local stack.
fn stack_summary(s: &Stack) -> StackSummary {
    StackSummary {
        name: s.name.clone(),
        base: s.base.clone(),
        branches: s.items.iter().map(|i| i.branch.clone()).collect(),
    }
}

/// Find the open PR whose head ref matches `branch`.
fn find_pr_for_branch<'a>(
    pulls: &'a [api::pull::Pull],
    branch: &str,
) -> Option<&'a api::pull::Pull> {
    pulls.iter().find(|p| p.head.ref_ == branch)
}

/// Render a compact, useful summary (plain text, no ANSI, so it's trivially
/// testable). Absent/best-effort data is shown briefly rather than omitted so
/// the layout is predictable. `--json` takes a different path.
fn render_human(wc: &WorkContext) -> String {
    use std::fmt::Write;
    let mut s = String::new();

    let repo = wc.repo.as_deref().unwrap_or("(not a git repository)");
    let host = wc.host.as_deref().unwrap_or("?");
    let _ = writeln!(s, "Repo:    {repo} ({host})");

    // Account / context line.
    let acct = match (&wc.account.auth, &wc.account.fjord_platform) {
        (Some(a), Some(p)) if a == "fjord" => {
            let inst = wc
                .account
                .fjord_instance
                .as_deref()
                .unwrap_or("no instance");
            format!("fjord — {p} [{inst}]")
        }
        (Some(a), _) => a.clone(),
        (None, _) => "not signed in".to_string(),
    };
    let _ = writeln!(s, "Account: {acct}");

    // Branch + working tree + tracking, on one compact line.
    let branch = wc.branch.as_deref().unwrap_or("(detached)");
    let tree = if wc.dirty {
        format!("{} uncommitted", wc.changed_files)
    } else {
        "clean".to_string()
    };
    let default = wc.default_branch.as_deref().unwrap_or("?");
    let mut branch_line = format!("Branch:  {branch} ({tree}; default {default})");
    if let (Some(a), Some(b)) = (wc.ahead, wc.behind) {
        let _ = write!(branch_line, "  ↑{a} ↓{b}");
    }
    let _ = writeln!(s, "{branch_line}");

    // PR + checks.
    match &wc.pull_request {
        Some(pr) => {
            let draft = if pr.draft { " draft" } else { "" };
            let checks = match &wc.checks {
                Some(c) => format!("  checks: {} ({})", c.state, c.total),
                None => String::new(),
            };
            let _ = writeln!(
                s,
                "PR:      #{} {} [{}{}]{}  {}",
                pr.number, pr.title, pr.state, draft, checks, pr.url
            );
        }
        None if !wc.api_reachable => {
            let _ = writeln!(s, "PR:      (Forgejo API not reachable)");
        }
        None => {
            let _ = writeln!(s, "PR:      (none open for this branch)");
        }
    }

    // Stack (only when present).
    if let Some(st) = &wc.stack {
        let _ = writeln!(
            s,
            "Stack:   {} (base {}, {} branch(es))",
            st.name,
            st.base,
            st.branches.len()
        );
    }
    s
}

/// Gather the working context. Local git/config/stack data is always
/// collected (offline); the Forgejo API is best-effort and any failure
/// (offline, no token, server down) degrades to `None` rather than erroring.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn gather(repo_flag: Option<&str>, host_flag: Option<&str>) -> Result<WorkContext> {
    // --- local, offline ---
    let remote = git::remote::discover(host_flag).ok();
    let hosts = Hosts::load().ok();
    let default_host = hosts.as_ref().and_then(|h| h.current.as_deref());
    let id = resolve_identity(repo_flag, host_flag, remote.as_ref(), default_host);

    let branch = git::current_branch();
    let (dirty, changed_files) = git::working_tree_status().unwrap_or((false, 0));
    let (ahead, behind) = match git::ahead_behind_upstream() {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    let host_cfg = hosts
        .as_ref()
        .zip(id.host.as_deref())
        .and_then(|(h, name)| h.hosts.get(name));
    let account = account_info(host_cfg);
    let stack = stack_state::load()
        .ok()
        .flatten()
        .map(|s| stack_summary(&s));

    // --- Forgejo API, best-effort ---
    let mut default_branch = None;
    let mut pull_request = None;
    let mut checks = None;
    let mut api_reachable = false;
    if let (Some(owner), Some(name)) = (id.owner.as_deref(), id.name.as_deref()) {
        if let Ok(client) = Client::connect(host_flag) {
            if let Ok(repo) = api::repo::get(&client, owner, name).await {
                api_reachable = true;
                default_branch = Some(repo.default_branch);
                if let Some(b) = branch.as_deref() {
                    if let Ok(page) =
                        api::pull::list(&client, owner, name, api::pull::ListOptions::default())
                            .await
                    {
                        if let Some(pr) = find_pr_for_branch(&page.items, b) {
                            pull_request = Some(PrSummary::from_pull(pr));
                            if let Ok(cs) =
                                api::pull::combined_status(&client, owner, name, &pr.head.sha).await
                            {
                                checks = Some(summarize_checks(&cs));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(WorkContext {
        repo: id.repo,
        host: id.host,
        account,
        branch,
        default_branch,
        dirty,
        changed_files,
        ahead,
        behind,
        pull_request,
        checks,
        stack,
        api_reachable,
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
        host: wc.host.clone().unwrap_or_default(),
        repo: wc.repo.clone().unwrap_or_default(),
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
    let wc = gather(args.r.repo.as_deref(), host).await?;

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
    let fresh = gather(args.r.repo.as_deref(), host).await?;
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
            repo: Some("o/n".into()),
            host: Some("git.example.com".into()),
            account: AccountInfo {
                auth: Some("pat".into()),
                fjord_platform: None,
                fjord_instance: None,
            },
            branch: Some("feature/x".into()),
            default_branch: Some("main".into()),
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
            checks: Some(CheckSummary {
                state: "success".into(),
                total: 3,
                sha: "aaa".into(),
            }),
            stack: Some(StackSummary {
                name: "feat".into(),
                base: "main".into(),
                branches: vec!["feature/x".into()],
            }),
            api_reachable: true,
        }
    }

    #[test]
    fn render_human_includes_branch_tracking_pr_and_stack() {
        let out = render_human(&sample_context());
        assert!(out.contains("o/n (git.example.com)"));
        assert!(out.contains("feature/x (3 uncommitted; default main)"));
        assert!(out.contains("↑2 ↓1"));
        assert!(out.contains("#7 Add feature [open]"));
        assert!(out.contains("checks: success (3)"));
        assert!(out.contains("Stack:   feat (base main, 1 branch(es))"));
    }

    #[test]
    fn render_human_degrades_when_api_unreachable() {
        let wc = WorkContext {
            host: None,
            account: AccountInfo {
                auth: None,
                fjord_platform: None,
                fjord_instance: None,
            },
            branch: None,
            default_branch: None,
            dirty: false,
            changed_files: 0,
            ahead: None,
            behind: None,
            pull_request: None,
            checks: None,
            stack: None,
            api_reachable: false,
            ..sample_context()
        };
        let out = render_human(&wc);
        assert!(out.contains("(detached)"));
        assert!(out.contains("clean; default ?"));
        assert!(out.contains("Forgejo API not reachable"));
        assert!(out.contains("not signed in"));
        // No stack line when there is no stack.
        assert!(!out.contains("Stack:"));
        // No tracking arrows when ahead/behind are unknown.
        assert!(!out.contains('↑'));
    }

    #[test]
    fn work_context_json_shape_is_stable() {
        let v = serde_json::to_value(sample_context()).unwrap();
        assert_eq!(v["repo"], "o/n");
        assert_eq!(v["host"], "git.example.com");
        assert_eq!(v["account"]["auth"], "pat");
        assert_eq!(v["default_branch"], "main");
        assert_eq!(v["branch"], "feature/x");
        assert_eq!(v["dirty"], true);
        assert_eq!(v["changed_files"], 3);
        assert_eq!(v["ahead"], 2);
        assert_eq!(v["pull_request"]["number"], 7);
        assert_eq!(v["checks"]["state"], "success");
        assert_eq!(v["checks"]["total"], 3);
        assert_eq!(v["stack"]["name"], "feat");
        assert_eq!(v["api_reachable"], true);
    }

    #[test]
    fn resolve_identity_prefers_flag_repo_and_host() {
        let remote = RemoteRepo {
            host: "remote.host".into(),
            owner: "ro".into(),
            name: "rn".into(),
        };
        let id = resolve_identity(
            Some("o/n"),
            Some("flag.host"),
            Some(&remote),
            Some("def.host"),
        );
        assert_eq!(id.repo.as_deref(), Some("o/n"));
        assert_eq!(id.owner.as_deref(), Some("o"));
        assert_eq!(id.host.as_deref(), Some("flag.host"));
    }

    #[test]
    fn resolve_identity_falls_back_to_remote_then_default_host() {
        let remote = RemoteRepo {
            host: "remote.host".into(),
            owner: "ro".into(),
            name: "rn".into(),
        };
        let id = resolve_identity(None, None, Some(&remote), Some("def.host"));
        assert_eq!(id.repo.as_deref(), Some("ro/rn"));
        assert_eq!(id.host.as_deref(), Some("remote.host"));

        // No remote: repo unknown, host falls back to the configured default.
        let id = resolve_identity(None, None, None, Some("def.host"));
        assert_eq!(id.repo, None);
        assert_eq!(id.owner, None);
        assert_eq!(id.host.as_deref(), Some("def.host"));
    }

    #[test]
    fn account_info_reflects_auth_kind() {
        assert_eq!(
            account_info(None),
            AccountInfo {
                auth: None,
                fjord_platform: None,
                fjord_instance: None
            }
        );
        let pat = Host::default(); // AuthKind defaults to Pat
        assert_eq!(account_info(Some(&pat)).auth.as_deref(), Some("pat"));

        let fjord: Host = serde_json::from_value(serde_json::json!({
            "auth_kind": "fjord_account",
            "fjord_account": {
                "platform_url": "https://platform.example",
                "default_instance_display_name": "My Forge"
            }
        }))
        .unwrap();
        let a = account_info(Some(&fjord));
        assert_eq!(a.auth.as_deref(), Some("fjord"));
        assert_eq!(
            a.fjord_platform.as_deref(),
            Some("https://platform.example")
        );
        assert_eq!(a.fjord_instance.as_deref(), Some("My Forge"));
    }

    #[test]
    fn summarize_checks_maps_state_and_empty() {
        let cs: api::pull::CombinedStatus = serde_json::from_value(serde_json::json!({
            "state": "success", "sha": "abc", "total_count": 4, "statuses": []
        }))
        .unwrap();
        let c = summarize_checks(&cs);
        assert_eq!(c.state, "success");
        assert_eq!(c.total, 4);
        assert_eq!(c.sha, "abc");

        let empty: api::pull::CombinedStatus =
            serde_json::from_value(serde_json::json!({ "sha": "abc" })).unwrap();
        assert_eq!(summarize_checks(&empty).state, "none");
    }

    #[test]
    fn stack_summary_flattens_item_branches() {
        let stack: Stack = serde_json::from_value(serde_json::json!({
            "version": 1, "id": "feat", "name": "feat", "slug": "feat",
            "repo": { "path": "/r", "remote": "origin", "url": "u", "host": "h" },
            "base": "main", "created_at": "t", "updated_at": "t", "issue": null,
            "items": [
                { "id": "feat-1", "title": "a", "branch": "u/feat-1", "base": "main", "pr": null, "status": "local" },
                { "id": "feat-2", "title": "b", "branch": "u/feat-2", "base": "main", "pr": null, "status": "local" }
            ]
        }))
        .unwrap();
        let s = stack_summary(&stack);
        assert_eq!(s.name, "feat");
        assert_eq!(s.base, "main");
        assert_eq!(s.branches, vec!["u/feat-1", "u/feat-2"]);
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
