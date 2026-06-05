//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Stack state is local and git-native (`<git-dir>/fj/stack.json`, see
//! [`stack_state`](super::stack_state)) — no server state, offline-capable,
//! never a committed manifest. `stack new` (author a stack), `stack review`
//! (show the stack + each branch's PR state), `stack sync` (push each branch
//! and create/update its Forgejo PR, chaining bases so each PR targets the
//! previous branch, and mirroring a navigation block into each PR body unless
//! `--no-annotate`), and `stack ship` (merge bottom-up, re-pointing each next
//! PR onto the trunk as the one below it merges) are implemented, as is
//! `stack split` (turn a single branch's commits into one stacked entry each,
//! moving only local refs) and `stack absorb` (fold staged changes into the
//! entries that introduced them, via `fixup!` commits + an autosquash
//! `--update-refs` rebase). All five are implemented.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::api;
use crate::api::pull::{CreatePull, EditPull, MergeStyle};
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
    /// Don't write the stack-navigation block into each PR description.
    #[arg(long)]
    pub no_annotate: bool,
    /// Print intended actions without pushing or writing to Forgejo.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SplitArgs {
    // No `-R`: split rewrites local branches; it needs no repo API.
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit the planned/created entries as JSON.
    #[arg(long)]
    pub json: bool,
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
    // No `-R`: absorb rewrites local branches; it needs no repo API.
    /// Print the attribution plan and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit the attribution plan as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShipArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Squash-merge each PR.
    #[arg(long, conflicts_with = "merge")]
    pub squash: bool,
    /// Merge-commit each PR.
    #[arg(long)]
    pub merge: bool,
    /// Delete each branch (remote + local) after its PR merges.
    #[arg(long)]
    pub delete_branches: bool,
    /// Ship even if a PR isn't green/approved.
    #[arg(long)]
    pub force: bool,
    /// Emit machine-readable operations/results as JSON.
    #[arg(long)]
    pub json: bool,
    /// Print the merge plan without merging or deleting anything.
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
        shipped_at: None,
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

/// Markers bracketing the fj-managed navigation block in a PR body. Content
/// outside them is human-authored and always preserved; content inside is
/// regenerated on every sync.
const STACK_BLOCK_START: &str = "<!-- fj-stack:start -->";
const STACK_BLOCK_END: &str = "<!-- fj-stack:end -->";

/// Render the navigation block (without markers) for the PR of item `current`:
/// the whole stack, bottom-up, each line linking the item's PR (or naming its
/// branch when it has none yet) and marking the current entry.
fn render_stack_block(stack: &Stack, current: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "### Stack: {}", stack.name);
    let _ = writeln!(s);
    for (i, item) in stack.items.iter().enumerate() {
        let label = match &item.pr {
            Some(pr) => format!("#{}", pr.number),
            None => format!("`{}`", item.branch),
        };
        let here = if i == current { " ← this PR" } else { "" };
        let _ = writeln!(s, "{}. {} {}{}", i + 1, label, item.title, here);
    }
    let _ = writeln!(s);
    let _ = write!(
        s,
        "<sub>Maintained by `fj stack sync`; edits inside this block are overwritten.</sub>"
    );
    s
}

/// Splice the navigation block into `existing`, replacing a prior fj block when
/// present and otherwise appending it. Everything outside the markers is kept
/// verbatim, so a hand-written PR description survives re-syncs.
fn apply_stack_block(existing: &str, block: &str) -> String {
    let wrapped = format!("{STACK_BLOCK_START}\n{block}\n{STACK_BLOCK_END}");
    if let (Some(start), Some(end)) = (
        existing.find(STACK_BLOCK_START),
        existing.find(STACK_BLOCK_END),
    ) {
        if start < end {
            let after = &existing[end + STACK_BLOCK_END.len()..];
            return format!("{}{}{}", &existing[..start], wrapped, after);
        }
    }
    if existing.trim().is_empty() {
        wrapped
    } else {
        format!("{}\n\n{}", existing.trim_end(), wrapped)
    }
}

/// Mirror the stack navigation into every item's PR body, preserving any
/// human-authored content. Lists the PRs once for their current bodies, then
/// PATCHes only those whose body actually changes. Returns how many were
/// updated. Items without a PR number are skipped.
async fn annotate_prs(client: &Client, owner: &str, name: &str, stack: &Stack) -> Result<usize> {
    let page = api::pull::list(client, owner, name, api::pull::ListOptions::default()).await?;
    let mut annotated = 0usize;
    for (i, item) in stack.items.iter().enumerate() {
        let Some(pr) = &item.pr else {
            continue;
        };
        let existing = page
            .items
            .iter()
            .find(|p| p.number == pr.number)
            .map(|p| p.body.clone())
            .unwrap_or_default();
        let new_body = apply_stack_block(&existing, &render_stack_block(stack, i));
        if new_body != existing {
            api::pull::edit(
                client,
                owner,
                name,
                pr.number,
                &EditPull {
                    body: Some(&new_body),
                    ..Default::default()
                },
            )
            .await?;
            annotated += 1;
        }
    }
    Ok(annotated)
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
        if !args.no_annotate {
            println!(
                "Would annotate {} PR description(s) with the stack.",
                plan.len()
            );
        }
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

    // Second pass: now that every item has a PR number, mirror the stack
    // navigation into each PR body. Convenience annotation, never the source of
    // truth; human-authored content is preserved. Skipped with `--no-annotate`.
    let annotated = if args.no_annotate {
        0
    } else {
        annotate_prs(&ctx.client, &ctx.owner, &ctx.name, &stack).await?
    };

    if args.json {
        return output::print_json(&serde_json::to_value(&ops)?);
    }
    print!("{}", render_ops(&ops));
    if annotated > 0 {
        println!("Annotated {annotated} PR description(s) with the stack.");
    }
    Ok(())
}

