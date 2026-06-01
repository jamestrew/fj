//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Stack state is local and git-native (`<git-dir>/fj/stack.json`, see
//! [`stack_state`](super::stack_state)) — no server state, offline-capable,
//! never a committed manifest. `stack new` (author a stack) and `stack
//! review` (show the stack + each branch's PR state) are implemented.
//! `sync` / `split` / `absorb` / `ship` are still placeholders (dry-run plan,
//! else a structured "not yet implemented" error); the chain rebases land in
//! follow-up PRs.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::stack_state::{self, IssueRef, RepoRef, Stack, StackItem, STACK_VERSION};
use crate::client::Client;
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
    /// Human-readable stack name; the slug and branch names derive from it.
    pub name: String,
    /// Base branch the stack targets. Defaults to the repo's default branch
    /// (when reachable), else `main`.
    #[arg(long)]
    pub base: Option<String>,
    /// Link the stack to an issue, e.g. `--from issue 123`.
    #[arg(long, num_args = 2, value_names = ["TYPE", "ID"])]
    pub from: Option<Vec<String>>,
    /// Create the stack metadata only; don't create the first branch.
    #[arg(long)]
    pub no_branch: bool,
    /// Emit JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
    /// Replace an existing stack file (never deletes git branches).
    #[arg(long)]
    pub force: bool,
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

// ── stack new ────────────────────────────────────────────────────────────

/// Require being inside a git repo. Pure over the detected git dir.
fn require_git_repo(git_dir: Option<PathBuf>) -> Result<PathBuf> {
    git_dir.ok_or_else(|| anyhow::anyhow!("not inside a git repository"))
}

/// Refuse to clobber an existing stack file unless `--force`.
fn guard_overwrite(exists: bool, force: bool) -> Result<()> {
    if exists && !force {
        bail!(
            "a stack already exists for this repo (use --force to replace it; branches are kept)"
        );
    }
    Ok(())
}

/// Parse `--from <TYPE> <ID>`; only `issue` is supported for now.
fn parse_from(from: Option<&[String]>) -> Result<Option<u64>> {
    match from {
        None => Ok(None),
        Some(parts) => {
            let kind = parts.first().map(String::as_str).unwrap_or("");
            if !kind.eq_ignore_ascii_case("issue") {
                bail!("`--from {kind} …` is not supported; only `--from issue <number>`");
            }
            let raw = parts.get(1).map(String::as_str).unwrap_or("");
            let number: u64 = raw
                .parse()
                .map_err(|_| anyhow::anyhow!("`--from issue <number>`: '{raw}' is not a number"))?;
            Ok(Some(number))
        }
    }
}

/// Build the linked-issue record. `fetched` is `(title, url)` when the API
/// returned the issue; `None` (offline / unavailable) keeps them null.
fn issue_ref_from(number: u64, fetched: Option<(String, String)>) -> IssueRef {
    IssueRef {
        number,
        title: fetched.as_ref().map(|(t, _)| t.clone()),
        url: fetched.map(|(_, u)| u),
    }
}

