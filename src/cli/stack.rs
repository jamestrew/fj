//! `fj stack` — stacked-branch / stacked-PR management.
//!
//! Stack state is local and git-native (`<git-dir>/fj/stack.json`, see
//! [`stack_state`](super::stack_state)) — no server state, offline-capable,
//! never a committed manifest. `stack new` (author a stack), `stack review`
//! (show the stack + each branch's PR state), `stack sync` (push each branch
//! and create/update its Forgejo PR, chaining bases so each PR targets the
//! previous branch, and mirroring a navigation block into each PR body unless
//! `--no-annotate`), and `stack ship` (merge bottom-up behind a pre-flight
//! over both the stack's git shape and its review state, replaying the
//! branches above each squash-landed item onto the new trunk tip and
//! re-syncing their PR bases) are implemented, as is
//! `stack split` (turn a single branch's commits into one stacked entry each,
//! moving only local refs) and `stack absorb` (fold staged changes into the
//! entries that introduced them, via `fixup!` commits + an autosquash
//! `--update-refs` rebase). All five are implemented.
//!
//! `ship` is a client-orchestrated sequence, not an atomic land: it makes one
//! merge call per item, and a failure part-way leaves the items below it
//! merged. What it guarantees is that it refuses up front when the stack can't
//! land as planned, stops cleanly when the ground shifts under it, and never
//! leaves a squash-landed stack for the user to repair by hand.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
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
    ///
    /// Merges bottom-up, one PR at a time. This is a sequence, not an atomic
    /// land: a failure part-way leaves the items below it merged. What ship
    /// guarantees is that it refuses before merging anything when the stack
    /// can't land as planned, and stops cleanly when it can't continue.
    ///
    /// Before the first merge it checks that each unmerged branch descends
    /// from the one below it, that the lowest one contains the trunk's current
    /// tip, that each branch matches its PR's head, and (unless --force) that
    /// every PR is approved and green.
    ///
    /// After a squash land it replays the branches above onto the new trunk
    /// tip (one rebase --update-refs, git 2.38+), force-pushes them with a
    /// lease, and re-points their PRs, so a squash-landed stack is never left
    /// for you to repair by hand. --no-restack opts out of that and of the
    /// local checks it depends on.
    ///
    /// Between merges it re-checks the next item and stops if the ground
    /// shifted. Re-running is safe: merged items are skipped.
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
    /// Don't restack after a squash land: skip the local pre-flight, and leave
    /// the branches above each landed item based on the pre-squash commits
    /// (you then have to rebase and force-push them yourself).
    #[arg(long)]
    pub no_restack: bool,
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
    /// The PR head's commit sha. `stack ship` compares it against the sha it
    /// validated (and the sha it force-pushed while restacking) to tell an
    /// independent push apart from its own work.
    head_sha: String,
    /// "approved" | "changes_requested" | "commented" | "none".
    review: String,
    /// Combined CI state, or null when unavailable.
    checks: Option<String>,
    /// Mergeability for open PRs; null when not applicable / unknown.
    mergeable: Option<bool>,
    /// Logins with a pending review request.
    requested_reviewers: Vec<String>,
}

/// Aggregate PR review state into the ship-gate token. The reduction lives in
/// fjord-core so fj and the GUI clients share one verdict (ADR 0001); fj maps
/// its fetched reviews to the per-review states core reduces over.
fn summarize_reviews(reviews: &[api::pull::Review]) -> String {
    let states: Vec<String> = reviews.iter().map(|r| r.state.clone()).collect();
    fjord_core::workflows::stack::summarize_reviews(&states)
        .as_str()
        .to_string()
}

