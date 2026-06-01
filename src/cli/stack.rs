//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Stack state is local and git-native (`<git-dir>/fj/stack.json`, see
//! [`stack_state`](super::stack_state)) — no server state, offline-capable,
//! never a committed manifest. `stack new` (author a stack), `stack review`
//! (show the stack + each branch's PR state), and `stack sync` (push each
//! branch and create/update its Forgejo PR, chaining bases so each PR targets
//! the previous branch) are implemented. `split` / `absorb` / `ship` are
//! still placeholders (dry-run plan, else a structured "not yet implemented").

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::api::pull::CreatePull;
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::stack_state::{self, IssueRef, ItemPr, RepoRef, Stack, StackItem, STACK_VERSION};
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
    /// Create new PRs as drafts.
    #[arg(long, conflicts_with = "ready")]
    pub draft: bool,
    /// Mark the stack's PRs ready for review.
    #[arg(long)]
    pub ready: bool,
    /// Emit machine-readable operations/results as JSON.
    #[arg(long)]
    pub json: bool,
    /// Print intended actions without pushing or writing to Forgejo.
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
    /// Open each PR URL in your browser.
    #[arg(long)]
    pub open: bool,
    /// Request review from a Forgejo user/team on every PR in the stack.
    #[arg(long, value_name = "USER")]
    pub request: Option<String>,
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

/// A stack plus the live review state of each of its items. Stable for JSON.
#[derive(Debug, Serialize)]
struct StackReview {
    name: String,
    base: String,
    entries: Vec<StackEntryView>,
}

#[derive(Debug, Serialize)]
struct StackEntryView {
    branch: String,
    /// The stack item's title.
    title: String,
    /// The PR for this branch, or null when none exists / is unreachable.
    pull_request: Option<EntryPr>,
}

#[derive(Debug, Serialize, PartialEq)]
struct EntryPr {
    number: u64,
    /// PR title (may differ from the item title).
    title: String,
    state: String,
    draft: bool,
    url: String,
    /// "approved" | "changes_requested" | "commented" | "none".
    review: String,
    /// Combined CI state, or null when unavailable.
    checks: Option<String>,
    /// Mergeability for open PRs; null when not applicable / unknown.
    mergeable: Option<bool>,
    /// Logins with a pending review request.
    requested_reviewers: Vec<String>,
}

/// Aggregate PR review state. Request-changes dominates an approval; an
/// approval dominates plain comments; no reviews → "none".
fn summarize_reviews(reviews: &[api::pull::Review]) -> String {
    let has = |s: &str| reviews.iter().any(|r| r.state.eq_ignore_ascii_case(s));
    if has("REQUEST_CHANGES") {
        "changes_requested".to_string()
    } else if has("APPROVED") {
        "approved".to_string()
    } else if reviews.is_empty() {
        "none".to_string()
    } else {
        "commented".to_string()
    }
}

/// Assemble the per-PR view from already-fetched pieces. Pure for testing.
fn build_entry_pr(full: &api::pull::Pull, review: String, checks: Option<String>) -> EntryPr {
    EntryPr {
        number: full.number,
        title: full.title.clone(),
        state: full.state.clone(),
        draft: full.draft,
        url: full.html_url.clone(),
        review,
        checks,
        // Mergeability is only meaningful while the PR is open.
        mergeable: (full.state == "open").then_some(full.mergeable),
        requested_reviewers: full
            .requested_reviewers
            .iter()
            .map(|u| u.login.clone())
            .collect(),
    }
}

/// Resolve the PR number for an item: a live open PR matching the branch,
/// else the number stored by a previous `sync` (covers merged/closed PRs).
fn entry_pr_number(item: &StackItem, open_pulls: &[api::pull::Pull]) -> Option<u64> {
    open_pulls
        .iter()
        .find(|p| p.head.ref_ == item.branch)
        .map(|p| p.number)
        .or_else(|| item.pr.as_ref().map(|p| p.number))
}