/// Assemble the stack from resolved inputs. Pure: all I/O (git, API, clock)
/// happens in the caller, so this is fully testable.
fn build_stack(
    name: &str,
    base: &str,
    repo: RepoRef,
    issue: Option<IssueRef>,
    handle: &str,
    no_branch: bool,
    now: &str,
) -> Stack {
    let slug = stack_state::slugify(name);
    let items = if no_branch {
        Vec::new()
    } else {
        vec![StackItem {
            id: format!("{slug}-1"),
            title: name.to_string(),
            branch: stack_state::item_branch(handle, &slug, 1),
            base: base.to_string(),
            pr: None,
            status: "local".to_string(),
        }]
    };
    Stack {
        version: STACK_VERSION,
        id: slug.clone(),
        name: name.to_string(),
        slug,
        repo,
        base: base.to_string(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        issue,
        items,
    }
}

/// Human summary for `stack new`.
fn render_new(stack: &Stack, path: &Path, branch: Option<&str>) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "✓ Created stack \"{}\" ({})", stack.name, stack.slug);
    let _ = writeln!(s, "  base:     {}", stack.base);
    let _ = writeln!(s, "  metadata: {}", path.display());
    match branch {
        Some(b) => {
            let _ = writeln!(s, "  branch:   {b}");
        }
        None => {
            let _ = writeln!(s, "  branch:   none (--no-branch)");
        }
    }
    if let Some(issue) = &stack.issue {
        match &issue.title {
            Some(t) => {
                let _ = writeln!(s, "  issue:    #{} {}", issue.number, t);
            }
            None => {
                let _ = writeln!(s, "  issue:    #{} (title unavailable)", issue.number);
            }
        }
    }
    let _ = writeln!(s, "Next: fj stack sync");
    s
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn new(args: NewArgs, host: Option<&str>) -> Result<()> {
    let git_dir = require_git_repo(git::git_dir())?;
    let path = stack_state::stack_path(&git_dir);
    guard_overwrite(path.exists(), args.force)?;

    let issue_number = parse_from(args.from.as_deref())?;

    // Repo identity — offline (git remote / `-R`-less detection).
    let detected = git::remote::discover(host).ok();
    let (remote_name, url) =
        git::primary_remote().unwrap_or_else(|| ("origin".into(), String::new()));
    let repo_root = git::output(&["rev-parse", "--show-toplevel"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let repo_ref = RepoRef {
        path: repo_root,
        remote: remote_name,
        url,
        host: detected
            .as_ref()
            .map(|r| r.host.clone())
            .unwrap_or_default(),
    };

    // Best-effort Forgejo: default branch, account login, linked issue.
    let owner_name = detected.as_ref().map(|r| (r.owner.clone(), r.name.clone()));
    let mut default_branch = None;
    let mut account_login = None;
    let mut issue_fetched = None;
    if let Some((owner, name)) = &owner_name {
        if let Ok(client) = Client::connect(host) {
            if args.base.is_none() {
                default_branch = api::repo::get(&client, owner, name)
                    .await
                    .ok()
                    .map(|r| r.default_branch);
            }
            account_login = api::user::current(&client).await.ok().map(|u| u.login);
            if let Some(n) = issue_number {
                issue_fetched = api::issue::get(&client, owner, name, n)
                    .await
                    .ok()
                    .map(|i| (i.title, i.html_url));
            }
        }
    }

    let base = args
        .base
        .clone()
        .or(default_branch)
        .unwrap_or_else(|| "main".to_string());
    let issue = issue_number.map(|n| issue_ref_from(n, issue_fetched));
    let git_user = git::output(&["config", "user.name"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let handle = stack_state::safe_handle(account_login.as_deref(), git_user.as_deref());

    let now = Utc::now().to_rfc3339();
    let stack = build_stack(
        &args.name,
        &base,
        repo_ref,
        issue,
        &handle,
        args.no_branch,
        &now,
    );

    // Create the first branch unless --no-branch (or it already exists).
    let created_branch = match stack.items.first() {
        Some(item) => {
            if !git::rev_exists(&format!("refs/heads/{}", item.branch)) {
                let start = if git::rev_exists(&base) {
                    Some(base.as_str())
                } else {
                    None
                };
                git::create_branch(&item.branch, start)?;
            }
            Some(item.branch.clone())
        }
        None => None,
    };

    stack_state::save(&stack)?;

    if args.json {
        return output::print_json(&serde_json::to_value(&stack)?);
    }
    print!("{}", render_new(&stack, &path, created_branch.as_deref()));
    Ok(())
}

// ── stack review ───────────────────────────────────────────────────────────

/// A stack plus the live PR state of each of its items. Serializes for JSON.
#[derive(Debug, Serialize)]
struct StackReview {
    name: String,
    base: String,
    entries: Vec<StackEntryView>,
}

#[derive(Debug, Serialize)]
struct StackEntryView {
    branch: String,
    title: String,
    pull_request: Option<EntryPr>,
}

#[derive(Debug, Serialize)]
struct EntryPr {
    number: u64,
    state: String,
    draft: bool,
    url: String,
}

/// Build the review view by matching each stack item's branch to its open PR.
/// Pure — takes the already-fetched PR list.
fn build_review(stack: &Stack, pulls: &[api::pull::Pull]) -> StackReview {
    let entries = stack
        .items
        .iter()
        .map(|item| {
            let pull_request = pulls
                .iter()
                .find(|p| p.head.ref_ == item.branch)
                .map(|p| EntryPr {
                    number: p.number,
                    state: p.state.clone(),
                    draft: p.draft,
                    url: p.html_url.clone(),
                });
            StackEntryView {
                branch: item.branch.clone(),
                title: item.title.clone(),
                pull_request,
            }
        })
        .collect();
    StackReview {
        name: stack.name.clone(),
        base: stack.base.clone(),
        entries,
    }
}

/// Plain-text render of a stack review (no ANSI, so it's easy to test).
fn render_review(r: &StackReview) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "Stack: {}   base: {}", r.name, r.base);
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
    let Some(stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let page = api::pull::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        api::pull::ListOptions::default(),
    )
    .await?;
    let review = build_review(&stack, &page.items);

    if args.json {
        return output::print_json(&serde_json::to_value(&review)?);
    }
    print!("{}", render_review(&review));
    Ok(())
}

// ── placeholders ─────────────────────────────────────────────────────────

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
        Cli::command().debug_assert();
    }

    // ---- arg parsing ----

    #[test]
    fn new_parses_name_base_from_and_flags() {
        let cli = Cli::try_parse_from([
            "fj",
            "stack",
            "new",
            "Login Flow",
            "--base",
            "develop",
            "--from",
            "issue",
            "123",
            "--no-branch",
            "--json",
            "--force",
        ])
        .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::New(a),
            }) => {
                assert_eq!(a.name, "Login Flow");
                assert_eq!(a.base.as_deref(), Some("develop"));
                assert_eq!(
                    a.from.as_deref(),
                    Some(&["issue".to_string(), "123".to_string()][..])
                );
                assert!(a.no_branch);
                assert!(a.json);
                assert!(a.force);
            }
            other => panic!("expected stack new, got {other:?}"),
        }
    }

    #[test]
    fn new_requires_a_name() {
        assert!(Cli::try_parse_from(["fj", "stack", "new"]).is_err());
    }

    #[test]
    fn review_parses_json_and_repo() {
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
    fn sync_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "stack", "sync", "--dry-run"]).expect("should parse");
        assert!(matches!(
            cli.command,
            Command::Stack(StackCmd {
                command: StackSub::Sync(SyncArgs { dry_run: true, .. })
            })
        ));
    }

    // ---- guards ----

    #[test]
    fn require_git_repo_errors_without_a_repo() {
        assert!(require_git_repo(None).is_err());
        assert_eq!(
            require_git_repo(Some(PathBuf::from("/x/.git"))).unwrap(),
            PathBuf::from("/x/.git")
        );
    }

    #[test]
    fn guard_overwrite_blocks_existing_unless_forced() {
        assert!(guard_overwrite(false, false).is_ok()); // no file -> ok
        assert!(guard_overwrite(true, false).is_err()); // file, no force -> err
        assert!(guard_overwrite(true, true).is_ok()); // file + force -> ok
    }

    #[test]
    fn parse_from_validates_issue() {
        assert_eq!(parse_from(None).unwrap(), None);
        assert_eq!(
            parse_from(Some(&["issue".into(), "123".into()])).unwrap(),
            Some(123)
        );
        assert!(parse_from(Some(&["pr".into(), "1".into()])).is_err());
        assert!(parse_from(Some(&["issue".into(), "nope".into()])).is_err());
    }

    #[test]
    fn issue_ref_from_handles_success_and_unavailable() {
        let ok = issue_ref_from(123, Some(("Title".into(), "https://x/i/123".into())));
        assert_eq!(ok.number, 123);
        assert_eq!(ok.title.as_deref(), Some("Title"));
        assert_eq!(ok.url.as_deref(), Some("https://x/i/123"));

        let degraded = issue_ref_from(123, None);
        assert_eq!(degraded.number, 123);
        assert!(degraded.title.is_none());
        assert!(degraded.url.is_none());
    }

    // ---- build_stack ----

    fn repo_ref() -> RepoRef {
        RepoRef {
            path: "/repo".into(),
            remote: "origin".into(),
            url: "ssh://git@host/o/r.git".into(),
            host: "host".into(),
        }
    }

    #[test]
    fn build_stack_happy_path() {
        let s = build_stack(
            "Login Flow",
            "main",
            repo_ref(),
            None,
            "stephen",
            false,
            "2026-06-01T00:00:00+00:00",
        );
        assert_eq!(s.version, STACK_VERSION);
        assert_eq!(s.name, "Login Flow");
        assert_eq!(s.slug, "login-flow");
        assert_eq!(s.id, "login-flow");
        assert_eq!(s.base, "main");
        assert_eq!(s.created_at, s.updated_at);
        assert!(s.issue.is_none());
        assert_eq!(s.items.len(), 1);
        let item = &s.items[0];
        assert_eq!(item.id, "login-flow-1");
        assert_eq!(item.title, "Login Flow");
        assert_eq!(item.branch, "stephen/login-flow-1");
        assert_eq!(item.base, "main");
        assert!(item.pr.is_none());
        assert_eq!(item.status, "local");
    }

    #[test]
    fn build_stack_custom_base_and_no_branch() {
        let s = build_stack(
            "Login Flow",
            "develop",
            repo_ref(),
            None,
            "stephen",
            true,
            "t",
        );
        assert_eq!(s.base, "develop");
        // --no-branch -> empty items (no first branch authored yet).
        assert!(s.items.is_empty());
    }

    #[test]
    fn build_stack_with_linked_issue() {
        let issue = issue_ref_from(7, Some(("Add login".into(), "https://x/i/7".into())));
        let s = build_stack("Login", "main", repo_ref(), Some(issue), "user", false, "t");
        let i = s.issue.unwrap();
        assert_eq!(i.number, 7);
        assert_eq!(i.title.as_deref(), Some("Add login"));
    }

    #[test]
    fn new_json_shape_is_stable() {
        let issue = issue_ref_from(123, None);
        let s = build_stack(
            "Login Flow",
            "main",
            repo_ref(),
            Some(issue),
            "stephen",
            false,
            "2026-06-01T00:00:00+00:00",
        );
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["id"], "login-flow");
        assert_eq!(v["name"], "Login Flow");
        assert_eq!(v["slug"], "login-flow");
        assert_eq!(v["repo"]["remote"], "origin");
        assert_eq!(v["repo"]["host"], "host");
        assert_eq!(v["base"], "main");
        assert_eq!(v["issue"]["number"], 123);
        assert!(v["issue"]["title"].is_null());
        assert_eq!(v["items"][0]["branch"], "stephen/login-flow-1");
        assert_eq!(v["items"][0]["status"], "local");
        assert!(v["items"][0]["pr"].is_null());
    }

    #[test]
    fn render_new_mentions_branch_base_and_next() {
        let s = build_stack(
            "Login Flow",
            "main",
            repo_ref(),
            None,
            "stephen",
            false,
            "t",
        );
        let out = render_new(
            &s,
            Path::new("/repo/.git/fj/stack.json"),
            Some("stephen/login-flow-1"),
        );
        assert!(out.contains("Created stack \"Login Flow\" (login-flow)"));
        assert!(out.contains("base:     main"));
        assert!(out.contains("/repo/.git/fj/stack.json"));
        assert!(out.contains("branch:   stephen/login-flow-1"));
        assert!(out.contains("Next: fj stack sync"));
    }

    #[test]
    fn render_new_no_branch() {
        let s = build_stack("X", "main", repo_ref(), None, "u", true, "t");
        let out = render_new(&s, Path::new("/p"), None);
        assert!(out.contains("branch:   none (--no-branch)"));
    }

    // ---- review ----

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

    fn stack_with_branches(branches: &[&str]) -> Stack {
        let items = branches
            .iter()
            .enumerate()
            .map(|(i, b)| StackItem {
                id: format!("feat-{}", i + 1),
                title: format!("Item {}", i + 1),
                branch: b.to_string(),
                base: "main".into(),
                pr: None,
                status: "local".into(),
            })
            .collect();
        Stack {
            version: STACK_VERSION,
            id: "feat".into(),
            name: "feat".into(),
            slug: "feat".into(),
            repo: repo_ref(),
            base: "main".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            issue: None,
            items,
        }
    }

    #[test]
    fn build_review_matches_items_to_prs() {
        let pulls = vec![
            pull_with_head(7, "feature/a", "open"),
            pull_with_head(9, "other", "open"),
        ];
        let r = build_review(&stack_with_branches(&["feature/a", "feature/b"]), &pulls);
        assert_eq!(r.name, "feat");
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].pull_request.as_ref().unwrap().number, 7);
        assert!(r.entries[1].pull_request.is_none());
    }

    #[test]
    fn render_review_shows_branches_and_pr_state() {
        let pulls = vec![pull_with_head(7, "feature/a", "open")];
        let out = render_review(&build_review(
            &stack_with_branches(&["feature/a", "feature/b"]),
            &pulls,
        ));
        assert!(out.contains("Stack: feat   base: main"));
        assert!(out.contains("feature/a  #7 [open]"));
        assert!(out.contains("feature/b  (no PR)"));
    }
}