// ── stack split ──────────────────────────────────────────────────────────
//
// Turn a single-branch stack into one stacked entry per commit. The N commits
// ahead of the stack base become branches `handle/slug-1..N`, each chained on
// the one below, bottom-up. It only moves local refs (no force-push, no API),
// so it's offline and reversible: the original commits stay reachable.

/// One commit ahead of the base: its full sha and subject line.
#[derive(Debug, PartialEq)]
struct Commit {
    sha: String,
    subject: String,
}

/// Parse `git log --format=%H\x1f%s` output (oldest first) into commits.
/// Blank lines and sha-less rows are skipped.
fn parse_commits(raw: &str) -> Vec<Commit> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\x1f');
            let sha = parts.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                sha: sha.to_string(),
                subject: parts.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

/// A split needs at least two commits; one (or zero) is a no-op.
fn validate_split(commits: &[Commit]) -> Result<()> {
    match commits.len() {
        0 => bail!("no commits between the stack base and HEAD to split"),
        1 => bail!("only one commit ahead of the base; nothing to split"),
        _ => Ok(()),
    }
}

/// Split only operates on a pristine single-entry stack, checked out on its
/// branch — so the commit→branch mapping is unambiguous and nothing is lost.
fn ensure_splittable(items: &[StackItem], current: &str) -> Result<()> {
    match items.len() {
        0 => bail!("stack has no branch yet (author one with `fj stack new`)"),
        1 => {}
        n => bail!("split expects a single-entry stack; this one already has {n} entries"),
    }
    if items[0].branch != current {
        bail!(
            "checkout the stack branch '{}' before splitting (currently on '{}')",
            items[0].branch,
            current
        );
    }
    Ok(())
}