/// Fetch the full PR plus its reviews and CI status, best-effort. Returns
/// `None` when the PR can't be fetched (deleted / unavailable) — graceful.
async fn enrich_pr(client: &Client, owner: &str, name: &str, number: u64) -> Option<EntryPr> {
    let full = api::pull::get(client, owner, name, number).await.ok()?;
    let reviews = api::pull::list_reviews(client, owner, name, number)
        .await
        .unwrap_or_default();
    let checks = api::pull::combined_status(client, owner, name, &full.head.sha)
        .await
        .ok()
        .map(|cs| {
            if cs.state.is_empty() {
                "none".to_string()
            } else {
                cs.state
            }
        });
    Some(build_entry_pr(&full, summarize_reviews(&reviews), checks))
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
                let draft = if pr.draft { " draft" } else { "" };
                let checks = pr.checks.as_deref().unwrap_or("?");
                let merge = match pr.mergeable {
                    Some(true) => "mergeable",
                    Some(false) => "conflicts",
                    None => "-",
                };
                let _ = writeln!(
                    s,
                    "  {}  #{} {} [{}{}]  review:{}  ci:{}  {}",
                    e.branch, pr.number, pr.title, pr.state, draft, pr.review, checks, merge
                );
                if !pr.requested_reviewers.is_empty() {
                    let _ = writeln!(s, "       requested: {}", pr.requested_reviewers.join(", "));
                }
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

    // --request: ask the user for review on each item's PR before reporting,
    // so the requested-reviewers column reflects it.
    if let Some(user) = &args.request {
        for item in &stack.items {
            if let Some(n) = entry_pr_number(item, &page.items) {
                api::pull::request_reviewers(
                    &ctx.client,
                    &ctx.owner,
                    &ctx.name,
                    n,
                    vec![user.clone()],
                )
                .await?;
            }
        }
    }

    let mut entries = Vec::with_capacity(stack.items.len());
    for item in &stack.items {
        let pull_request = match entry_pr_number(item, &page.items) {
            Some(n) => enrich_pr(&ctx.client, &ctx.owner, &ctx.name, n).await,
            None => None,
        };
        entries.push(StackEntryView {
            branch: item.branch.clone(),
            title: item.title.clone(),
            pull_request,
        });
    }
    let review = StackReview {
        name: stack.name.clone(),
        base: stack.base.clone(),
        entries,
    };

    if args.open {
        for e in &review.entries {
            if let Some(pr) = &e.pull_request {
                let _ = crate::cli::web::open(&pr.url);
            }
        }
    }

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

// ── stack sync ─────────────────────────────────────────────────────────────

/// The PR base for stack item `i`: the previous item's branch, or the stack
/// base for the first item.
fn pr_base(stack_base: &str, items: &[StackItem], i: usize) -> String {
    if i == 0 {
        stack_base.to_string()
    } else {
        items[i - 1].branch.clone()
    }
}

/// Validate the stack is syncable: at least one item, and unique, non-empty
/// branch names (a malformed/duplicated chain would push/PR incorrectly).
fn validate_items(items: &[StackItem]) -> Result<()> {
    if items.is_empty() {
        bail!("stack has no items to sync (author one with `fj stack new`)");
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        if item.branch.trim().is_empty() {
            bail!("stack item '{}' has an empty branch name", item.id);
        }
        if !seen.insert(item.branch.as_str()) {
            bail!("duplicate branch '{}' in the stack", item.branch);
        }
    }
    Ok(())
}

/// What a sync intends to do for one item. Serializes for `--json` / dry-run.
#[derive(Debug, Serialize, PartialEq)]
struct PlannedOp {
    branch: String,
    base: String,
    title: String,
}

fn plan_sync(stack: &Stack) -> Vec<PlannedOp> {
    stack
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| PlannedOp {
            branch: item.branch.clone(),
            base: pr_base(&stack.base, &stack.items, i),
            title: item.title.clone(),
        })
        .collect()
}

fn render_plan(plan: &[PlannedOp]) -> String {
    use std::fmt::Write;
    let mut s = String::from("Would sync (no pushes or PR writes):\n");
    for op in plan {
        let _ = writeln!(
            s,
            "  push {0} → origin; ensure PR {0} → {1}",
            op.branch, op.base
        );
    }
    s
}

/// The result of syncing one item. Serializes for `--json`.
#[derive(Debug, Serialize)]
struct SyncOp {
    branch: String,
    base: String,
    /// "created" | "updated"
    action: String,
    pr: ItemPr,
}

fn render_ops(ops: &[SyncOp]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for op in ops {
        let _ = writeln!(
            s,
            "✓ {} {} → {}  (#{} {})",
            op.action, op.branch, op.base, op.pr.number, op.pr.url
        );
    }
    let _ = writeln!(s, "Synced {} item(s).", ops.len());
    s
}

/// How sync should set PR draft/ready state.
#[derive(Debug, Clone, Copy)]
struct PrMode {
    draft: bool,
    ready: bool,
}