/// Assemble the per-PR view from already-fetched pieces. Pure for testing.
fn build_entry_pr(full: &api::pull::Pull, review: String, checks: Option<String>) -> EntryPr {
    EntryPr {
        number: full.number,
        title: full.title.clone(),
        state: full.state.clone(),
        draft: full.draft,
        url: full.html_url.clone(),
        head_sha: full.head.sha.clone(),
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
/// Delegates the rules to `fjord_core`; fj only supplies its command label and
/// maps the core error back to a bare anyhow message.
fn validate_items(items: &[StackItem]) -> Result<()> {
    let core_items: Vec<fjord_core::workflows::stack::StackItem> =
        items.iter().map(Into::into).collect();
    fjord_core::workflows::stack::validate_items(&core_items, "`fj stack new`").map_err(|e| match e
    {
        fjord_core::error::CoreError::Validation { message } => anyhow::anyhow!(message),
        other => anyhow::anyhow!(other.to_string()),
    })
}

use fjord_core::workflows::stack::SyncStep;

/// The bottom-up sync plan, computed by `fjord_core`. `SyncStep` serializes for
/// `--json` / dry-run.
fn plan_sync(stack: &Stack) -> Vec<SyncStep> {
    fjord_core::workflows::stack::plan_sync(&stack.into())
}

fn render_plan(plan: &[SyncStep]) -> String {
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

/// Mirror the stack navigation into every item's PR body, preserving any
/// human-authored content. Lists the PRs once for their current bodies, then
/// PATCHes only those whose body actually changes. Returns how many were
/// updated. Items without a PR number are skipped. The block markup, payload,
/// and splice rules live in `fjord_core`; fj supplies its caption label.
async fn annotate_prs(client: &Client, owner: &str, name: &str, stack: &Stack) -> Result<usize> {
    let page = api::pull::list(client, owner, name, api::pull::ListOptions::default()).await?;
    let core_stack: fjord_core::workflows::stack::Stack = stack.into();
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
        let new_body = fjord_core::workflows::stack::apply_stack_navigation(
            &existing,
            &core_stack,
            i,
            "`fj stack sync`",
        );
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

/// One commit ahead of the base: its full sha and subject line. The canonical
/// shape lives in fjord-core (ADR 0001); re-exported so call sites and tests
/// keep the short local name.
use fjord_core::workflows::stack::Commit;

/// Parse `git log --format=%H\x1f%s` output (oldest first) into commits.
/// Blank lines and sha-less rows are skipped. Delegates to fjord-core.
fn parse_commits(raw: &str) -> Vec<Commit> {
    fjord_core::workflows::stack::parse_commits(raw)
}

/// A split needs at least two commits; one (or zero) is a no-op. Delegates the
/// rule to fjord-core, mapping its validation error back to a bare fj message
/// (core's `Display` prefixes "invalid request:", which we don't want here).
fn validate_split(commits: &[Commit]) -> Result<()> {
    fjord_core::workflows::stack::validate_split(commits).map_err(|e| match e {
        fjord_core::error::CoreError::Validation { message } => anyhow::anyhow!(message),
        other => anyhow::anyhow!(other.to_string()),
    })
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
    fjord_core::workflows::stack::plan_split(base, handle, slug, commits)
        .into_iter()
        .map(StackItem::from)
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

// fjord-core owns the canonical absorb routing and its data shapes (ADR 0001);
// re-exported so the render helpers, the `absorb()` driver, and the tests keep
// the short local names. The shapes are identical (pub fields, same order, same
// serde layout), so this slice is pure delegation with no conversion.
use fjord_core::workflows::stack::{AbsorbPlan, FileAttribution};

/// Map every commit sha in the stack to the entry (0-based) it belongs to.
/// `entries[i]` is the list of shas in item `i`'s range (`prev..branch`).
/// Later entries win a tie (a sha should only appear once in a linear stack).
/// Delegates to fjord-core.
fn index_commits(entries: &[Vec<String>]) -> std::collections::HashMap<String, usize> {
    fjord_core::workflows::stack::index_commits(entries)
}

/// Attribute each `(file, last_touching_sha)` to a stack entry via the
/// sha→entry index. A file whose last touch is outside the stack (e.g. from the
/// base branch, or brand new) is left unattributed. Delegates to fjord-core.
fn attribute_files(
    files: &[(String, Option<String>)],
    sha_to_entry: &std::collections::HashMap<String, usize>,
) -> Vec<FileAttribution> {
    fjord_core::workflows::stack::attribute_files(files, sha_to_entry)
}

/// Build the plan from attributions. `tips[i]` is item `i`'s target commit sha
/// (its branch tip); `branches[i]` its branch name. Groups preserve entry order
/// (bottom-up) and are dropped when they have no files. Delegates to fjord-core.
fn plan_absorb(
    attributions: &[FileAttribution],
    branches: &[String],
    tips: &[String],
) -> AbsorbPlan {
    fjord_core::workflows::stack::plan_absorb(attributions, branches, tips)
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

/// The Forgejo merge styles bridge 1:1 with `fjord_core`'s, so the surfaces
/// agree on the token sent to the API.
impl From<fjord_core::workflows::stack::MergeStyle> for MergeStyle {
    fn from(s: fjord_core::workflows::stack::MergeStyle) -> Self {
        use fjord_core::workflows::stack::MergeStyle as Core;
        match s {
            Core::Merge => MergeStyle::Merge,
            Core::Rebase => MergeStyle::Rebase,
            Core::RebaseMerge => MergeStyle::RebaseMerge,
            Core::Squash => MergeStyle::Squash,
        }
    }
}

impl From<MergeStyle> for fjord_core::workflows::stack::MergeStyle {
    fn from(s: MergeStyle) -> Self {
        use fjord_core::workflows::stack::MergeStyle as Core;
        match s {
            MergeStyle::Merge => Core::Merge,
            MergeStyle::Rebase => Core::Rebase,
            MergeStyle::RebaseMerge => Core::RebaseMerge,
            MergeStyle::Squash => Core::Squash,
        }
    }
}

/// The merge style to use. `--squash`/`--merge` are explicit; the default is
/// a plain merge commit, the most universally-allowed (safest) style. The
/// squash-wins resolution lives in `fjord_core`.
fn merge_style(squash: bool, merge: bool) -> MergeStyle {
    if merge {
        return MergeStyle::Merge;
    }
    fjord_core::workflows::stack::merge_style(squash).into()
}

/// Whether a PR is shippable: approved and green. "Green" treats no CI
/// (`none`/absent) as non-blocking; only pending/failure/error block.
/// Already-merged PRs are shippable (they're skipped). Returns the blocking
/// reason on failure. Gating rules live in `fjord_core`.
fn shippable(review: &str, checks: Option<&str>, state: &str) -> std::result::Result<(), String> {
    match fjord_core::workflows::stack::ship_blocker(review, checks, state) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

use fjord_core::workflows::stack::ShipStep;

/// The bottom-up ship plan, computed by `fjord_core`. `ShipStep` serializes for
/// `--dry-run` / `--json`.
fn plan_ship(stack: &Stack, style: MergeStyle) -> Vec<ShipStep> {
    fjord_core::workflows::stack::plan_ship(&stack.into(), style.into())
}

fn render_ship_plan(plan: &[ShipStep], restack: bool) -> String {
    use std::fmt::Write;
    let mut s = String::from("Would ship bottom-up (no merges or deletes):\n");
    for p in plan {
        let _ = writeln!(s, "  merge {} → {} ({})", p.branch, p.base, p.style);
    }
    if restack && plan.len() > 1 {
        let _ = writeln!(
            s,
            "  after each merge: rebase the branches above onto the new trunk tip, \
             force-push them (--force-with-lease), and re-point their PRs"
        );
    }
    s
}

/// Recovery guidance printed when a merge fails partway through. Composed by
/// `fjord_core`; fj only supplies its command label.
fn recovery_message(failed_branch: &str, merged: &[String]) -> String {
    fjord_core::workflows::stack::recovery_message(failed_branch, merged, "`fj stack ship`")
}

/// Recovery guidance when ship stops *before* merging an item, because the
/// ground shifted under the plan or the stack couldn't be restacked. Mirrors
/// [`recovery_message`]'s shape (what stopped, what already landed, how to
/// resume) without claiming a merge was attempted.
fn stopped_message(branch: &str, reason: &str, merged: &[String]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "Stopped before merging '{branch}': {reason}.");
    if merged.is_empty() {
        let _ = writeln!(s, "  Nothing was merged.");
    } else {
        let _ = writeln!(s, "  Already merged: {}", merged.join(", "));
    }
    let _ = writeln!(
        s,
        "  Fix the issue, then re-run `fj stack ship`; merged items are skipped."
    );
    s
}

/// Whether a merge style rewrites the commits it lands. A squash (or rebase)
/// land replaces the item's commits with new ones, so every branch above it is
/// still parented on history that no longer exists on the trunk and has to be
/// replayed. A merge commit keeps the original commits reachable, so the
/// branches above stay valid and only their PR bases move.
fn rewrites_history(style: MergeStyle) -> bool {
    match style {
        MergeStyle::Merge => false,
        MergeStyle::Squash | MergeStyle::Rebase | MergeStyle::RebaseMerge => true,
    }
}

/// What the item's PR head says about the branch that would actually merge.
#[derive(Debug, Clone, PartialEq)]
enum HeadCheck {
    /// The local tip and the PR head are the same commit.
    Agrees,
    /// `(local tip, PR head)`: the branch and its PR disagree about what would
    /// merge.
    Disagrees(String, String),
    /// fj could not establish the PR's head, so it cannot say which sha would
    /// merge or which sha it would lease a force-push against.
    ///
    /// This refuses rather than passes, and that direction is the whole fix:
    /// an unknown head used to fall through a catch-all arm as "nothing to
    /// report", which silently disabled the check for any stack whose PRs sat
    /// outside the single unpaginated page ship starts from.
    Unknown,
}

/// What git says about one pending stack item, gathered before any merge.
#[derive(Debug, Clone)]
struct BranchAncestry {
    branch: String,
    /// The item's PR number, for the message when its head can't be read.
    number: u64,
    /// The ref this item should sit on: the branch below it, or the trunk for
    /// the lowest unmerged item. Used for the message, not the test.
    below: String,
    /// The branch resolves in this clone.
    exists: bool,
    /// `below` is an ancestor of `branch` (tested against the trunk's *fetched*
    /// tip for the lowest item, so a stale local trunk can't pass).
    descends: bool,
    /// How the local tip compares to the PR head.
    head: HeadCheck,
}

/// Pre-flight the stack's shape against git and refuse before merging anything
/// if it can't land as planned: a missing branch, a chain that isn't linear
/// (item _i_ must descend from item _i-1_, and the lowest unmerged item from
/// the trunk's current tip), or a branch whose local tip disagrees with the PR
/// that would be merged. `observed` is bottom-up over the *unmerged* items.
///
/// Failing here is the whole point: the alternative is discovering it after
/// the bottom item has already landed.
fn linear_history_blocker(observed: &[BranchAncestry]) -> Option<String> {
    for (i, o) in observed.iter().enumerate() {
        if !o.exists {
            return Some(format!(
                "branch '{}' is not in this clone, so ship can't verify the stack's \
                 history before merging. Fetch it, or pass --no-restack to ship \
                 without the local checks.",
                o.branch
            ));
        }
        if !o.descends {
            return Some(if i == 0 {
                format!(
                    "'{}' is not up to date with '{}': the stack's lowest unmerged \
                     branch doesn't contain the trunk's current tip. Restack it onto \
                     '{}', run `fj stack sync`, then re-run `fj stack ship` (or pass \
                     --no-restack to ship the stack as it is).",
                    o.branch, o.below, o.below
                )
            } else {
                format!(
                    "stack is not linear: '{}' is not an ancestor of '{}'. Restack the \
                     stack, run `fj stack sync`, then re-run `fj stack ship`.",
                    o.below, o.branch
                )
            });
        }
        match &o.head {
            HeadCheck::Agrees => {}
            HeadCheck::Disagrees(local, head) => {
                return Some(format!(
                    "'{}' is at {} locally but its PR head is {}: the branch and the pull \
                     request disagree about what would merge. Run `fj stack sync` to push \
                     it (or fetch, if the PR is ahead), then re-run `fj stack ship`.",
                    o.branch,
                    short_sha(local),
                    short_sha(head)
                ));
            }
            HeadCheck::Unknown => {
                return Some(format!(
                    "could not read PR #{}'s head, so ship can't confirm that '{}' is what \
                     would merge. Re-run when the forge is reachable, or pass --no-restack \
                     to ship without the local checks.",
                    o.number, o.branch
                ));
            }
        }
    }
    None
}

/// Gather what git says about every pending item, bottom-up, for the
/// pre-flight.
///
/// Split out of `ship` so the refusal it feeds can be exercised directly: this
/// is the seam where a PR head fj had not read used to be dropped on the floor
/// instead of stopping the run.
fn observe_stack(
    repo: &git::Repo,
    items: &[StackItem],
    pending: &[usize],
    numbers: &[u64],
    trunk: &str,
    trunk_tip: &str,
    heads: &std::collections::HashMap<u64, String>,
) -> Result<Vec<BranchAncestry>> {
    let mut observed = Vec::with_capacity(pending.len());
    for (k, &i) in pending.iter().enumerate() {
        let branch = items[i].branch.clone();
        let number = numbers[i];
        let exists = repo.rev_exists(&format!("refs/heads/{branch}"));
        let (below, below_rev) = if k == 0 {
            (trunk.to_string(), trunk_tip.to_string())
        } else {
            let b = items[pending[k - 1]].branch.clone();
            (b.clone(), b)
        };
        let descends = exists && repo.is_ancestor(&below_rev, &branch);
        let head = match heads.get(&number) {
            Some(head) if exists && !head.is_empty() => {
                let local = repo.rev_parse(&branch)?;
                if local == *head {
                    HeadCheck::Agrees
                } else {
                    HeadCheck::Disagrees(local, head.clone())
                }
            }
            // Either the branch isn't in this clone (which `exists` refuses
            // first, with a better message) or fj never read the PR's head.
            // Neither is "fine": the fallback has to be the refusing variant.
            _ => HeadCheck::Unknown,
        };
        observed.push(BranchAncestry {
            branch,
            number,
            below,
            exists,
            descends,
            head,
        });
    }
    Ok(observed)
}

/// The heads `--force` re-validates each item against before merging it.
///
/// The gated path re-reads every PR with `enrich_pr` and takes the head from
/// there; `--force` skips that pass entirely, so this list is the only thing
/// standing between a shifted head and a merge. A head fj could not read is
/// therefore a refusal, not an empty expectation: an empty one makes
/// `revalidate`'s head-moved guard vacuous, which is exactly how the bottom
/// item could land before a stale lease was ever rejected.
fn forced_expected_heads(
    pending: &[usize],
    numbers: &[u64],
    heads: &std::collections::HashMap<u64, String>,
) -> Result<std::collections::HashMap<usize, ExpectedHead>> {
    let mut expected = std::collections::HashMap::new();
    for &i in pending {
        let number = numbers[i];
        let sha = heads
            .get(&number)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "could not read PR #{number}'s head, so --force has nothing to check \
                     it against before merging. Re-run when the forge is reachable."
                )
            })?;
        expected.insert(i, ExpectedHead::validated(sha));
    }
    Ok(expected)
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// How the branches above a landed item get replayed and re-pointed.
#[derive(Debug, PartialEq)]
struct RestackPlan {
    /// The rebase `<upstream>`: the branch whose commits just landed. Commits
    /// after it are the ones to replay.
    upstream: String,
    /// The rebase target. Rewriting the top branch with `--update-refs` moves
    /// every intermediate stack ref in a single pass, the same plumbing
    /// `stack absorb` uses.
    top: String,
    /// Every branch above the landed item, bottom-up. Parallel to `bases`.
    branches: Vec<String>,
    /// The base each of those items' PRs should target once the replay lands:
    /// the item directly above the landed one moves to the trunk, the rest keep
    /// targeting the branch below them.
    bases: Vec<String>,
}

/// Build the restack for item `merged`, or `None` when it was the top item and
/// nothing is left above it.
///
/// Also `None` when something above it has *already* merged: the chain then has
/// a hole in it, ship can't have put it there (it merges strictly bottom-up),
/// and replaying a branch whose PR already landed would be wrong. Ship falls
/// back to re-pointing the next PR and leaves the repair to the user.
fn plan_restack(items: &[StackItem], merged: usize, trunk: &str) -> Option<RestackPlan> {
    let above = items.get(merged + 1..)?;
    if above.is_empty() || above.iter().any(item_is_merged) {
        return None;
    }
    let branches: Vec<String> = above.iter().map(|i| i.branch.clone()).collect();
    let mut bases = Vec::with_capacity(branches.len());
    bases.push(trunk.to_string());
    for b in branches.iter().take(branches.len() - 1) {
        bases.push(b.clone());
    }
    Some(RestackPlan {
        upstream: items[merged].branch.clone(),
        top: branches.last().cloned().expect("non-empty"),
        branches,
        bases,
    })
}

/// The head fj expects an item's PR to be at when it comes up for merge.
#[derive(Debug, Clone, Default)]
struct ExpectedHead {
    /// Shas fj is itself responsible for: the one it validated in the
    /// pre-flight, plus any it force-pushed while restacking.
    shas: Vec<String>,
    /// fj has force-pushed this branch during this run, so a pending CI state
    /// is fj re-arming the checks rather than the ground shifting.
    restacked: bool,
}

impl ExpectedHead {
    fn validated(sha: &str) -> Self {
        ExpectedHead {
            shas: if sha.is_empty() {
                Vec::new()
            } else {
                vec![sha.to_string()]
            },
            restacked: false,
        }
    }

    fn pushed(&mut self, sha: &str) {
        if !sha.is_empty() {
            self.shas.push(sha.to_string());
        }
        self.restacked = true;
    }

    /// The head fj most recently put on the branch, and the only one the forge
    /// should be reporting once it has caught up.
    fn current(&self) -> Option<&str> {
        self.shas.last().map(String::as_str)
    }

    /// A sha fj put there but has since replaced. Distinct from a sha fj never
    /// pushed: this one means the forge has not observed the force-push yet,
    /// not that the ground shifted.
    fn superseded(&self, sha: &str) -> bool {
        self.shas.iter().any(|s| s == sha) && self.current() != Some(sha)
    }
}

/// The verdict on an item re-checked just before its merge.
///
/// The two failure modes are deliberately separate: one is worth re-reading,
/// the other never is.
#[derive(Debug, PartialEq)]
enum Revalidation {
    /// Go ahead and merge.
    Ready,
    /// The forge is still serving a snapshot from before fj's force-push.
    /// Nothing in that snapshot can be trusted, so re-read rather than judge
    /// on it. Terminal if the forge never catches up.
    Stale(String),
    /// A hard stop. Re-reading will not change it.
    Blocked(String),
}

impl Revalidation {
    /// The reason to stop, if this verdict is terminal. A `Stale` verdict is
    /// only terminal once the retry budget is spent, which is the caller's
    /// business; by then "the forge never caught up" is itself a blocker.
    fn blocker(self) -> Option<String> {
        match self {
            Revalidation::Ready => None,
            Revalidation::Stale(r) | Revalidation::Blocked(r) => Some(r),
        }
    }
}

/// Re-validate an item just before merging it, once something below it has
/// landed.
///
/// `gated` is false under `--force`, which drops the review/checks gate but
/// never the structural checks: a PR that closed, stopped merging cleanly, or
/// moved to a head fj didn't put there still stops the run.
///
/// When fj has restacked the branch it force-pushed a new head, which re-arms
/// CI; that pending state is fj's own doing, and the tree was already approved
/// and green before the replay, so the checks gate is not re-applied to it.
/// (`--auto`, the planned follow-up, is what will let a stack wait for the
/// re-armed checks instead.)
///
/// A head fj superseded with its own force-push is `Stale`, not `Ready`: every
/// other field on that PR record was computed for the pre-restack head against
/// the pre-retarget base, so judging `state`, `mergeable` or the checks off it
/// would be reading a snapshot of a shape that no longer exists.
fn revalidate(pr: &EntryPr, expected: &ExpectedHead, gated: bool) -> Revalidation {
    if let Some(current) = expected.current() {
        if pr.head_sha != current {
            if expected.restacked && expected.superseded(&pr.head_sha) {
                return Revalidation::Stale(format!(
                    "the forge still reports its pre-restack head {}, not the {} fj \
                     force-pushed onto the new trunk tip",
                    short_sha(&pr.head_sha),
                    short_sha(current)
                ));
            }
            return Revalidation::Blocked(format!(
                "its head moved to {} after the ship plan was made (expected {})",
                short_sha(&pr.head_sha),
                short_sha(current)
            ));
        }
    }
    if pr.state != "open" {
        return Revalidation::Blocked(format!("its pull request is no longer open ({})", pr.state));
    }
    if pr.mergeable == Some(false) {
        return Revalidation::Blocked("it no longer merges cleanly into its base".to_string());
    }
    if !gated {
        return Revalidation::Ready;
    }
    let checks = if expected.restacked {
        None
    } else {
        pr.checks.as_deref()
    };
    match shippable(&pr.review, checks, &pr.state).err() {
        Some(reason) => Revalidation::Blocked(reason),
        None => Revalidation::Ready,
    }
}

#[derive(Debug, Serialize)]
struct ShipOp {
    branch: String,
    number: u64,
    /// "merged" | "skipped"
    action: String,
    /// Branches replayed onto the new trunk tip and force-pushed after this
    /// item landed. Empty unless the merge style rewrites history.
    restacked: Vec<String>,
}

/// Forgejo updates a PR's head and re-computes its mergeability asynchronously
/// after a push, so a branch fj has just force-pushed can briefly still report
/// the pre-push head. Re-read a few times before calling that a shift.
const REVALIDATE_ATTEMPTS: usize = 3;
const REVALIDATE_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Replay every branch above the item that just landed onto the new trunk tip,
/// force-push them, and re-point their PRs. One `rebase --onto … --update-refs`
/// moves the whole remainder, so either all the refs move or the rebase aborts
/// and none do. Returns the branches it rewrote, with their new tips.
async fn restack_onto_trunk(
    repo: &git::Repo,
    ctx: &crate::cli::context::RepoContext,
    remote: &str,
    trunk: &str,
    plan: &RestackPlan,
    numbers: &[u64],
) -> Result<Vec<(String, String)>> {
    // The trunk moved when the PR merged; read where it landed.
    let trunk_tip = repo
        .fetch_branch(remote, trunk)
        .with_context(|| format!("fetching '{remote}/{trunk}' after the merge"))?;
    let upstream = repo.rev_parse(&plan.upstream)?;
    // Lease each force-push against the tip the branch had before the replay.
    let mut leases = Vec::with_capacity(plan.branches.len());
    for branch in &plan.branches {
        leases.push(repo.rev_parse(branch)?);
    }

    if let Err(e) = repo.rebase_onto_update_refs(&trunk_tip, &upstream, &plan.top) {
        repo.rebase_abort();
        return Err(e.context(format!(
            "replaying the stack above '{}' onto {}. The rebase was rolled back; \
             resolve the conflict by hand (git rebase --onto {} {} {} --update-refs), \
             push with --force-with-lease, then re-run `fj stack ship`",
            plan.upstream,
            short_sha(&trunk_tip),
            short_sha(&trunk_tip),
            short_sha(&upstream),
            plan.top
        )));
    }

    let mut rewritten = Vec::with_capacity(plan.branches.len());
    for (branch, lease) in plan.branches.iter().zip(&leases) {
        if let Err(e) = repo.push_force_with_lease(remote, branch, lease) {
            // A rejected lease and a failed transport need opposite advice. If
            // the push never landed, the local refs are the replayed truth and
            // re-pushing them is right. If the *lease* was rejected, someone
            // pushed to the branch after fj read it, the local refs no longer
            // contain their commit, and "push it again" is how that commit
            // gets destroyed.
            let context = if e.downcast_ref::<git::StaleLease>().is_some() {
                format!(
                    "publishing the replayed '{branch}'. Its remote tip moved after fj \
                     read it, so the local refs are NOT authoritative here: they don't \
                     contain whatever was pushed. Fetch '{remote}/{branch}' and reconcile \
                     the two before pushing anything; do not re-run with --force"
                )
            } else {
                format!(
                    "publishing the replayed '{branch}'. The local refs are already \
                     correct; push the remaining branches with --force-with-lease, \
                     then re-run `fj stack ship`"
                )
            };
            return Err(e.context(context));
        }
        rewritten.push((branch.clone(), repo.rev_parse(branch)?));
    }

    // Re-sync the PR bases to match the new shape: the item directly above the
    // landed one now targets the trunk, the rest keep targeting the branch
    // below them. Idempotent, so re-pointing to an unchanged base is a no-op.
    for ((branch, base), &number) in plan.branches.iter().zip(&plan.bases).zip(numbers) {
        api::pull::edit(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            number,
            &EditPull {
                base: Some(base),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("re-pointing PR #{number} ({branch}) at '{base}'"))?;
    }
    Ok(rewritten)
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn ship(args: ShipArgs, host: Option<&str>) -> Result<()> {
    let Some(mut stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    validate_items(&stack.items)?;
    let style = merge_style(args.squash, args.merge);
    // A squash (or rebase) land rewrites the commits it lands, so the branches
    // above it must be replayed onto the new trunk tip or they keep carrying
    // the pre-squash history. A merge commit leaves them valid as they are.
    let restack = rewrites_history(style) && !args.no_restack;

    if args.dry_run {
        let plan = plan_ship(&stack, style);
        if args.json {
            return output::print_json(&serde_json::to_value(&plan)?);
        }
        print!("{}", render_ship_plan(&plan, restack));
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
    let trunk = stack.base.clone();
    let remote = stack.repo.remote.clone();

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
    // Items that still need merging; the ones below them already landed and
    // are part of the trunk, so they take no part in the pre-flight.
    let pending: Vec<usize> = (0..stack.items.len())
        .filter(|&i| !item_is_merged(&stack.items[i]))
        .collect();

    // PR head shas, from the list we already have. `page` is a single
    // unpaginated page of open PRs, so a stack whose PRs fall outside it would
    // leave the head checks with nothing to compare against; read those
    // directly. Costs nothing on a repo whose open PRs fit in one page.
    let mut heads: std::collections::HashMap<u64, String> = page
        .items
        .iter()
        .map(|p| (p.number, p.head.sha.clone()))
        .collect();
    for &i in &pending {
        let number = numbers[i];
        if heads.contains_key(&number) {
            continue;
        }
        // Best-effort: a head that still can't be read leaves the item's
        // `HeadCheck::Unknown` (or `forced_expected_heads`) to refuse the run
        // up front, which is where the operator wants to hear about it.
        if let Ok(pr) = api::pull::get(&ctx.client, &ctx.owner, &ctx.name, number).await {
            heads.insert(number, pr.head.sha.clone());
        }
    }

    let repo = git::Repo::discover();
    let started_on = repo.current_branch();

    // Pre-flight 1: the stack's shape in git. Skipped by --no-restack, which
    // opts out of the local checks along with the replay.
    if !args.no_restack && !pending.is_empty() {
        if restack {
            let version = parse_git_version(&git::version_string()?)
                .ok_or_else(|| anyhow::anyhow!("could not determine the git version"))?;
            if !supports_update_refs(version) {
                bail!(
                    "restacking after a {} land needs git >= 2.38 (for rebase \
                     --update-refs); found {}.{}. Pass --no-restack to ship without it.",
                    style.as_str(),
                    version.0,
                    version.1
                );
            }
        }
        let trunk_tip = repo.fetch_branch(&remote, &trunk).with_context(|| {
            format!("fetching '{remote}/{trunk}' to check the stack is current")
        })?;
        let observed = observe_stack(
            &repo,
            &stack.items,
            &pending,
            &numbers,
            &trunk,
            &trunk_tip,
            &heads,
        )?;
        if let Some(reason) = linear_history_blocker(&observed) {
            bail!("{reason}");
        }
    }

    // Pre-flight 2: validate ALL PRs before merging any (unless --force), so we
    // never merge the bottom and then stop.
    let mut expected: std::collections::HashMap<usize, ExpectedHead> =
        std::collections::HashMap::new();
    if !args.force {
        for &i in &pending {
            let number = numbers[i];
            let branch = &stack.items[i].branch;
            let pr = enrich_pr(&ctx.client, &ctx.owner, &ctx.name, number)
                .await
                .ok_or_else(|| anyhow::anyhow!("could not fetch PR #{number} for '{branch}'"))?;
            if let Err(reason) = shippable(&pr.review, pr.checks.as_deref(), &pr.state) {
                bail!("'{branch}' (PR #{number}) is not ready: {reason}. Pass --force to ship anyway.");
            }
            expected.insert(i, ExpectedHead::validated(&pr.head_sha));
        }
    } else {
        expected = forced_expected_heads(&pending, &numbers, &heads)?;
    }

    // Merge bottom-up; stop cleanly the moment anything stops matching the plan.
    let mut ops = Vec::with_capacity(stack.items.len());
    let mut merged: Vec<String> = Vec::new();
    let mut landed_this_run = false;
    for i in 0..stack.items.len() {
        let branch = stack.items[i].branch.clone();
        let number = numbers[i];
        let mut restacked: Vec<String> = Vec::new();

        if item_is_merged(&stack.items[i]) {
            ops.push(ShipOp {
                branch: branch.clone(),
                number,
                action: "skipped".to_string(),
                restacked,
            });
            record_merged(&mut stack, i, &mut merged, branch);
            stack_state::save(&stack)?;
            continue;
        }

        // Re-validate before merging: the pre-flight verdict was taken before
        // anything landed, and a merge can change what sits above it.
        if landed_this_run {
            let entry = expected.entry(i).or_default();
            let mut verdict = Revalidation::Ready;
            for attempt in 0..REVALIDATE_ATTEMPTS {
                let pr = enrich_pr(&ctx.client, &ctx.owner, &ctx.name, number)
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!("could not re-fetch PR #{number} for '{branch}'")
                    })?;
                verdict = revalidate(&pr, entry, !args.force);
                // Only a stale read is worth re-reading. `Ready` merges, and a
                // `Blocked` verdict is a real shift that waiting cannot undo.
                if !matches!(verdict, Revalidation::Stale(_)) || attempt + 1 == REVALIDATE_ATTEMPTS
                {
                    break;
                }
                tokio::time::sleep(REVALIDATE_BACKOFF).await;
            }
            // A forge that never caught up is a blocker in its own right: fj
            // would otherwise merge on a snapshot it knows is out of date.
            if let Some(reason) = verdict.blocker() {
                eprint!("{}", stopped_message(&branch, &reason, &merged));
                bail!("'{branch}' (PR #{number}) is no longer ready to merge: {reason}");
            }
        }

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
        landed_this_run = true;

        // The item landed. Bring the rest of the stack onto the new trunk.
        match (restack, plan_restack(&stack.items, i, &trunk)) {
            (true, Some(plan)) => {
                let above: Vec<u64> = numbers[i + 1..].to_vec();
                match restack_onto_trunk(&repo, &ctx, &remote, &trunk, &plan, &above).await {
                    Ok(rewritten) => {
                        for (offset, (b, sha)) in rewritten.iter().enumerate() {
                            expected.entry(i + 1 + offset).or_default().pushed(sha);
                            restacked.push(b.clone());
                        }
                        for (offset, base) in plan.bases.iter().enumerate() {
                            stack.items[i + 1 + offset].base = base.clone();
                        }
                    }
                    Err(e) => {
                        // The item landed and its branch is merging away, so
                        // the PR above it can't keep targeting it whatever
                        // happened to the replay. Leave the forge in the state
                        // ship would have produced without a restack, and let
                        // the user repair git.
                        let _ = api::pull::edit(
                            &ctx.client,
                            &ctx.owner,
                            &ctx.name,
                            numbers[i + 1],
                            &EditPull {
                                base: Some(&trunk),
                                ..Default::default()
                            },
                        )
                        .await;
                        stack.items[i + 1].base = trunk.clone();
                        record_merged(&mut stack, i, &mut merged, branch.clone());
                        stack_state::save(&stack)?;
                        eprint!(
                            "{}",
                            stopped_message(
                                &plan.top,
                                "the branches above the item that just landed could not be \
                                 replayed onto the new trunk tip",
                                &merged
                            )
                        );
                        return Err(e);
                    }
                }
            }
            // No replay needed (merge commit, or --no-restack): the next PR
            // still has to leave the base branch that just merged away.
            _ => {
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
                    stack.items[i + 1].base = trunk.clone();
                }
            }
        }

        if args.delete_branches {
            let _ = git::delete_remote_branch(&remote, &branch);
            let _ = git::delete_local_branch(&branch);
        }
        ops.push(ShipOp {
            branch: branch.clone(),
            number,
            action: "merged".to_string(),
            restacked,
        });
        record_merged(&mut stack, i, &mut merged, branch);
        stack_state::save(&stack)?;
    }

    stack.shipped_at = Some(Utc::now().to_rfc3339());
    stack_state::save(&stack)?;

    // A restack leaves the top branch checked out; put the user back.
    if let Some(b) = started_on {
        if repo.current_branch().as_deref() != Some(b.as_str())
            && repo.rev_exists(&format!("refs/heads/{b}"))
        {
            let _ = repo.checkout(&b);
        }
    }

    if args.json {
        return output::print_json(&serde_json::to_value(&ops)?);
    }
    use std::fmt::Write as _;
    let mut out = String::new();
    for op in &ops {
        let _ = writeln!(out, "✓ {} {} (#{})", op.action, op.branch, op.number);
        if !op.restacked.is_empty() {
            let _ = writeln!(
                out,
                "  restacked onto {}: {}",
                trunk,
                op.restacked.join(", ")
            );
        }
    }
    let _ = writeln!(out, "Shipped {} item(s).", ops.len());
    print!("{out}");
    Ok(())
}

/// An item whose PR is already merged: ship skips it (and a re-run resumes
/// from the first that isn't).
fn item_is_merged(item: &StackItem) -> bool {
    item.pr
        .as_ref()
        .map(|p| p.state == "merged")
        .unwrap_or(false)
}

/// Mark item `i` merged in the stack file and remember it for the recovery
/// text. The caller saves, so progress survives a mid-run stop.
fn record_merged(stack: &mut Stack, i: usize, merged: &mut Vec<String>, branch: String) {
    if let Some(pr) = stack.items[i].pr.as_mut() {
        pr.state = "merged".to_string();
    }
    stack.items[i].status = "merged".to_string();
    merged.push(branch);
    stack.updated_at = Utc::now().to_rfc3339();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::{CommandFactory, Parser};
    use fjord_core::workflows::stack::AbsorbGroup;

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
                        head_sha: "aaa".into(),
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
    fn merge_style_flags_select_expected_style() {
        assert!(matches!(merge_style(false, true), MergeStyle::Merge));
        assert!(matches!(merge_style(true, false), MergeStyle::Squash));
        assert!(matches!(merge_style(false, false), MergeStyle::Merge));
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

        let out = render_ship_plan(&plan, false);
        assert!(out.contains("Would ship bottom-up"));
        assert!(out.contains("merge feature/a → main (squash)"));
        assert!(out.contains("merge feature/b → feature/a (squash)"));
        assert!(!out.contains("force-push"));

        // With a history-rewriting style the plan says the stack gets replayed.
        let out = render_ship_plan(&plan, true);
        assert!(out.contains("rebase the branches above onto the new trunk tip"));
        assert!(out.contains("--force-with-lease"));

        // JSON shape is stable.
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v[0]["branch"], "feature/a");
        assert_eq!(v[0]["base"], "main");
        assert_eq!(v[2]["style"], "squash");
    }

    // ---- ship: restack after a squash land ----

    #[test]
    fn ship_parses_no_restack() {
        let cli = Cli::try_parse_from(["fj", "stack", "ship", "--squash", "--no-restack"])
            .expect("parses");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Ship(a),
            }) => assert!(a.squash && a.no_restack),
            other => panic!("expected stack ship, got {other:?}"),
        }
        // Off by default: a squash land restacks the remainder.
        let cli = Cli::try_parse_from(["fj", "stack", "ship"]).expect("parses");
        match cli.command {
            Command::Stack(StackCmd {
                command: StackSub::Ship(a),
            }) => assert!(!a.no_restack),
            other => panic!("expected stack ship, got {other:?}"),
        }
    }

    #[test]
    fn only_history_rewriting_styles_need_a_restack() {
        // A squash replaces the item's commits, so everything above it is
        // parented on history the trunk no longer has.
        assert!(rewrites_history(MergeStyle::Squash));
        assert!(rewrites_history(MergeStyle::Rebase));
        assert!(rewrites_history(MergeStyle::RebaseMerge));
        // A merge commit keeps the original commits reachable.
        assert!(!rewrites_history(MergeStyle::Merge));
    }

    fn ancestry(branch: &str, below: &str) -> BranchAncestry {
        BranchAncestry {
            branch: branch.into(),
            number: 1,
            below: below.into(),
            exists: true,
            descends: true,
            head: HeadCheck::Agrees,
        }
    }

    #[test]
    fn linear_history_blocker_passes_a_linear_current_stack() {
        let observed = vec![
            ancestry("u/f-1", "main"),
            ancestry("u/f-2", "u/f-1"),
            ancestry("u/f-3", "u/f-2"),
        ];
        assert_eq!(linear_history_blocker(&observed), None);
        // Nothing pending (everything already merged) has nothing to check.
        assert_eq!(linear_history_blocker(&[]), None);
    }

    #[test]
    fn linear_history_blocker_refuses_a_stack_behind_the_trunk() {
        let mut observed = vec![ancestry("u/f-1", "main"), ancestry("u/f-2", "u/f-1")];
        observed[0].descends = false;
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("'u/f-1' is not up to date with 'main'"));
        assert!(reason.contains("trunk's current tip"));
        assert!(reason.contains("fj stack sync"));
    }

    #[test]
    fn linear_history_blocker_refuses_a_broken_link() {
        let mut observed = vec![
            ancestry("u/f-1", "main"),
            ancestry("u/f-2", "u/f-1"),
            ancestry("u/f-3", "u/f-2"),
        ];
        observed[2].descends = false;
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("stack is not linear"));
        assert!(reason.contains("'u/f-2' is not an ancestor of 'u/f-3'"));
        assert!(reason.contains("fj stack sync"));
    }

    #[test]
    fn linear_history_blocker_refuses_a_branch_that_is_not_here() {
        let mut observed = vec![ancestry("u/f-1", "main"), ancestry("u/f-2", "u/f-1")];
        observed[1].exists = false;
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("'u/f-2' is not in this clone"));
        assert!(reason.contains("--no-restack"));
    }

    #[test]
    fn linear_history_blocker_refuses_when_the_branch_and_its_pr_disagree() {
        let mut observed = vec![ancestry("u/f-1", "main")];
        observed[0].head = HeadCheck::Disagrees("abcdef1234".into(), "9876543210".into());
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("'u/f-1' is at abcdef12 locally but its PR head is 98765432"));
        assert!(reason.contains("fj stack sync"));
    }

    /// An unreadable PR head refuses instead of passing. It used to fall
    /// through a catch-all and leave the check with nothing to report.
    #[test]
    fn linear_history_blocker_refuses_a_head_it_could_not_read() {
        let mut observed = vec![ancestry("u/f-1", "main"), ancestry("u/f-2", "u/f-1")];
        observed[0].number = 412;
        observed[0].head = HeadCheck::Unknown;
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("could not read PR #412's head"));
        assert!(reason.contains("'u/f-1'"));
    }

    #[test]
    fn linear_history_blocker_reports_the_lowest_break_first() {
        // Two breaks: the message names the one nearest the trunk, because
        // that is the one the user has to repair first.
        let mut observed = vec![
            ancestry("u/f-1", "main"),
            ancestry("u/f-2", "u/f-1"),
            ancestry("u/f-3", "u/f-2"),
        ];
        observed[1].descends = false;
        observed[2].descends = false;
        let reason = linear_history_blocker(&observed).expect("should refuse");
        assert!(reason.contains("'u/f-1' is not an ancestor of 'u/f-2'"));
    }

    /// The pre-flight hole, closed.
    ///
    /// `ship` reads PR heads from one unpaginated page of open PRs. A stack
    /// whose PRs sit outside that page left *both* head guards with nothing to
    /// check: the pre-flight's comparison fell through a catch-all, and
    /// `--force` handed `revalidate` an empty expectation. A stale
    /// bottom item therefore passed the pre-flight, squash-landed, and only
    /// then failed mid-restack on a rejected lease, with the bottom already
    /// merged and the stack half-repaired. Both guards now refuse first.
    ///
    /// Driven against a real repo so the git shape is genuinely linear and
    /// current: nothing but the head check can account for the refusal.
    #[test]
    fn a_stale_bottom_is_refused_up_front_when_its_pr_is_off_the_list_page() {
        use crate::gitfixture::{git, git_supports_update_refs, published_stack};
        use std::collections::HashMap;
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (_tmp, work, _bare, _tip1) = published_stack();
        let repo = git::Repo::at(&work);
        let items = stack_with_branches(&["u/s-1", "u/s-2", "u/s-3"]).items;
        let pending = vec![0usize, 1, 2];
        // PR numbers well past the first page of open PRs.
        let numbers = vec![41u64, 42, 43];
        let trunk_tip = git(&work, &["rev-parse", "main"]);

        // Baseline: with every head read and matching, the stack passes. This
        // is what keeps the refusals below meaningful.
        let agreeing: HashMap<u64, String> = numbers
            .iter()
            .zip(["u/s-1", "u/s-2", "u/s-3"])
            .map(|(&n, b)| (n, git(&work, &["rev-parse", b])))
            .collect();
        let observed = observe_stack(
            &repo, &items, &pending, &numbers, "main", &trunk_tip, &agreeing,
        )
        .expect("observing a healthy stack");
        assert!(observed.iter().all(|o| o.exists && o.descends));
        assert_eq!(linear_history_blocker(&observed), None);

        // The hole: the stack's PRs are off the page, so no head was read.
        let unread: HashMap<u64, String> = HashMap::new();
        let observed = observe_stack(
            &repo, &items, &pending, &numbers, "main", &trunk_tip, &unread,
        )
        .expect("observing still works without heads");
        // The git shape is fine; this is purely the head check talking.
        assert!(observed.iter().all(|o| o.exists && o.descends));
        assert_eq!(observed[0].head, HeadCheck::Unknown);
        let reason = linear_history_blocker(&observed)
            .expect("an unread head must refuse before anything merges");
        assert!(reason.contains("could not read PR #41's head"));
        assert!(reason.contains("'u/s-1'"));

        // …and the same input under --force, which skips the per-PR gate and
        // so has only this list to lean on. It used to yield an empty
        // expectation and merge; now it refuses.
        let err = forced_expected_heads(&pending, &numbers, &unread)
            .expect_err("--force must not merge an item whose head it never read");
        let chain = format!("{err:#}");
        assert!(chain.contains("could not read PR #41's head"));
        assert!(chain.contains("--force has nothing to check it against"));

        // A head that *was* read but disagrees is refused with both shas, the
        // case the pre-flight was written for.
        let mut stale = agreeing.clone();
        let pr_head = git(&work, &["rev-parse", "u/s-2"]);
        stale.insert(41, pr_head.clone()); // someone pushed; the local tip is behind
        let observed = observe_stack(
            &repo, &items, &pending, &numbers, "main", &trunk_tip, &stale,
        )
        .expect("observing a stale bottom");
        let local = git(&work, &["rev-parse", "u/s-1"]);
        assert_eq!(
            observed[0].head,
            HeadCheck::Disagrees(local.clone(), pr_head.clone())
        );
        let reason = linear_history_blocker(&observed).expect("a stale bottom must refuse");
        assert!(reason.contains(&format!("'u/s-1' is at {}", short_sha(&local))));
        assert!(reason.contains("disagree about what would merge"));

        // Finally, under --force the re-validation now carries a real sha, so
        // a head that moves after the plan still stops the run rather than
        // being waved through by an empty expectation.
        let expected = forced_expected_heads(&pending, &numbers, &agreeing)
            .expect("every head was read this time");
        let entry = expected.get(&0).expect("bottom item");
        assert_eq!(entry.shas, vec![git(&work, &["rev-parse", "u/s-1"])]);
        let moved = entry_pr("open", "approved", Some("success"), Some(true));
        // `entry_pr`'s head is a sha fj never validated or pushed, so this is
        // Blocked rather than Stale: there was no force-push to lag behind.
        let verdict = revalidate(&moved, entry, false);
        assert!(matches!(verdict, Revalidation::Blocked(_)));
        assert!(verdict
            .blocker()
            .expect("a moved head must stop --force")
            .contains("its head moved to"));
    }

    #[test]
    fn plan_restack_reparents_the_remainder_and_resyncs_bases() {
        let items = stack_with_branches(&["u/f-1", "u/f-2", "u/f-3"]).items;
        let plan = plan_restack(&items, 0, "main").expect("two items are left above");
        // The commits to replay are the ones after the branch that just landed.
        assert_eq!(plan.upstream, "u/f-1");
        // One rebase of the top branch moves every ref in between.
        assert_eq!(plan.top, "u/f-3");
        assert_eq!(plan.branches, vec!["u/f-2", "u/f-3"]);
        // The item directly above the landed one now targets the trunk; the
        // rest keep targeting the branch below them.
        assert_eq!(plan.bases, vec!["main", "u/f-2"]);

        // Landing the middle item next leaves only the top.
        let plan = plan_restack(&items, 1, "main").expect("one item is left above");
        assert_eq!(plan.upstream, "u/f-2");
        assert_eq!(plan.top, "u/f-3");
        assert_eq!(plan.branches, vec!["u/f-3"]);
        assert_eq!(plan.bases, vec!["main"]);
    }

    #[test]
    fn plan_restack_is_none_at_the_top_of_the_stack() {
        let items = stack_with_branches(&["u/f-1", "u/f-2"]).items;
        assert_eq!(plan_restack(&items, 1, "main"), None);
        assert_eq!(plan_restack(&items, 9, "main"), None);
    }

    #[test]
    fn plan_restack_is_none_when_the_chain_above_has_a_hole() {
        // Someone merged the top item out of order. Replaying a branch whose PR
        // already landed would be wrong, so ship doesn't try.
        let mut items = stack_with_branches(&["u/f-1", "u/f-2", "u/f-3"]).items;
        items[2].pr = Some(ItemPr {
            number: 9,
            url: "u".into(),
            state: "merged".into(),
        });
        assert_eq!(plan_restack(&items, 0, "main"), None);
    }

    // ---- ship: re-validation between merges ----

    fn entry_pr(
        state: &str,
        review: &str,
        checks: Option<&str>,
        mergeable: Option<bool>,
    ) -> EntryPr {
        EntryPr {
            number: 7,
            title: "t".into(),
            state: state.into(),
            draft: false,
            url: "https://x/pulls/7".into(),
            head_sha: "sha-validated".into(),
            review: review.into(),
            checks: checks.map(str::to_string),
            mergeable,
            requested_reviewers: vec![],
        }
    }

    #[test]
    fn revalidate_lets_an_unchanged_ready_item_through() {
        let pr = entry_pr("open", "approved", Some("success"), Some(true));
        let expected = ExpectedHead::validated("sha-validated");
        assert_eq!(revalidate(&pr, &expected, true), Revalidation::Ready);
    }

    #[test]
    fn revalidate_stops_when_the_item_stops_being_mergeable() {
        // The item below landed and this one now conflicts with the trunk.
        let pr = entry_pr("open", "approved", Some("success"), Some(false));
        let expected = ExpectedHead::validated("sha-validated");
        let reason = revalidate(&pr, &expected, true)
            .blocker()
            .expect("should stop");
        assert!(reason.contains("no longer merges cleanly"));
        // Still enforced under --force: the merge would fail anyway.
        assert!(revalidate(&pr, &expected, false).blocker().is_some());
    }

    #[test]
    fn revalidate_stops_when_someone_else_pushed() {
        let mut pr = entry_pr("open", "approved", Some("success"), Some(true));
        pr.head_sha = "sha-theirs".into();
        let expected = ExpectedHead::validated("sha-validated");
        let reason = revalidate(&pr, &expected, true)
            .blocker()
            .expect("should stop");
        assert!(reason.contains("its head moved to sha-thei"));
    }

    #[test]
    fn revalidate_stops_when_the_pr_is_no_longer_open() {
        let pr = entry_pr("closed", "approved", Some("success"), None);
        let expected = ExpectedHead::validated("sha-validated");
        let reason = revalidate(&pr, &expected, true)
            .blocker()
            .expect("should stop");
        assert!(reason.contains("no longer open (closed)"));
    }

    #[test]
    fn revalidate_stops_when_the_review_or_checks_regressed() {
        let expected = ExpectedHead::validated("sha-validated");
        let pr = entry_pr("open", "changes_requested", Some("success"), Some(true));
        assert!(revalidate(&pr, &expected, true)
            .blocker()
            .expect("should stop")
            .contains("changes_requested"));
        let pr = entry_pr("open", "approved", Some("failure"), Some(true));
        assert!(revalidate(&pr, &expected, true)
            .blocker()
            .expect("should stop")
            .contains("checks not green"));
        // --force drops the review/checks gate but keeps the rest.
        assert_eq!(revalidate(&pr, &expected, false), Revalidation::Ready);
    }

    #[test]
    fn revalidate_accepts_the_head_fj_force_pushed_and_its_re_armed_checks() {
        let mut expected = ExpectedHead::validated("sha-validated");
        expected.pushed("sha-restacked");
        // fj rewrote the branch onto the new trunk tip, so CI is running again
        // on a tree that was already approved and green. Pending is fj's own
        // doing, not the ground shifting.
        let mut pr = entry_pr("open", "approved", Some("pending"), Some(true));
        pr.head_sha = "sha-restacked".into();
        assert_eq!(revalidate(&pr, &expected, true), Revalidation::Ready);
        // The review gate still applies to the restacked head.
        pr.review = "changes_requested".into();
        assert!(revalidate(&pr, &expected, true).blocker().is_some());
        // A head fj did not push is still a shift, restack or no restack.
        let mut theirs = entry_pr("open", "approved", Some("pending"), Some(true));
        theirs.head_sha = "sha-theirs".into();
        assert!(revalidate(&theirs, &expected, true).blocker().is_some());
    }

    #[test]
    fn revalidate_treats_a_superseded_head_as_a_lagging_forge_not_a_pass() {
        // The regression this guards: the pre-restack sha stays in the
        // acceptance set, so `contains` used to return Ready and the retry
        // loop broke on the first read. A stale read must be Stale, which is
        // what makes the caller re-read instead of merging.
        let mut expected = ExpectedHead::validated("sha-validated");
        expected.pushed("sha-restacked");
        let mut lagging = entry_pr("open", "approved", Some("success"), Some(true));
        lagging.head_sha = "sha-validated".into();
        let verdict = revalidate(&lagging, &expected, true);
        assert!(
            matches!(verdict, Revalidation::Stale(_)),
            "a superseded head is the forge lagging, not a ready item: {verdict:?}"
        );
        // Nothing on that snapshot is judged: the item is not merged, and the
        // reason names both shas so the terminal message is actionable.
        let reason = verdict
            .blocker()
            .expect("stale is terminal once retries run out");
        assert!(reason.contains("pre-restack head sha-vali"));
        assert!(reason.contains("sha-rest"));
    }

    #[test]
    fn revalidate_does_not_treat_a_stale_read_as_stale_without_a_restack() {
        // No force-push happened, so there is exactly one expected sha and a
        // mismatch can only be somebody else. This is what keeps the common
        // path free of the retry latency.
        let expected = ExpectedHead::validated("sha-validated");
        let mut pr = entry_pr("open", "approved", Some("success"), Some(true));
        pr.head_sha = "sha-theirs".into();
        assert!(matches!(
            revalidate(&pr, &expected, true),
            Revalidation::Blocked(_)
        ));
        // And the matching head is Ready with no retry.
        let ready = entry_pr("open", "approved", Some("success"), Some(true));
        assert_eq!(revalidate(&ready, &expected, true), Revalidation::Ready);
    }

    #[test]
    fn revalidate_judges_nothing_off_a_stale_snapshot() {
        // Every other field on a pre-restack record was computed for the old
        // head against the base that has since merged away. A stale
        // `mergeable: true` must not be read as "still merges cleanly".
        let mut expected = ExpectedHead::validated("sha-validated");
        expected.pushed("sha-restacked");
        let mut lagging = entry_pr("open", "approved", Some("success"), Some(true));
        lagging.head_sha = "sha-validated".into();
        assert!(matches!(
            revalidate(&lagging, &expected, true),
            Revalidation::Stale(_)
        ));
        // Same verdict under --force: this is a structural check, not a gate.
        assert!(matches!(
            revalidate(&lagging, &expected, false),
            Revalidation::Stale(_)
        ));
    }

    #[test]
    fn revalidate_stale_resolves_to_ready_once_the_forge_catches_up() {
        // The retry budget exists for exactly this: the same PR, read again.
        let mut expected = ExpectedHead::validated("sha-validated");
        expected.pushed("sha-restacked");
        let mut pr = entry_pr("open", "approved", Some("pending"), Some(true));
        pr.head_sha = "sha-validated".into();
        assert!(matches!(
            revalidate(&pr, &expected, true),
            Revalidation::Stale(_)
        ));
        pr.head_sha = "sha-restacked".into();
        assert_eq!(revalidate(&pr, &expected, true), Revalidation::Ready);
    }

    #[test]
    fn revalidate_lag_tolerance_does_not_compound_down_the_stack() {
        // Each land restacks again, so an entry deep in the stack accumulates
        // several shas fj pushed. Only the newest may pass; every earlier one
        // is the forge lagging, however many rounds ago fj pushed it.
        let mut expected = ExpectedHead::validated("sha-validated");
        expected.pushed("sha-restack-1");
        expected.pushed("sha-restack-2");
        for stale in ["sha-validated", "sha-restack-1"] {
            let mut pr = entry_pr("open", "approved", Some("success"), Some(true));
            pr.head_sha = stale.into();
            assert!(
                matches!(revalidate(&pr, &expected, true), Revalidation::Stale(_)),
                "{stale} is superseded and must not pass"
            );
        }
        let mut current = entry_pr("open", "approved", Some("success"), Some(true));
        current.head_sha = "sha-restack-2".into();
        assert_eq!(revalidate(&current, &expected, true), Revalidation::Ready);
    }

    #[test]
    fn revalidate_without_a_known_head_falls_back_to_the_gate() {
        // Nothing to compare against, so the sha check is skipped and the
        // ordinary gate decides. `ship` no longer reaches this state (it
        // refuses up front rather than merging on an unread head, see
        // `forced_expected_heads`), but the fallback is kept honest here so
        // the gate stays the floor if anything else ever constructs one.
        let expected = ExpectedHead::validated("");
        let pr = entry_pr("open", "approved", Some("success"), Some(true));
        assert_eq!(revalidate(&pr, &expected, true), Revalidation::Ready);
        let pr = entry_pr("open", "none", Some("success"), Some(true));
        assert!(revalidate(&pr, &expected, true).blocker().is_some());
    }

    #[test]
    fn stopped_message_names_the_reason_and_what_already_landed() {
        let m = stopped_message("u/f-2", "it no longer merges cleanly", &[]);
        assert!(m.contains("Stopped before merging 'u/f-2': it no longer merges cleanly."));
        assert!(m.contains("Nothing was merged"));
        assert!(m.contains("re-run `fj stack ship`; merged items are skipped"));

        let m = stopped_message("u/f-3", "its head moved", &["u/f-1".into(), "u/f-2".into()]);
        assert!(m.contains("Already merged: u/f-1, u/f-2"));
    }

    #[test]
    fn item_is_merged_reads_the_recorded_pr_state() {
        let mut items = stack_with_branches(&["u/f-1", "u/f-2"]).items;
        assert!(!item_is_merged(&items[0])); // no PR recorded yet
        items[0].pr = Some(ItemPr {
            number: 7,
            url: "u".into(),
            state: "merged".into(),
        });
        items[1].pr = Some(ItemPr {
            number: 8,
            url: "u".into(),
            state: "open".into(),
        });
        assert!(item_is_merged(&items[0]));
        assert!(!item_is_merged(&items[1]));
    }

    // ---- ship: the restack, end to end against real git ----

    fn repo_context(server: &wiremock::MockServer) -> crate::cli::context::RepoContext {
        crate::cli::context::RepoContext {
            client: client_for(server),
            host: "x".into(),
            web_base: None,
            owner: "o".into(),
            name: "r".into(),
        }
    }

    /// The footgun, closed: a squash land leaves the branches above it based on
    /// the pre-squash commit until something replays them. This drives the real
    /// rebase, the real force-push (to a bare "origin"), and the PR retarget.
    #[tokio::test]
    async fn restack_onto_trunk_replays_pushes_and_repoints_every_remaining_pr() {
        use crate::gitfixture::{
            count, git, git_supports_update_refs, published_stack, squash_land_on_remote,
        };
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (tmp, work, bare, tip1) = published_stack();
        // PR #7 (u/s-1) squash-merges: the trunk gains one new commit.
        let landed = squash_land_on_remote(tmp.path(), &bare);

        let items = stack_with_branches(&["u/s-1", "u/s-2", "u/s-3"]).items;
        let plan = plan_restack(&items, 0, "main").expect("two items above");

        let server = MockServer::start().await;
        // u/s-2 moves onto the trunk; u/s-3 keeps targeting u/s-2.
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/8"))
            .and(body_json(serde_json::json!({ "base": "main" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pull_value(8, "u/s-2", "open", false)),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/9"))
            .and(body_json(serde_json::json!({ "base": "u/s-2" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(pull_value(9, "u/s-3", "open", false)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let ctx = repo_context(&server);
        let repo = git::Repo::at(&work);
        let rewritten = restack_onto_trunk(&repo, &ctx, "origin", "main", &plan, &[8, 9])
            .await
            .expect("the stack should replay onto the landed trunk");

        let branches: Vec<String> = rewritten.iter().map(|(b, _)| b.clone()).collect();
        assert_eq!(branches, vec!["u/s-2", "u/s-3"]);

        // Locally: both branches now descend from the squashed trunk tip, one
        // commit each, and no longer carry the pre-squash commit.
        assert!(repo.is_ancestor(&landed, "u/s-2"));
        assert!(repo.is_ancestor("u/s-2", "u/s-3"));
        assert_eq!(count(&work, &format!("{landed}..u/s-2")), 1);
        assert_eq!(count(&work, "u/s-2..u/s-3"), 1);
        assert!(!repo.is_ancestor(&tip1, "u/s-3"));

        // And on the remote, so each PR shows only its own diff.
        assert_eq!(git(&bare, &["rev-parse", "u/s-2"]), rewritten[0].1);
        assert_eq!(git(&bare, &["rev-parse", "u/s-3"]), rewritten[1].1);
    }

    /// A replay that can't be applied rolls back and touches nothing: no moved
    /// refs, no force-push, no retargeted PRs.
    ///
    /// Scoped to `restack_onto_trunk` itself. Ship's caller still re-points the
    /// PR directly above the landed item at the trunk afterwards, because that
    /// item's base branch has merged away regardless of the replay; see the
    /// `Err` arm in `ship`.
    #[tokio::test]
    async fn restack_onto_trunk_rolls_back_and_retargets_nothing_on_conflict() {
        use crate::gitfixture::{
            clone_repo, commit_file, git, git_supports_update_refs, published_stack,
        };
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (tmp, work, bare, _tip1) = published_stack();

        // The trunk lands u/s-1 *and* an edit that collides with u/s-2.
        let forge = tmp.path().join("forge");
        clone_repo(&bare, &forge);
        commit_file(&forge, "a.txt", "a", "item 1 (#1)");
        commit_file(&forge, "b.txt", "conflicting", "someone else");
        git(&forge, &["push", "--quiet", "origin", "main"]);

        let before_2 = git(&work, &["rev-parse", "u/s-2"]);
        let before_3 = git(&work, &["rev-parse", "u/s-3"]);
        let published_2 = git(&bare, &["rev-parse", "u/s-2"]);

        let items = stack_with_branches(&["u/s-1", "u/s-2", "u/s-3"]).items;
        let plan = plan_restack(&items, 0, "main").expect("two items above");

        let server = MockServer::start().await;
        // Any PR write at all would be a bug: nothing was replayed.
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let ctx = repo_context(&server);
        let repo = git::Repo::at(&work);
        let err = restack_onto_trunk(&repo, &ctx, "origin", "main", &plan, &[8, 9])
            .await
            .expect_err("a conflicting replay must fail");
        let chain = format!("{err:#}");
        assert!(chain.contains("The rebase was rolled back"));
        assert!(chain.contains("--force-with-lease"));

        assert_eq!(git(&work, &["rev-parse", "u/s-2"]), before_2);
        assert_eq!(git(&work, &["rev-parse", "u/s-3"]), before_3);
        assert_eq!(git(&bare, &["rev-parse", "u/s-2"]), published_2);
    }

    #[test]
    fn ship_op_json_reports_the_branches_it_restacked() {
        let ops = vec![ShipOp {
            branch: "u/f-1".into(),
            number: 7,
            action: "merged".into(),
            restacked: vec!["u/f-2".into(), "u/f-3".into()],
        }];
        let v = serde_json::to_value(&ops).unwrap();
        assert_eq!(v[0]["branch"], "u/f-1");
        assert_eq!(v[0]["action"], "merged");
        assert_eq!(v[0]["restacked"][0], "u/f-2");
        assert_eq!(v[0]["restacked"][1], "u/f-3");
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

    // The block markup, payload, and splice rules now live in `fjord_core`
    // (covered by its own tests). fj's only contribution is the caption label;
    // assert that the literal it passes flows into the rendered navigation.
    #[test]
    fn navigation_carries_fj_caption() {
        let stack = stack_with_prs(&["feature/a", "feature/b"], &[7, 8]);
        let core_stack: fjord_core::workflows::stack::Stack = (&stack).into();
        let block = fjord_core::workflows::stack::apply_stack_navigation(
            "",
            &core_stack,
            1,
            "`fj stack sync`",
        );
        assert!(block
            .contains("_Maintained by `fj stack sync`; edits inside this block are overwritten._"));
    }

    #[tokio::test]
    async fn annotate_prs_patches_only_changed_bodies() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let stack = stack_with_prs(&["feature/a", "feature/b"], &[7, 8]);

        // PR #7 has no fj block yet -> must be patched. PR #8 already carries the
        // exact block it would get -> must be left alone.
        let body8 = fjord_core::workflows::stack::apply_stack_navigation(
            "",
            &(&stack).into(),
            1,
            "`fj stack sync`",
        );
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