/// Derive the branch-prefix handle from the stack's first item (`handle/slug-1`
/// → `handle`). `None` when there's no item or no `/` to split on.
fn handle_from_items(items: &[StackItem]) -> Option<String> {
    items
        .first()
        .and_then(|it| it.branch.split_once('/'))
        .map(|(prefix, _)| prefix)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// Resolve the handle for split: the existing item's prefix, else the local
/// git user (offline), else `user`.
fn stack_handle(stack: &Stack) -> String {
    if let Some(h) = handle_from_items(&stack.items) {
        return h;
    }
    let git_user = git::output(&["config", "user.name"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    stack_state::safe_handle(None, git_user.as_deref())
}

/// Build the new stacked items from the commit list. Item `k` (1-based) lives
/// on `handle/slug-k`, bases on the one below (the stack base for the first),
/// and takes its title from the commit subject (falling back to the branch).
fn plan_split(base: &str, handle: &str, slug: &str, commits: &[Commit]) -> Vec<StackItem> {
    commits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let n = i + 1;
            let branch = stack_state::item_branch(handle, slug, n);
            let item_base = if i == 0 {
                base.to_string()
            } else {
                stack_state::item_branch(handle, slug, i)
            };
            let title = if c.subject.is_empty() {
                format!("{slug}-{n}")
            } else {
                c.subject.clone()
            };
            StackItem {
                id: format!("{slug}-{n}"),
                title,
                branch,
                base: item_base,
                pr: None,
                status: "local".to_string(),
            }
        })
        .collect()
}

/// Human dry-run render. Zips items with their source commits to show the
/// short sha each branch will point at.
fn render_split_plan(items: &[StackItem], commits: &[Commit]) -> String {
    use std::fmt::Write;
    let mut s = format!(
        "Would split into {} stacked entries (no pushes, local refs only):\n",
        items.len()
    );
    for (item, c) in items.iter().zip(commits) {
        let short: String = c.sha.chars().take(8).collect();
        let _ = writeln!(
            s,
            "  {} ← {}  {}  (base: {})",
            item.branch, short, item.title, item.base
        );
    }
    s
}

/// Confirmation render after the refs are moved.
fn render_split_done(items: &[StackItem]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for item in items {
        let _ = writeln!(s, "✓ {} → {}  {}", item.branch, item.base, item.title);
    }
    let _ = writeln!(s, "Split into {} entries. Next: fj stack sync", items.len());
    s
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn split(args: SplitArgs, _host: Option<&str>) -> Result<()> {
    let Some(mut stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };

    // Refuse on a dirty tree: split moves the checked-out branch, and a stray
    // uncommitted change could be left stranded on a now-unreferenced state.
    if let Some((dirty, n)) = git::working_tree_status() {
        if dirty {
            bail!("working tree has {n} uncommitted change(s); commit or stash before splitting");
        }
    }
    let current = git::current_branch()
        .ok_or_else(|| anyhow::anyhow!("HEAD is detached; checkout the stack branch first"))?;
    ensure_splittable(&stack.items, &current)?;

    let handle = stack_handle(&stack);
    let commits = parse_commits(&git::commits_since(&stack.base)?);
    validate_split(&commits)?;
    let items = plan_split(&stack.base, &handle, &stack.slug, &commits);

    if args.dry_run {
        if args.json {
            return output::print_json(&serde_json::to_value(&items)?);
        }
        print!("{}", render_split_plan(&items, &commits));
        return Ok(());
    }

    // Detach first so the currently-checked-out branch can be re-pointed, then
    // lay down each entry's ref at its commit, and land on the top branch.
    git::checkout_detach()?;
    for (item, c) in items.iter().zip(&commits) {
        git::set_branch(&item.branch, &c.sha)?;
    }
    if let Some(top) = items.last() {
        git::checkout(&top.branch)?;
    }

    stack.items = items.clone();
    stack.updated_at = Utc::now().to_rfc3339();
    stack_state::save(&stack)?;

    if args.json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    print!("{}", render_split_done(&items));
    Ok(())
}

// ── stack absorb ─────────────────────────────────────────────────────────
//
// Fold staged changes into the stack entries that introduced the lines, the
// way `git absorb` does. Attribution is per-file (the most recent stack commit
// to touch a file wins); each target gets a `fixup!` commit, then a single
// `git rebase --autosquash --update-refs` rewrites the stack in place and moves
// every branch ref. Offline (no API) but it rewrites history, so the branches
// need a force-push afterward.

/// Parse a major/minor from `git --version` output. None when it doesn't look
/// like "git version X.Y…".
fn parse_git_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.split("version").nth(1)?;
    let nums = rest.trim();
    let mut parts = nums.split(['.', ' ', '\n', '\t']).filter(|p| !p.is_empty());
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// `rebase --update-refs` landed in git 2.38; absorb relies on it (plus
/// `--autosquash`) to move the intermediate branch refs during the rewrite.
fn supports_update_refs(version: (u32, u32)) -> bool {
    version >= (2, 38)
}

/// Map every commit sha in the stack to the entry (0-based) it belongs to.
/// `entries[i]` is the list of shas in item `i`'s range (`prev..branch`).
/// Later entries win a tie (a sha should only appear once in a linear stack).
fn index_commits(entries: &[Vec<String>]) -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    for (i, shas) in entries.iter().enumerate() {
        for sha in shas {
            map.insert(sha.clone(), i);
        }
    }
    map
}

/// A staged file routed to a stack entry, or left unattributed.
#[derive(Debug, PartialEq)]
struct FileAttribution {
    file: String,
    /// `Some(entry_index)` when the file's last-touching commit is in the stack.
    entry: Option<usize>,
}

/// Attribute each `(file, last_touching_sha)` to a stack entry via the
/// sha→entry index. A file whose last touch is outside the stack (e.g. from the
/// base branch, or brand new) is left unattributed.
fn attribute_files(
    files: &[(String, Option<String>)],
    sha_to_entry: &std::collections::HashMap<String, usize>,
) -> Vec<FileAttribution> {
    files
        .iter()
        .map(|(file, sha)| FileAttribution {
            file: file.clone(),
            entry: sha.as_ref().and_then(|s| sha_to_entry.get(s).copied()),
        })
        .collect()
}

/// One entry's absorb group: the target commit to fix up, the entry's branch,
/// and the files headed for it. Serializes for `--json` / dry-run.
#[derive(Debug, Serialize, PartialEq)]
struct AbsorbGroup {
    branch: String,
    /// The commit (a stack entry's tip) the fixup targets.
    target: String,
    files: Vec<String>,
}

/// The full absorb plan: per-entry groups plus the files left staged.
#[derive(Debug, Serialize, PartialEq)]
struct AbsorbPlan {
    groups: Vec<AbsorbGroup>,
    /// Files that couldn't be attributed; they stay staged, untouched.
    skipped: Vec<String>,
}

/// Build the plan from attributions. `tips[i]` is item `i`'s target commit sha
/// (its branch tip); `branches[i]` its branch name. Groups preserve entry order
/// (bottom-up) and are dropped when they have no files.
fn plan_absorb(
    attributions: &[FileAttribution],
    branches: &[String],
    tips: &[String],
) -> AbsorbPlan {
    let mut groups: Vec<AbsorbGroup> = branches
        .iter()
        .zip(tips)
        .map(|(branch, target)| AbsorbGroup {
            branch: branch.clone(),
            target: target.clone(),
            files: Vec::new(),
        })
        .collect();
    let mut skipped = Vec::new();
    for a in attributions {
        match a.entry {
            Some(i) if i < groups.len() => groups[i].files.push(a.file.clone()),
            _ => skipped.push(a.file.clone()),
        }
    }
    groups.retain(|g| !g.files.is_empty());
    AbsorbPlan { groups, skipped }
}

fn render_absorb_plan(plan: &AbsorbPlan) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if plan.groups.is_empty() {
        s.push_str("Nothing to absorb: no staged file maps to a stack entry.\n");
    } else {
        let _ = writeln!(s, "Would absorb staged changes into {}:", {
            let n = plan.groups.len();
            if n == 1 {
                "1 entry".to_string()
            } else {
                format!("{n} entries")
            }
        });
        for g in &plan.groups {
            let short: String = g.target.chars().take(8).collect();
            let _ = writeln!(s, "  {} ({}):", g.branch, short);
            for f in &g.files {
                let _ = writeln!(s, "      {f}");
            }
        }
    }
    if !plan.skipped.is_empty() {
        let _ = writeln!(s, "Left staged (no matching entry):");
        for f in &plan.skipped {
            let _ = writeln!(s, "      {f}");
        }
    }
    s
}

fn render_absorb_done(plan: &AbsorbPlan) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for g in &plan.groups {
        let _ = writeln!(s, "✓ absorbed {} file(s) into {}", g.files.len(), g.branch);
    }
    let _ = writeln!(
        s,
        "Rewrote {} entr{}. Branches now need a force-push: \
         git push --force-with-lease (per branch) or re-run after.",
        plan.groups.len(),
        if plan.groups.len() == 1 { "y" } else { "ies" }
    );
    if !plan.skipped.is_empty() {
        let _ = writeln!(
            s,
            "{} file(s) left staged (no matching entry).",
            plan.skipped.len()
        );
    }
    s
}