/// Create the PR for `branch` if missing, else record (and optionally mark
/// ready) the existing one. Idempotent: re-running finds the existing PR and
/// updates rather than duplicating. Returns `(action, pr)`.
async fn ensure_pr(
    client: &Client,
    owner: &str,
    name: &str,
    branch: &str,
    base: &str,
    title: &str,
    mode: PrMode,
) -> Result<(String, ItemPr)> {
    let page = api::pull::list(client, owner, name, api::pull::ListOptions::default()).await?;
    if let Some(existing) = page.items.iter().find(|p| p.head.ref_ == branch) {
        let mut state = existing.state.clone();
        if mode.ready && existing.draft {
            api::pull::ready(client, owner, name, existing.number).await?;
            state = "open".to_string();
        }
        return Ok((
            "updated".to_string(),
            ItemPr {
                number: existing.number,
                url: existing.html_url.clone(),
                state,
            },
        ));
    }
    let created = api::pull::create(
        client,
        owner,
        name,
        &CreatePull {
            title,
            head: branch,
            base,
            body: None,
            draft: mode.draft,
        },
    )
    .await?;
    Ok((
        "created".to_string(),
        ItemPr {
            number: created.number,
            url: created.html_url,
            state: created.state,
        },
    ))
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn sync(args: SyncArgs, host: Option<&str>) -> Result<()> {
    let Some(mut stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    validate_items(&stack.items)?;

    if args.dry_run {
        let plan = plan_sync(&stack);
        if args.json {
            return output::print_json(&serde_json::to_value(&plan)?);
        }
        print!("{}", render_plan(&plan));
        return Ok(());
    }

    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let mode = PrMode {
        draft: args.draft,
        ready: args.ready,
    };
    let remote = stack.repo.remote.clone();
    let bases: Vec<String> = (0..stack.items.len())
        .map(|i| pr_base(&stack.base, &stack.items, i))
        .collect();

    let mut ops = Vec::with_capacity(stack.items.len());
    for (i, base) in bases.iter().enumerate() {
        let branch = stack.items[i].branch.clone();
        let title = stack.items[i].title.clone();

        git::push(&remote, &branch)?;
        let (action, pr) = ensure_pr(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            &branch,
            base,
            &title,
            mode,
        )
        .await?;

        stack.items[i].pr = Some(pr.clone());
        stack.items[i].status = "pushed".to_string();
        // Persist incrementally so a mid-run failure keeps completed progress
        // (and a re-run is idempotent from where it stopped).
        stack.updated_at = Utc::now().to_rfc3339();
        stack_state::save(&stack)?;

        ops.push(SyncOp {
            branch,
            base: base.clone(),
            action,
            pr,
        });
    }

    if args.json {
        return output::print_json(&serde_json::to_value(&ops)?);
    }
    print!("{}", render_ops(&ops));
    Ok(())
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
    fn review_parses_open_request_and_json() {
        let cli = Cli::try_parse_from([
            "fj",
            "stack",
            "review",
            "--open",
            "--request",
            "bob",
            "--json",
        ])
        .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Review(a),
            }) => {
                assert!(a.open && a.json);
                assert_eq!(a.request.as_deref(), Some("bob"));
            }
            other => panic!("expected stack review, got {other:?}"),
        }
    }

    fn review_value(state: &str) -> api::pull::Review {
        serde_json::from_value(serde_json::json!({
            "id": 1, "state": state, "user": { "id": 1, "login": "bob" },
            "html_url": "https://x/r/1"
        }))
        .unwrap()
    }

    #[test]
    fn summarize_reviews_aggregates() {
        assert_eq!(summarize_reviews(&[]), "none");
        assert_eq!(summarize_reviews(&[review_value("COMMENT")]), "commented");
        assert_eq!(summarize_reviews(&[review_value("APPROVED")]), "approved");
        // request-changes dominates an approval
        assert_eq!(
            summarize_reviews(&[review_value("APPROVED"), review_value("REQUEST_CHANGES")]),
            "changes_requested"
        );
    }

    fn full_pull(number: u64, state: &str, mergeable: bool, reviewers: &[&str]) -> api::pull::Pull {
        serde_json::from_value(serde_json::json!({
            "id": number, "number": number, "title": "PR title", "state": state,
            "user": { "id": 1, "login": "alice" },
            "html_url": format!("https://x/pulls/{number}"),
            "head": { "ref": "feature/a", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "draft": false, "mergeable": mergeable,
            "requested_reviewers": reviewers.iter().map(|l| serde_json::json!({"id":1,"login":l})).collect::<Vec<_>>(),
            "created_at": "2026-06-01T00:00:00Z", "updated_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn build_entry_pr_carries_review_checks_mergeable_reviewers() {
        let pr = build_entry_pr(
            &full_pull(7, "open", true, &["bob", "carol"]),
            "approved".into(),
            Some("success".into()),
        );
        assert_eq!(pr.number, 7);
        assert_eq!(pr.review, "approved");
        assert_eq!(pr.checks.as_deref(), Some("success"));
        assert_eq!(pr.mergeable, Some(true)); // open -> reported
        assert_eq!(pr.requested_reviewers, vec!["bob", "carol"]);
    }

    #[test]
    fn build_entry_pr_hides_mergeable_when_not_open() {
        let pr = build_entry_pr(&full_pull(7, "merged", false, &[]), "none".into(), None);
        assert_eq!(pr.mergeable, None); // not open -> not applicable
    }

    #[test]
    fn entry_pr_number_prefers_open_then_stored() {
        let open = vec![pull_with_head(7, "feature/a", "open")];
        let mut items = stack_with_branches(&["feature/a", "feature/b"]).items;
        // feature/a matches an open PR
        assert_eq!(entry_pr_number(&items[0], &open), Some(7));
        // feature/b has no open PR; falls back to a stored pr number
        items[1].pr = Some(ItemPr {
            number: 99,
            url: "u".into(),
            state: "merged".into(),
        });
        assert_eq!(entry_pr_number(&items[1], &open), Some(99));
        items[1].pr = None;
        assert_eq!(entry_pr_number(&items[1], &open), None);
    }

    #[test]
    fn render_review_shows_enriched_fields_and_missing_pr() {
        let review = StackReview {
            name: "feat".into(),
            base: "main".into(),
            entries: vec![
                StackEntryView {
                    branch: "feature/a".into(),
                    title: "A".into(),
                    pull_request: Some(EntryPr {
                        number: 7,
                        title: "Add A".into(),
                        state: "open".into(),
                        draft: false,
                        url: "https://x/pulls/7".into(),
                        review: "approved".into(),
                        checks: Some("success".into()),
                        mergeable: Some(true),
                        requested_reviewers: vec!["bob".into()],
                    }),
                },
                StackEntryView {
                    branch: "feature/b".into(),
                    title: "B".into(),
                    pull_request: None,
                },
            ],
        };
        let out = render_review(&review);
        assert!(out.contains("feature/a  #7 Add A [open]  review:approved  ci:success  mergeable"));
        assert!(out.contains("requested: bob"));
        assert!(out.contains("feature/b  (no PR)"));

        let v = serde_json::to_value(&review).unwrap();
        assert_eq!(v["entries"][0]["pull_request"]["review"], "approved");
        assert_eq!(v["entries"][0]["pull_request"]["checks"], "success");
        assert_eq!(v["entries"][0]["pull_request"]["mergeable"], true);
        assert_eq!(
            v["entries"][0]["pull_request"]["requested_reviewers"][0],
            "bob"
        );
        assert!(v["entries"][1]["pull_request"].is_null());
    }

    #[tokio::test]
    async fn enrich_pr_gathers_full_reviews_and_checks() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::to_value(full_pull(7, "open", true, &["bob"])).unwrap(),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([review_value("APPROVED")])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/commits/aaa/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "state": "success", "sha": "aaa", "total_count": 2, "statuses": []
            })))
            .mount(&server)
            .await;
        let c = client_for(&server);
        let pr = enrich_pr(&c, "o", "r", 7).await.expect("should enrich");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.review, "approved");
        assert_eq!(pr.checks.as_deref(), Some("success"));
        assert_eq!(pr.mergeable, Some(true));
        assert_eq!(pr.requested_reviewers, vec!["bob"]);
    }

    #[tokio::test]
    async fn enrich_pr_returns_none_when_pr_missing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/9"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let c = client_for(&server);
        assert!(enrich_pr(&c, "o", "r", 9).await.is_none());
    }

    // ---- sync: pure helpers ----

    #[test]
    fn sync_parses_flags_and_draft_conflicts_ready() {
        let cli = Cli::try_parse_from(["fj", "stack", "sync", "--draft", "--json", "--dry-run"])
            .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Sync(a),
            }) => {
                assert!(a.draft && a.json && a.dry_run && !a.ready);
            }
            other => panic!("expected stack sync, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["fj", "stack", "sync", "--draft", "--ready"]).is_err());
    }

    #[test]
    fn pr_base_chains_items() {
        let items = stack_with_branches(&["a", "b", "c"]).items;
        assert_eq!(pr_base("main", &items, 0), "main");
        assert_eq!(pr_base("main", &items, 1), "a");
        assert_eq!(pr_base("main", &items, 2), "b");
    }

    #[test]
    fn validate_items_rejects_empty_and_duplicates() {
        assert!(validate_items(&[]).is_err());
        assert!(validate_items(&stack_with_branches(&["a", "b"]).items).is_ok());
        assert!(validate_items(&stack_with_branches(&["a", "a"]).items).is_err());
    }

    #[test]
    fn plan_sync_uses_chained_bases() {
        let plan = plan_sync(&stack_with_branches(&["a", "b"]));
        assert_eq!(plan[0].base, "main");
        assert_eq!(plan[1].base, "a");
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v[1]["branch"], "b");
        assert_eq!(v[1]["base"], "a");
    }

    #[test]
    fn render_ops_lists_actions() {
        let ops = vec![SyncOp {
            branch: "feature/a".into(),
            base: "main".into(),
            action: "created".into(),
            pr: ItemPr {
                number: 7,
                url: "https://x/pulls/7".into(),
                state: "open".into(),
            },
        }];
        let out = render_ops(&ops);
        assert!(out.contains("created feature/a → main  (#7 https://x/pulls/7)"));
        assert!(out.contains("Synced 1 item(s)."));
    }

    // ---- sync: mocked Forgejo API (ensure_pr) ----

    fn pull_value(number: u64, head: &str, state: &str, draft: bool) -> serde_json::Value {
        serde_json::json!({
            "id": number, "number": number, "title": "t", "state": state,
            "user": { "id": 1, "login": "alice" },
            "html_url": format!("https://x/pulls/{number}"),
            "head": { "ref": head, "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "draft": draft,
            "created_at": "2026-06-01T00:00:00Z",
            "updated_at": "2026-06-01T00:00:00Z"
        })
    }

    fn client_for(server: &wiremock::MockServer) -> Client {
        Client::for_base_url(
            format!("{}/api/v1/", server.uri()).parse().unwrap(),
            "t".into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn ensure_pr_creates_when_missing() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .and(body_json(serde_json::json!({
                "title": "Add login", "head": "stephen/login-1", "base": "main", "draft": false
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(pull_value(
                12,
                "stephen/login-1",
                "open",
                false,
            )))
            .mount(&server)
            .await;
        let c = client_for(&server);
        let (action, pr) = ensure_pr(
            &c,
            "o",
            "r",
            "stephen/login-1",
            "main",
            "Add login",
            PrMode {
                draft: false,
                ready: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(action, "created");
        assert_eq!(pr.number, 12);
        assert_eq!(pr.state, "open");
    }

    #[tokio::test]
    async fn ensure_pr_updates_existing_idempotently() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // The branch already has a PR -> no POST is mounted, so a create would 404.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                pull_value(7, "stephen/login-1", "open", false)
            ])))
            .mount(&server)
            .await;
        let c = client_for(&server);
        let (action, pr) = ensure_pr(
            &c,
            "o",
            "r",
            "stephen/login-1",
            "main",
            "Add login",
            PrMode {
                draft: false,
                ready: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(action, "updated");
        assert_eq!(pr.number, 7);
    }

    #[tokio::test]
    async fn ensure_pr_creates_draft_with_draft_flag() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .and(body_json(serde_json::json!({
                "title": "t", "head": "b", "base": "main", "draft": true
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(pull_value(3, "b", "open", true)),
            )
            .mount(&server)
            .await;
        let c = client_for(&server);
        let (action, _pr) = ensure_pr(
            &c,
            "o",
            "r",
            "b",
            "main",
            "t",
            PrMode {
                draft: true,
                ready: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(action, "created");
    }

    #[tokio::test]
    async fn ensure_pr_marks_draft_ready_with_ready_flag() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([pull_value(7, "b", "open", true)])),
            )
            .mount(&server)
            .await;
        // ready = PATCH the pull with {draft:false}
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pull_value(7, "b", "open", false)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let c = client_for(&server);
        let (action, pr) = ensure_pr(
            &c,
            "o",
            "r",
            "b",
            "main",
            "t",
            PrMode {
                draft: false,
                ready: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(action, "updated");
        assert_eq!(pr.state, "open");
    }
}