/// Resolve, per staged file, the most recent stack commit (`base..top`) that
/// touched it. `None` means no stack commit touched it (base-owned / new).
fn last_touch_in_stack(base: &str, top: &str, file: &str) -> Option<String> {
    let range = format!("{base}..{top}");
    let out = git::output(&["log", "-1", "--format=%H", &range, "--", file]).ok()?;
    let sha = out.trim();
    (!sha.is_empty()).then(|| sha.to_string())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn absorb(args: AbsorbArgs, _host: Option<&str>) -> Result<()> {
    let Some(mut stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    validate_items(&stack.items)?;

    let version = parse_git_version(&git::version_string()?)
        .ok_or_else(|| anyhow::anyhow!("could not determine the git version"))?;
    if !supports_update_refs(version) {
        bail!(
            "stack absorb needs git >= 2.38 (for rebase --update-refs); found {}.{}",
            version.0,
            version.1
        );
    }

    // The whole stack must be checked out at its top, and linear, so a single
    // autosquash rebase rebuilds every entry and --update-refs moves the refs.
    let current = git::current_branch().ok_or_else(|| {
        anyhow::anyhow!("HEAD is detached; checkout the stack's top branch first")
    })?;
    let top_branch = stack
        .items
        .last()
        .expect("validated non-empty")
        .branch
        .clone();
    if current != top_branch {
        bail!("checkout the top branch '{top_branch}' before absorbing (currently on '{current}')");
    }
    for pair in stack.items.windows(2) {
        if !git::is_ancestor(&pair[0].branch, &pair[1].branch) {
            bail!(
                "stack is not linear: '{}' is not an ancestor of '{}'. Run `fj stack sync` first.",
                pair[0].branch,
                pair[1].branch
            );
        }
    }

    // Only staged changes are absorbed (deterministic, like `git add` then
    // `git absorb --and-rebase`). Unstaged/untracked are ignored.
    let staged_raw = git::output(&["diff", "--cached", "--name-only"])?;
    let staged: Vec<String> = staged_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if staged.is_empty() {
        bail!(
            "nothing staged to absorb (stage changes with `git add`); unstaged changes are ignored"
        );
    }

    // Per-entry commit shas (for sha→entry indexing) and each entry's tip.
    let mut entry_shas: Vec<Vec<String>> = Vec::with_capacity(stack.items.len());
    let mut tips: Vec<String> = Vec::with_capacity(stack.items.len());
    let mut branches: Vec<String> = Vec::with_capacity(stack.items.len());
    for (i, item) in stack.items.iter().enumerate() {
        let prev = if i == 0 {
            stack.base.clone()
        } else {
            stack.items[i - 1].branch.clone()
        };
        let range = format!("{prev}..{}", item.branch);
        let shas: Vec<String> = git::output(&["rev-list", &range])?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        entry_shas.push(shas);
        tips.push(git::rev_parse(&item.branch)?);
        branches.push(item.branch.clone());
    }

    let sha_to_entry = index_commits(&entry_shas);
    let files_with_touch: Vec<(String, Option<String>)> = staged
        .iter()
        .map(|f| (f.clone(), last_touch_in_stack(&stack.base, &top_branch, f)))
        .collect();
    let attributions = attribute_files(&files_with_touch, &sha_to_entry);
    let plan = plan_absorb(&attributions, &branches, &tips);

    if args.dry_run {
        if args.json {
            return output::print_json(&serde_json::to_value(&plan)?);
        }
        print!("{}", render_absorb_plan(&plan));
        return Ok(());
    }

    if plan.groups.is_empty() {
        // Nothing maps to an entry: don't rewrite anything.
        print!("{}", render_absorb_plan(&plan));
        return Ok(());
    }

    // Snapshot for recovery: the top tip (to undo fixups) and the staged patch.
    let orig_top = git::rev_parse("HEAD")?;

    // One fixup commit per target entry, peeling its files off the index.
    for g in &plan.groups {
        git::commit_fixup(&g.target, &g.files)?;
    }

    // Rebuild the stack; on failure, abort and re-stage the fixup contents so
    // the user's working state is preserved.
    if let Err(e) = git::rebase_autosquash_update_refs(&stack.base) {
        git::rebase_abort();
        let _ = git::reset_soft(&orig_top);
        return Err(e.context(
            "absorb rebase hit a conflict and was rolled back; resolve by hand \
             (git rebase -i --autosquash --update-refs) or stage a smaller change",
        ));
    }

    stack.updated_at = Utc::now().to_rfc3339();
    stack_state::save(&stack)?;

    if args.json {
        return output::print_json(&serde_json::to_value(&plan)?);
    }
    print!("{}", render_absorb_done(&plan));
    Ok(())
}

// ── stack ship ─────────────────────────────────────────────────────────────

/// The merge style to use. `--squash`/`--merge` are explicit; the default is
/// a plain merge commit, the most universally-allowed (safest) style.
fn merge_style(squash: bool, merge: bool) -> MergeStyle {
    match (squash, merge) {
        (true, _) => MergeStyle::Squash,
        _ => MergeStyle::Merge,
    }
}

/// Whether a PR is shippable: approved and green. "Green" treats no CI
/// (`none`/absent) as non-blocking; only pending/failure/error block.
/// Already-merged PRs are shippable (they're skipped). Returns the blocking
/// reason on failure.
fn shippable(review: &str, checks: Option<&str>, state: &str) -> std::result::Result<(), String> {
    if state == "merged" {
        return Ok(());
    }
    if review != "approved" {
        return Err(format!("not approved (review: {review})"));
    }
    match checks {
        Some(s) if s != "success" && s != "none" => Err(format!("checks not green (ci: {s})")),
        _ => Ok(()),
    }
}

/// What `ship` intends to do per item, bottom-up. For `--dry-run` / `--json`.
#[derive(Debug, Serialize, PartialEq)]
struct ShipPlan {
    branch: String,
    base: String,
    style: String,
}

fn plan_ship(stack: &Stack, style: MergeStyle) -> Vec<ShipPlan> {
    let bases: Vec<String> = (0..stack.items.len())
        .map(|i| pr_base(&stack.base, &stack.items, i))
        .collect();
    stack
        .items
        .iter()
        .zip(bases)
        .map(|(item, base)| ShipPlan {
            branch: item.branch.clone(),
            base,
            style: style.as_str().to_string(),
        })
        .collect()
}

fn render_ship_plan(plan: &[ShipPlan]) -> String {
    use std::fmt::Write;
    let mut s = String::from("Would ship bottom-up (no merges or deletes):\n");
    for p in plan {
        let _ = writeln!(s, "  merge {} → {} ({})", p.branch, p.base, p.style);
    }
    s
}

/// Recovery guidance printed when a merge fails partway through.
fn recovery_message(failed_branch: &str, merged: &[String]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "Stopped: merging '{failed_branch}' failed.");
    if merged.is_empty() {
        let _ = writeln!(s, "  Nothing was merged.");
    } else {
        let _ = writeln!(s, "  Already merged: {}", merged.join(", "));
    }
    let _ = writeln!(
        s,
        "  Fix the issue, then re-run `fj stack ship` — merged items are skipped."
    );
    s
}

#[derive(Debug, Serialize)]
struct ShipOp {
    branch: String,
    number: u64,
    /// "merged" | "skipped"
    action: String,
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn ship(args: ShipArgs, host: Option<&str>) -> Result<()> {
    let Some(mut stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    validate_items(&stack.items)?;
    let style = merge_style(args.squash, args.merge);

    if args.dry_run {
        let plan = plan_ship(&stack, style);
        if args.json {
            return output::print_json(&serde_json::to_value(&plan)?);
        }
        print!("{}", render_ship_plan(&plan));
        return Ok(());
    }

    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let page = api::pull::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        api::pull::ListOptions::default(),
    )
    .await?;
    // Once an item merges, everything above it should target the trunk (its
    // own base branch has merged away), so re-point the next PR onto `trunk`.
    let trunk = stack.base.clone();

    // Resolve each item's PR number; every item must have one to ship.
    let mut numbers = Vec::with_capacity(stack.items.len());
    for item in &stack.items {
        match entry_pr_number(item, &page.items) {
            Some(n) => numbers.push(n),
            None => bail!(
                "stack item '{}' has no PR — run `fj stack sync` first",
                item.branch
            ),
        }
    }

    // Pre-flight: validate ALL PRs before merging any (unless --force), so we
    // never merge the bottom and then stop.
    if !args.force {
        for (item, &number) in stack.items.iter().zip(&numbers) {
            let pr = enrich_pr(&ctx.client, &ctx.owner, &ctx.name, number)
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!("could not fetch PR #{number} for '{}'", item.branch)
                })?;
            if let Err(reason) = shippable(&pr.review, pr.checks.as_deref(), &pr.state) {
                bail!(
                    "'{}' (PR #{number}) is not ready: {reason}. Pass --force to ship anyway.",
                    item.branch
                );
            }
        }
    }

    // Merge bottom-up; stop immediately on failure with recovery guidance.
    let mut ops = Vec::with_capacity(stack.items.len());
    let mut merged: Vec<String> = Vec::new();
    for i in 0..stack.items.len() {
        let branch = stack.items[i].branch.clone();
        let number = numbers[i];
        let already_merged = stack.items[i]
            .pr
            .as_ref()
            .map(|p| p.state == "merged")
            .unwrap_or(false);

        if !already_merged {
            if let Err(e) = api::pull::merge(
                &ctx.client,
                &ctx.owner,
                &ctx.name,
                number,
                style,
                &api::pull::MergeOptions::default(),
            )
            .await
            {
                eprint!("{}", recovery_message(&branch, &merged));
                return Err(e.context(format!("merging PR #{number} ({branch})")));
            }
            // Re-point the next PR onto the trunk this one merged into.
            if let Some(next) = numbers.get(i + 1) {
                let _ = api::pull::edit(
                    &ctx.client,
                    &ctx.owner,
                    &ctx.name,
                    *next,
                    &EditPull {
                        base: Some(&trunk),
                        ..Default::default()
                    },
                )
                .await;
            }
            if args.delete_branches {
                let _ = git::delete_remote_branch(&stack.repo.remote, &branch);
                let _ = git::delete_local_branch(&branch);
            }
            ops.push(ShipOp {
                branch: branch.clone(),
                number,
                action: "merged".to_string(),
            });
        } else {
            ops.push(ShipOp {
                branch: branch.clone(),
                number,
                action: "skipped".to_string(),
            });
        }

        if let Some(pr) = stack.items[i].pr.as_mut() {
            pr.state = "merged".to_string();
        }
        stack.items[i].status = "merged".to_string();
        merged.push(branch);
        stack.updated_at = Utc::now().to_rfc3339();
        stack_state::save(&stack)?;
    }

    stack.shipped_at = Some(Utc::now().to_rfc3339());
    stack_state::save(&stack)?;

    if args.json {
        return output::print_json(&serde_json::to_value(&ops)?);
    }
    use std::fmt::Write as _;
    let mut out = String::new();
    for op in &ops {
        let _ = writeln!(out, "✓ {} {} (#{})", op.action, op.branch, op.number);
    }
    let _ = writeln!(out, "Shipped {} item(s).", ops.len());
    print!("{out}");
    Ok(())
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
            shipped_at: None,
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

    // ---- ship ----

    #[test]
    fn ship_parses_flags_and_squash_conflicts_merge() {
        let cli = Cli::try_parse_from([
            "fj",
            "stack",
            "ship",
            "--squash",
            "--delete-branches",
            "--force",
            "--json",
            "--dry-run",
        ])
        .expect("should parse");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Ship(a),
            }) => {
                assert!(a.squash && a.delete_branches && a.force && a.json && a.dry_run);
                assert!(!a.merge);
            }
            other => panic!("expected stack ship, got {other:?}"),
        }
        // --squash and --merge are mutually exclusive.
        assert!(Cli::try_parse_from(["fj", "stack", "ship", "--squash", "--merge"]).is_err());
    }

    #[test]
    fn merge_style_defaults_to_merge_commit() {
        assert!(matches!(merge_style(false, false), MergeStyle::Merge));
        assert!(matches!(merge_style(false, true), MergeStyle::Merge));
        assert!(matches!(merge_style(true, false), MergeStyle::Squash));
    }

    #[test]
    fn shippable_requires_approved_and_green() {
        // approved + success
        assert!(shippable("approved", Some("success"), "open").is_ok());
        // approved + no CI configured is fine
        assert!(shippable("approved", Some("none"), "open").is_ok());
        assert!(shippable("approved", None, "open").is_ok());
        // already merged is always shippable (it'll be skipped)
        assert!(shippable("none", Some("failure"), "merged").is_ok());
        // not approved blocks
        let e = shippable("none", Some("success"), "open").unwrap_err();
        assert!(e.contains("not approved"));
        let e = shippable("changes_requested", Some("success"), "open").unwrap_err();
        assert!(e.contains("changes_requested"));
        // pending/failing CI blocks
        let e = shippable("approved", Some("pending"), "open").unwrap_err();
        assert!(e.contains("checks not green"));
        assert!(shippable("approved", Some("failure"), "open").is_err());
    }

    #[test]
    fn plan_ship_chains_bases_and_renders_dry_run() {
        let stack = stack_with_branches(&["feature/a", "feature/b", "feature/c"]);
        let plan = plan_ship(&stack, MergeStyle::Squash);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].base, "main"); // bottom targets trunk
        assert_eq!(plan[1].base, "feature/a"); // chained onto the one below
        assert_eq!(plan[2].base, "feature/b");
        assert!(plan.iter().all(|p| p.style == "squash"));

        let out = render_ship_plan(&plan);
        assert!(out.contains("Would ship bottom-up"));
        assert!(out.contains("merge feature/a → main (squash)"));
        assert!(out.contains("merge feature/b → feature/a (squash)"));

        // JSON shape is stable.
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v[0]["branch"], "feature/a");
        assert_eq!(v[0]["base"], "main");
        assert_eq!(v[2]["style"], "squash");
    }

    #[test]
    fn recovery_message_reflects_progress() {
        // nothing merged yet
        let m = recovery_message("feature/a", &[]);
        assert!(m.contains("merging 'feature/a' failed"));
        assert!(m.contains("Nothing was merged"));
        assert!(m.contains("merged items are skipped"));
        // some merged before the failure
        let m = recovery_message("feature/c", &["feature/a".into(), "feature/b".into()]);
        assert!(m.contains("Already merged: feature/a, feature/b"));
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

    // ---- sync: PR-body annotation ----

    #[test]
    fn sync_parses_no_annotate() {
        let cli =
            Cli::try_parse_from(["fj", "stack", "sync", "--no-annotate"]).expect("should parse");
        assert!(matches!(
            cli.command,
            Command::Stack(StackCmd {
                command: StackSub::Sync(SyncArgs {
                    no_annotate: true,
                    ..
                })
            })
        ));
    }

    fn stack_with_prs(branches: &[&str], numbers: &[u64]) -> Stack {
        let mut s = stack_with_branches(branches);
        for (item, n) in s.items.iter_mut().zip(numbers) {
            item.pr = Some(ItemPr {
                number: *n,
                url: format!("https://x/pulls/{n}"),
                state: "open".into(),
            });
        }
        s
    }

    #[test]
    fn render_stack_block_lists_entries_and_marks_current() {
        let stack = stack_with_prs(&["feature/a", "feature/b"], &[7, 8]);
        let block = render_stack_block(&stack, 1);
        assert!(block.contains("### Stack: feat"));
        assert!(block.contains("1. #7 Item 1"));
        assert!(block.contains("2. #8 Item 2 ← this PR"));
        assert!(block.contains("Maintained by `fj stack sync`"));
        // an item without a PR yet falls back to its branch name
        let mut partial = stack_with_prs(&["feature/a", "feature/b"], &[7]);
        partial.items[1].pr = None;
        let block = render_stack_block(&partial, 0);
        assert!(block.contains("2. `feature/b` Item 2"));
    }

    #[test]
    fn apply_stack_block_appends_when_absent() {
        let out = apply_stack_block("Human-written description.", "NAV");
        assert!(out.starts_with("Human-written description.\n\n"));
        assert!(out.contains(STACK_BLOCK_START));
        assert!(out.contains("\nNAV\n"));
        assert!(out.trim_end().ends_with(STACK_BLOCK_END));
        // empty existing body -> just the wrapped block
        let only = apply_stack_block("", "NAV");
        assert_eq!(only, format!("{STACK_BLOCK_START}\nNAV\n{STACK_BLOCK_END}"));
    }

    #[test]
    fn apply_stack_block_replaces_in_place_preserving_surroundings() {
        let existing = format!(
            "Top prose.\n\n{STACK_BLOCK_START}\nOLD NAV\n{STACK_BLOCK_END}\n\nBottom prose."
        );
        let out = apply_stack_block(&existing, "NEW NAV");
        assert!(out.contains("Top prose."));
        assert!(out.contains("Bottom prose."));
        assert!(out.contains("NEW NAV"));
        assert!(!out.contains("OLD NAV"));
        // exactly one block remains
        assert_eq!(out.matches(STACK_BLOCK_START).count(), 1);
        assert_eq!(out.matches(STACK_BLOCK_END).count(), 1);
    }

    #[test]
    fn apply_stack_block_is_idempotent() {
        let block = "NAV";
        let once = apply_stack_block("body", block);
        let twice = apply_stack_block(&once, block);
        assert_eq!(once, twice);
    }

    #[tokio::test]
    async fn annotate_prs_patches_only_changed_bodies() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let stack = stack_with_prs(&["feature/a", "feature/b"], &[7, 8]);

        // PR #7 has no fj block yet -> must be patched. PR #8 already carries the
        // exact block it would get -> must be left alone.
        let body8 = apply_stack_block("", &render_stack_block(&stack, 1));
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                pull_value_body(7, "feature/a", ""),
                pull_value_body(8, "feature/b", &body8),
            ])))
            .mount(&server)
            .await;
        // Only #7 should be PATCHed.
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull_value_body(
                7,
                "feature/a",
                "x",
            )))
            .expect(1)
            .mount(&server)
            .await;

        let c = client_for(&server);
        let n = annotate_prs(&c, "o", "r", &stack).await.unwrap();
        assert_eq!(n, 1);
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

    fn pull_value_body(number: u64, head: &str, body: &str) -> serde_json::Value {
        let mut v = pull_value(number, head, "open", false);
        v["body"] = serde_json::Value::String(body.to_string());
        v
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

    // ---- split ----

    #[test]
    fn split_parses_dry_run_and_json() {
        let cli =
            Cli::try_parse_from(["fj", "stack", "split", "--dry-run", "--json"]).expect("parses");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Split(a),
            }) => assert!(a.dry_run && a.json),
            other => panic!("expected stack split, got {other:?}"),
        }
    }

    #[test]
    fn parse_commits_skips_blank_and_subjectless_rows() {
        let raw = "aaa\u{1f}first commit\nbbb\u{1f}second\n\n  \nccc\u{1f}";
        let commits = parse_commits(raw);
        assert_eq!(commits.len(), 3);
        assert_eq!(commits[0].sha, "aaa");
        assert_eq!(commits[0].subject, "first commit");
        assert_eq!(commits[2].sha, "ccc");
        assert_eq!(commits[2].subject, ""); // subjectless still counts
    }

    fn commits(subjects: &[&str]) -> Vec<Commit> {
        subjects
            .iter()
            .enumerate()
            .map(|(i, s)| Commit {
                sha: format!("sha{}", i + 1),
                subject: s.to_string(),
            })
            .collect()
    }

    #[test]
    fn validate_split_requires_at_least_two() {
        assert!(validate_split(&commits(&[])).is_err());
        assert!(validate_split(&commits(&["one"])).is_err());
        assert!(validate_split(&commits(&["one", "two"])).is_ok());
    }

    #[test]
    fn ensure_splittable_guards_count_and_branch() {
        let one = stack_with_branches(&["u/feat-1"]).items;
        assert!(ensure_splittable(&one, "u/feat-1").is_ok());
        // wrong branch checked out
        assert!(ensure_splittable(&one, "other").is_err());
        // empty stack
        assert!(ensure_splittable(&[], "x").is_err());
        // already multi-entry
        let many = stack_with_branches(&["u/feat-1", "u/feat-2"]).items;
        assert!(ensure_splittable(&many, "u/feat-1").is_err());
    }

    #[test]
    fn handle_from_items_takes_branch_prefix() {
        let items = stack_with_branches(&["stephen/login-1"]).items;
        assert_eq!(handle_from_items(&items).as_deref(), Some("stephen"));
        // no slash -> none
        let items = stack_with_branches(&["nodash"]).items;
        assert_eq!(handle_from_items(&items), None);
        assert_eq!(handle_from_items(&[]), None);
    }

    #[test]
    fn plan_split_chains_bases_ids_and_titles() {
        let items = plan_split(
            "main",
            "stephen",
            "login",
            &commits(&["add form", "wire api"]),
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "login-1");
        assert_eq!(items[0].branch, "stephen/login-1");
        assert_eq!(items[0].base, "main"); // first targets the trunk
        assert_eq!(items[0].title, "add form");
        assert_eq!(items[0].status, "local");
        assert!(items[0].pr.is_none());
        assert_eq!(items[1].branch, "stephen/login-2");
        assert_eq!(items[1].base, "stephen/login-1"); // chained on the one below
        assert_eq!(items[1].title, "wire api");
    }

    #[test]
    fn plan_split_falls_back_to_branch_title_when_subject_empty() {
        let items = plan_split("main", "u", "feat", &commits(&["", "real"]));
        assert_eq!(items[0].title, "feat-1"); // empty subject -> id-ish title
        assert_eq!(items[1].title, "real");
    }

    #[test]
    fn render_split_plan_and_done_are_legible() {
        let cs = commits(&["add form", "wire api"]);
        let items = plan_split("main", "stephen", "login", &cs);
        let plan = render_split_plan(&items, &cs);
        assert!(plan.contains("Would split into 2 stacked entries"));
        assert!(plan.contains("stephen/login-1 ← sha1  add form  (base: main)"));
        assert!(plan.contains("stephen/login-2 ← sha2  wire api  (base: stephen/login-1)"));

        let done = render_split_done(&items);
        assert!(done.contains("✓ stephen/login-1 → main  add form"));
        assert!(done.contains("Split into 2 entries. Next: fj stack sync"));
    }

    #[test]
    fn split_items_json_shape_is_stable() {
        let items = plan_split("main", "u", "feat", &commits(&["a", "b"]));
        let v = serde_json::to_value(&items).unwrap();
        assert_eq!(v[0]["branch"], "u/feat-1");
        assert_eq!(v[0]["base"], "main");
        assert_eq!(v[1]["base"], "u/feat-1");
        assert_eq!(v[1]["title"], "b");
        assert!(v[0]["pr"].is_null());
    }

    // ---- absorb ----

    #[test]
    fn absorb_parses_dry_run_and_json() {
        let cli =
            Cli::try_parse_from(["fj", "stack", "absorb", "--dry-run", "--json"]).expect("parses");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Absorb(a),
            }) => assert!(a.dry_run && a.json),
            other => panic!("expected stack absorb, got {other:?}"),
        }
    }

    #[test]
    fn parse_git_version_reads_major_minor() {
        assert_eq!(parse_git_version("git version 2.54.0"), Some((2, 54)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-145)"),
            Some((2, 39))
        );
        assert_eq!(parse_git_version("git version 3"), Some((3, 0)));
        assert_eq!(parse_git_version("nonsense"), None);
    }

    #[test]
    fn supports_update_refs_gates_on_2_38() {
        assert!(!supports_update_refs((2, 37)));
        assert!(supports_update_refs((2, 38)));
        assert!(supports_update_refs((2, 54)));
        assert!(supports_update_refs((3, 0)));
    }

    #[test]
    fn index_commits_maps_sha_to_entry() {
        let entries = vec![
            vec!["a1".to_string(), "a2".to_string()],
            vec!["b1".to_string()],
        ];
        let map = index_commits(&entries);
        assert_eq!(map.get("a1"), Some(&0));
        assert_eq!(map.get("a2"), Some(&0));
        assert_eq!(map.get("b1"), Some(&1));
        assert_eq!(map.get("zz"), None);
    }

    fn attribs(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        pairs
            .iter()
            .map(|(f, s)| (f.to_string(), s.map(str::to_string)))
            .collect()
    }

    #[test]
    fn attribute_files_routes_or_skips() {
        let mut map = std::collections::HashMap::new();
        map.insert("a1".to_string(), 0usize);
        map.insert("b1".to_string(), 1usize);
        let files = attribs(&[
            ("form.rs", Some("a1")), // -> entry 0
            ("api.rs", Some("b1")),  // -> entry 1
            ("README", Some("zz")),  // last touch outside the stack -> skip
            ("new.rs", None),        // brand new -> skip
        ]);
        let got = attribute_files(&files, &map);
        assert_eq!(got[0].entry, Some(0));
        assert_eq!(got[1].entry, Some(1));
        assert_eq!(got[2].entry, None);
        assert_eq!(got[3].entry, None);
    }

    #[test]
    fn plan_absorb_groups_by_entry_and_drops_empty() {
        let attributions = vec![
            FileAttribution {
                file: "form.rs".into(),
                entry: Some(0),
            },
            FileAttribution {
                file: "form_test.rs".into(),
                entry: Some(0),
            },
            FileAttribution {
                file: "api.rs".into(),
                entry: Some(2),
            },
            FileAttribution {
                file: "stray.rs".into(),
                entry: None,
            },
        ];
        let branches = vec!["u/f-1".into(), "u/f-2".into(), "u/f-3".into()];
        let tips = vec!["sha1".into(), "sha2".into(), "sha3".into()];
        let plan = plan_absorb(&attributions, &branches, &tips);
        // entry 1 has no files -> dropped; 0 and 2 kept, in order.
        assert_eq!(plan.groups.len(), 2);
        assert_eq!(plan.groups[0].branch, "u/f-1");
        assert_eq!(plan.groups[0].target, "sha1");
        assert_eq!(plan.groups[0].files, vec!["form.rs", "form_test.rs"]);
        assert_eq!(plan.groups[1].branch, "u/f-3");
        assert_eq!(plan.groups[1].target, "sha3");
        assert_eq!(plan.skipped, vec!["stray.rs"]);
    }

    #[test]
    fn render_absorb_plan_covers_groups_skips_and_empty() {
        let branches = vec!["u/f-1".into(), "u/f-2".into()];
        let tips = vec!["abc12345def".into(), "999".into()];
        let attributions = vec![
            FileAttribution {
                file: "form.rs".into(),
                entry: Some(0),
            },
            FileAttribution {
                file: "stray.rs".into(),
                entry: None,
            },
        ];
        let plan = plan_absorb(&attributions, &branches, &tips);
        let out = render_absorb_plan(&plan);
        assert!(out.contains("Would absorb staged changes into 1 entry:"));
        assert!(out.contains("u/f-1 (abc12345):"));
        assert!(out.contains("      form.rs"));
        assert!(out.contains("Left staged (no matching entry):"));
        assert!(out.contains("      stray.rs"));

        // Nothing maps to an entry.
        let empty = plan_absorb(
            &[FileAttribution {
                file: "x".into(),
                entry: None,
            }],
            &branches,
            &tips,
        );
        let out = render_absorb_plan(&empty);
        assert!(out.contains("Nothing to absorb"));
    }

    #[test]
    fn render_absorb_done_reports_and_warns_force_push() {
        let plan = AbsorbPlan {
            groups: vec![AbsorbGroup {
                branch: "u/f-1".into(),
                target: "sha1".into(),
                files: vec!["a.rs".into(), "b.rs".into()],
            }],
            skipped: vec!["s.rs".into()],
        };
        let out = render_absorb_done(&plan);
        assert!(out.contains("absorbed 2 file(s) into u/f-1"));
        assert!(out.contains("Rewrote 1 entry"));
        assert!(out.contains("force-with-lease"));
        assert!(out.contains("1 file(s) left staged"));
    }

    #[test]
    fn absorb_plan_json_shape_is_stable() {
        let plan = AbsorbPlan {
            groups: vec![AbsorbGroup {
                branch: "u/f-1".into(),
                target: "sha1".into(),
                files: vec!["a.rs".into()],
            }],
            skipped: vec!["s.rs".into()],
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["groups"][0]["branch"], "u/f-1");
        assert_eq!(v["groups"][0]["target"], "sha1");
        assert_eq!(v["groups"][0]["files"][0], "a.rs");
        assert_eq!(v["skipped"][0], "s.rs");
    }
}
