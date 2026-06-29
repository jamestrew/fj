//! `fj agent` — AI-assisted code operations (review / fix / explain).
//!
//! All three route through the [`agent_provider`](super::agent_provider)
//! abstraction: config comes from `FJ_AGENT_PROVIDER` / `FJ_AGENT_API_KEY` /
//! `FJ_AGENT_MODEL` (the Fjord token is never reused). With nothing
//! configured, every command reports a clear "agent backend not configured"
//! error; the built-in `mock` provider echoes for development.
//!
//! `explain [target]` feeds the provider real context: a file's contents, a
//! git revision (`git show`), or the working-tree diff when omitted.
//! `review` is a local diff-analysis command: it gathers the working-tree diff
//! (or `--pr <n>`, or each `--stack` item), redacts secrets / sensitive files
//! ([`agent_redact`](super::agent_redact)), derives severity-tagged local
//! findings ([`agent_review`](super::agent_review)), and asks the provider for
//! a narrative when one is configured (local-only otherwise). `fix` asks the
//! provider for a patch over the current change: by default it prints the patch
//! (no mutation); `--write` applies it to the working tree; `--pr` carries the
//! change plus the fix onto a fresh branch, commits, pushes, and opens a PR.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::pull::CreatePull;
use crate::cli::agent_provider::{self, AgentConfig, AgentRequest, AgentTask};
use crate::cli::agent_redact::redact_diff;
use crate::cli::agent_review::{
    self, assemble_context, build_context, local_findings, review_file, Redaction, ReviewContext,
    ReviewReport,
};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::stack_state;
use crate::{git, output};

#[derive(Debug, Args)]
pub struct AgentCmd {
    #[command(subcommand)]
    pub command: AgentSub,
}

#[derive(Debug, Subcommand)]
pub enum AgentSub {
    /// Run an automated review of the current change.
    Review(ReviewArgs),
    /// Apply automated fixes to the current change.
    Fix(FixArgs),
    /// Explain a piece of code or a change.
    Explain(ExplainArgs),
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Review a specific PR's diff instead of the working-tree diff.
    #[arg(long, conflicts_with = "stack")]
    pub pr: Option<u64>,
    /// Review each item of the local stack separately.
    #[arg(long)]
    pub stack: bool,
    /// Focus the analysis on security (adds a security advisory and frames the
    /// provider prompt accordingly).
    #[arg(long)]
    pub security: bool,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct FixArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Apply the proposed patch to the working tree (instead of printing it).
    #[arg(long, conflicts_with = "pr")]
    pub write: bool,
    /// Apply the patch on a new branch, commit, push, and open a PR.
    #[arg(long)]
    pub pr: bool,
    /// Branch name for `--pr` (default: `agent-fix`).
    #[arg(long, requires = "pr")]
    pub branch: Option<String>,
    /// PR title and commit subject for `--pr` (default: "Apply agent fix").
    #[arg(long, requires = "pr")]
    pub title: Option<String>,
    /// Print the planned actions and exit without mutating anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    // No `-R`: explain reads local code/diff via git, not the repo API.
    /// What to explain: a file path, a git revision, or omitted for the
    /// current working-tree diff.
    pub target: Option<String>,
    /// Emit JSON instead of a table (honors the global `--json-fields`).
    #[arg(long)]
    pub json: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: AgentCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        AgentSub::Review(args) => review(args, host).await,
        AgentSub::Fix(args) => fix(args, host).await,
        AgentSub::Explain(args) => explain(args, host).await,
    }
}

/// Resolve the configured provider, run a task, and print the result.
/// Shared by every agent command. With no backend configured this surfaces
/// the provider's "not configured" error; with `mock` it echoes the request.
/// `input` is the context the command gathered (PR6/PR7 enrich it per task).
#[cfg_attr(coverage_nightly, coverage(off))]
fn run_task(task: AgentTask, input: String, json: bool) -> Result<()> {
    let cfg = AgentConfig::from_env();
    let provider = agent_provider::resolve(&cfg)?;
    let resp = provider.complete(&AgentRequest {
        task,
        input,
        model: cfg.model.clone(),
    })?;
    if json {
        return output::print_json(&serde_json::json!({
            "task": task.as_str(),
            "provider": resp.provider,
            "model": resp.model,
            "output": resp.output,
        }));
    }
    println!("{}", resp.output);
    Ok(())
}

/// Build the structured prompt handed to the provider for a single target.
/// Pure over the gathered context and the already-redacted diff.
fn provider_input(
    target: &str,
    ctx: &ReviewContext,
    redacted_diff: &str,
    security: bool,
) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if security {
        let _ = writeln!(
            s,
            "Task: security review. Flag authz gaps, injection, secret handling, and risky \
             dependency changes."
        );
    } else {
        let _ = writeln!(
            s,
            "Task: code review. Flag bugs, risky changes, and missing tests; cite file/line."
        );
    }
    let _ = writeln!(s, "Target: {target}");
    let _ = writeln!(
        s,
        "Summary: {} files changed, +{} -{}, {} test file(s).",
        ctx.files_changed, ctx.additions, ctx.deletions, ctx.test_files_changed
    );
    if !ctx.risky_areas.is_empty() {
        let _ = writeln!(s, "Risky areas:");
        for area in &ctx.risky_areas {
            let _ = writeln!(s, "  - {}: {}", area.path, area.reason);
        }
    }
    let _ = writeln!(s, "\nDiff (secrets redacted):\n{redacted_diff}");
    s
}

/// One target to review: a label plus the numstat and full diff git produced.
struct ReviewTarget {
    label: String,
    context: ReviewContext,
    diff: String,
}

/// Gather a single review (working-tree or PR) plus the `--stack` per-item
/// fan-out, as a list of targets. Each is then redacted + reviewed identically.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn collect_targets(args: &ReviewArgs, host: Option<&str>) -> Result<Vec<ReviewTarget>> {
    if args.stack {
        return collect_stack_targets();
    }
    if let Some(number) = args.pr {
        let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
        let files = api::pull::files(&ctx.client, &ctx.owner, &ctx.name, number).await?;
        let tagged = files
            .iter()
            .map(|f| review_file(&f.filename, Some(f.additions), Some(f.deletions)))
            .collect();
        let diff = api::pull::diff_text(&ctx.client, &ctx.owner, &ctx.name, number).await?;
        return Ok(vec![ReviewTarget {
            label: format!("PR #{number}"),
            context: assemble_context(tagged),
            diff,
        }]);
    }
    let diff = git::output(&["diff", "HEAD"])?;
    if diff.trim().is_empty() {
        bail!("no uncommitted changes to review; pass --pr <number> or --stack");
    }
    let numstat = git::output(&["diff", "--numstat", "HEAD"])?;
    Ok(vec![ReviewTarget {
        label: "working tree".to_string(),
        context: build_context(&numstat),
        diff,
    }])
}

/// `--stack`: one target per item, diffing each branch against its base.
/// Items whose branch/base aren't present locally are skipped with a note.
#[cfg_attr(coverage_nightly, coverage(off))]
fn collect_stack_targets() -> Result<Vec<ReviewTarget>> {
    let Some(stack) = stack_state::load()? else {
        bail!("no stack for this repo. Run `fj stack new` to start one.");
    };
    let mut targets = Vec::new();
    for item in &stack.items {
        if !git::rev_exists(&item.branch) || !git::rev_exists(&item.base) {
            eprintln!(
                "skipping '{}': branch or base not found locally (fetch it first)",
                item.branch
            );
            continue;
        }
        let range = format!("{}...{}", item.base, item.branch);
        let diff = git::output(&["diff", &range])?;
        let numstat = git::output(&["diff", "--numstat", &range])?;
        targets.push(ReviewTarget {
            label: format!("stack item {}", item.branch),
            context: build_context(&numstat),
            diff,
        });
    }
    if targets.is_empty() {
        bail!("no reviewable stack items (branches not present locally)");
    }
    Ok(targets)
}

/// Redact a target's diff, run local analysis, and (when configured) ask the
/// provider for a narrative. Never sends secrets; unconfigured = local-only.
#[cfg_attr(coverage_nightly, coverage(off))]
fn review_target(t: ReviewTarget, security: bool) -> Result<ReviewReport> {
    let redacted = redact_diff(&t.diff, &[]);
    let comments = local_findings(&t.context, security);

    let cfg = AgentConfig::from_env();
    let (provider, model, narrative) = if cfg.provider.is_none() {
        // No AI backend: stay a useful local analysis instead of erroring.
        ("none".to_string(), None, String::new())
    } else {
        let provider = agent_provider::resolve(&cfg)?;
        let input = provider_input(&t.label, &t.context, &redacted.text, security);
        let resp = provider.complete(&AgentRequest {
            task: AgentTask::Review,
            input,
            model: cfg.model.clone(),
        })?;
        (resp.provider, resp.model, resp.output)
    };

    Ok(ReviewReport {
        target: t.label,
        context: t.context,
        comments,
        redaction: Redaction {
            secrets_masked: redacted.secrets_masked,
            withheld_files: redacted.dropped_files,
        },
        provider,
        model,
        narrative,
    })
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let targets = collect_targets(&args, host).await?;
    let reports: Vec<ReviewReport> = targets
        .into_iter()
        .map(|t| review_target(t, args.security))
        .collect::<Result<_>>()?;

    if args.json {
        let value = if args.stack {
            serde_json::to_value(&reports)?
        } else {
            serde_json::to_value(&reports[0])?
        };
        return output::print_json(&value);
    }
    let rendered: Vec<String> = reports.iter().map(agent_review::render_report).collect();
    print!("{}", rendered.join("\n"));
    Ok(())
}

/// The prompt handed to the provider for `fix`: the current change plus an
/// instruction to answer with a patch only. Pure over the diff.
fn fix_input(diff: &str) -> String {
    format!(
        "Task: propose a fix for the change below. Respond with ONLY a unified diff \
         that `git apply` accepts, no prose or fences.\n\nDiff:\n{diff}"
    )
}

fn fix_input_redacted(diff: &str) -> String {
    let redacted = redact_diff(diff, &[]);
    fix_input(&redacted.text)
}

/// The fix branch for `--pr`: the caller's `--branch`, else `agent-fix`.
fn fix_branch(provided: Option<&str>) -> String {
    provided
        .map(str::to_string)
        .unwrap_or_else(|| "agent-fix".to_string())
}

/// The PR title / commit subject for `--pr`.
fn fix_title(provided: Option<&str>) -> String {
    provided
        .map(str::to_string)
        .unwrap_or_else(|| "Apply agent fix".to_string())
}

/// Human description of what a given flag combination will do. Used by
/// `--dry-run` so the user sees the intent before anything happens.
fn fix_plan(write: bool, pr: bool, branch: &str, base: &str) -> String {
    if pr {
        format!(
            "would: ask the agent for a patch, commit it on branch '{branch}', \
             push, and open a PR against '{base}'"
        )
    } else if write {
        "would: ask the agent for a patch and apply it to the working tree".to_string()
    } else {
        "would: ask the agent for a patch and print it (no changes applied)".to_string()
    }
}

/// Pull the unified diff out of a provider response. Prefers the first fenced
/// code block; otherwise accepts the text as-is when it looks like a patch.
/// `None` when nothing patch-shaped is present.
fn extract_patch(output: &str) -> Option<String> {
    if let Some(block) = first_fenced_block(output) {
        let b = block.trim();
        if looks_like_patch(b) {
            return Some(ensure_trailing_newline(b));
        }
    }
    let trimmed = output.trim();
    if looks_like_patch(trimmed) {
        return Some(ensure_trailing_newline(trimmed));
    }
    None
}

/// First ```...``` fenced block's contents (without the fences), or `None` when
/// there's no terminated block.
fn first_fenced_block(s: &str) -> Option<String> {
    let mut in_block = false;
    let mut buf = String::new();
    for line in s.lines() {
        let is_fence = line.trim_start().starts_with("```");
        if !in_block {
            if is_fence {
                in_block = true;
            }
        } else if is_fence {
            return Some(buf);
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    None
}

/// Heuristic: does this text start like a unified diff git can apply?
fn looks_like_patch(s: &str) -> bool {
    let s = s.trim_start();
    s.starts_with("diff --git ") || s.starts_with("--- ") || s.starts_with("Index: ")
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fix(args: FixArgs, host: Option<&str>) -> Result<()> {
    // Never mutate on --dry-run: report the plan and stop before any API call.
    if args.dry_run {
        let base = git::current_branch().unwrap_or_else(|| "HEAD".to_string());
        println!(
            "{}",
            fix_plan(
                args.write,
                args.pr,
                &fix_branch(args.branch.as_deref()),
                &base
            )
        );
        return Ok(());
    }

    // The "current change" is the uncommitted working-tree diff, matching
    // `agent review` / `explain`.
    let diff = git::output(&["diff", "HEAD"])?;
    if diff.trim().is_empty() {
        bail!("no uncommitted changes to fix; make a change first (agent fix works on `git diff HEAD`)");
    }

    let cfg = AgentConfig::from_env();
    let provider = agent_provider::resolve(&cfg)?;
    let resp = provider.complete(&AgentRequest {
        task: AgentTask::Fix,
        input: fix_input_redacted(&diff),
        model: cfg.model.clone(),
    })?;

    // Default contract: emit the patch only, never mutate.
    if !args.write && !args.pr {
        println!("{}", resp.output);
        return Ok(());
    }

    let patch = extract_patch(&resp.output).ok_or_else(|| {
        anyhow::anyhow!(
            "the agent did not return a unified diff; re-run without --write/--pr to see its raw output"
        )
    })?;

    if args.write {
        git::apply_patch(&patch, true)?; // validate before touching the tree
        git::apply_patch(&patch, false)?;
        println!("✓ applied the agent's patch to the working tree (review with `git diff`)");
        return Ok(());
    }

    // --pr: carry the current change + the fix onto a fresh branch and PR it.
    // Do every cheap local check first, before resolve_repo touches the network
    // or the keychain, so a bad branch/patch fails fast and offline.
    let base = git::current_branch()
        .ok_or_else(|| anyhow::anyhow!("HEAD is detached; checkout a branch before --pr"))?;
    let branch = fix_branch(args.branch.as_deref());
    if git::rev_exists(&format!("refs/heads/{branch}")) {
        bail!("branch '{branch}' already exists; pass --branch <name>");
    }
    git::apply_patch(&patch, true)?; // validate against the working tree first
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;

    let title = fix_title(args.title.as_deref());
    // Create the branch (carries the uncommitted change), then apply + commit.
    // On any failure, return to the base branch and drop the empty branch so
    // the user's working state is preserved.
    git::create_branch(&branch, None)?;
    let staged = (|| -> Result<()> {
        git::apply_patch(&patch, false)?;
        git::add_all()?;
        git::commit(&title)?;
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = git::checkout(&base);
        let _ = git::delete_local_branch(&branch);
        return Err(e.context("preparing the fix branch failed; restored your previous branch"));
    }

    let remote = git::primary_remote()
        .map(|(name, _)| name)
        .unwrap_or_else(|| "origin".to_string());
    git::push(&remote, &branch)?;
    let body = "Automated fix proposed by `fj agent fix`.";
    let created = api::pull::create(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &CreatePull {
            title: &title,
            head: &branch,
            base: &base,
            body: Some(body),
            draft: false,
        },
    )
    .await?;
    println!(
        "✓ Created PR #{}: {title}\n{}",
        created.number, created.html_url
    );
    Ok(())
}

/// Where `agent explain` draws its context from.
#[derive(Debug, PartialEq)]
enum ExplainSource {
    /// No target given: explain the working-tree diff.
    WorkingDiff,
    /// An existing file path: explain its contents.
    File(String),
    /// Anything else: treat as a git revision to `git show`.
    Revision(String),
}

/// Decide what a `target` refers to. `file_exists` is whether the target names
/// an existing file (the caller checks the filesystem; this stays pure).
fn classify_target(target: Option<&str>, file_exists: bool) -> ExplainSource {
    match target {
        None => ExplainSource::WorkingDiff,
        Some(t) if file_exists => ExplainSource::File(t.to_string()),
        Some(t) => ExplainSource::Revision(t.to_string()),
    }
}

/// Build the context string to hand the provider for `explain`.
#[cfg_attr(coverage_nightly, coverage(off))]
fn gather_explain_input(target: Option<&str>) -> Result<String> {
    let exists = target.map(|t| Path::new(t).is_file()).unwrap_or(false);
    match classify_target(target, exists) {
        ExplainSource::WorkingDiff => {
            let diff = git::output(&["diff", "HEAD"])?;
            if diff.trim().is_empty() {
                bail!("no uncommitted changes to explain; pass a file path or a git revision");
            }
            Ok(format!("Working-tree diff:\n{diff}"))
        }
        ExplainSource::File(p) => {
            let content =
                std::fs::read_to_string(&p).with_context(|| format!("reading file {p}"))?;
            Ok(format!("File {p}:\n{content}"))
        }
        ExplainSource::Revision(r) => {
            if r.starts_with('-') {
                bail!("revision {r:?} starts with '-'; refusing to pass it to git");
            }
            let diff = git::output(&["show", &r])?;
            Ok(format!("Revision {r}:\n{diff}"))
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(args: ExplainArgs, _host: Option<&str>) -> Result<()> {
    let input = gather_explain_input(args.target.as_deref())?;
    run_task(AgentTask::Explain, input, args.json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::{CommandFactory, Parser};

    #[test]
    fn agent_subtree_is_internally_consistent() {
        // Structural smoke check over the whole derived tree, including this
        // group. clap only runs these checks behind debug_assert.
        Cli::command().debug_assert();
    }

    #[test]
    fn review_parses_pr_json_and_repo() {
        let cli =
            Cli::try_parse_from(["fj", "agent", "review", "--pr", "42", "--json", "-R", "o/n"])
                .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Review(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.pr, Some(42));
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected agent review, got {other:?}"),
        }
    }

    #[test]
    fn review_parses_stack_and_security() {
        let cli = Cli::try_parse_from(["fj", "agent", "review", "--stack", "--security"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Review(a),
            }) => {
                assert!(a.stack && a.security);
                assert!(a.pr.is_none());
            }
            other => panic!("expected agent review, got {other:?}"),
        }
        // --stack and --pr are mutually exclusive.
        assert!(Cli::try_parse_from(["fj", "agent", "review", "--stack", "--pr", "1"]).is_err());
    }

    #[test]
    fn provider_input_frames_task_and_includes_redacted_diff() {
        let ctx = build_context("10\t0\tsrc/auth/login.rs\n");
        let plain = provider_input("working tree", &ctx, "DIFF-BODY", false);
        assert!(plain.contains("Task: code review"));
        assert!(plain.contains("Target: working tree"));
        assert!(plain.contains("1 files changed, +10 -0"));
        assert!(plain.contains("Risky areas:"));
        assert!(plain.contains("DIFF-BODY"));

        let sec = provider_input("PR #5", &ctx, "X", true);
        assert!(sec.contains("Task: security review"));
    }

    #[test]
    fn fix_parses_dry_run_flag() {
        let cli = Cli::try_parse_from(["fj", "agent", "fix", "--dry-run", "-R", "o/n"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Fix(a),
            }) => {
                assert!(a.dry_run);
                assert_eq!(a.r.repo.as_deref(), Some("o/n"));
            }
            other => panic!("expected agent fix, got {other:?}"),
        }
    }

    #[test]
    fn fix_parses_write_pr_branch_title() {
        let cli = Cli::try_parse_from([
            "fj", "agent", "fix", "--pr", "--branch", "fix-1", "--title", "Tidy",
        ])
        .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Fix(a),
            }) => {
                assert!(a.pr && !a.write);
                assert_eq!(a.branch.as_deref(), Some("fix-1"));
                assert_eq!(a.title.as_deref(), Some("Tidy"));
            }
            other => panic!("expected agent fix, got {other:?}"),
        }
        // --write and --pr are mutually exclusive.
        assert!(Cli::try_parse_from(["fj", "agent", "fix", "--write", "--pr"]).is_err());
        // --branch / --title require --pr.
        assert!(Cli::try_parse_from(["fj", "agent", "fix", "--branch", "x"]).is_err());
        assert!(Cli::try_parse_from(["fj", "agent", "fix", "--title", "x"]).is_err());
    }

    #[test]
    fn fix_branch_and_title_defaults() {
        assert_eq!(fix_branch(None), "agent-fix");
        assert_eq!(fix_branch(Some("custom")), "custom");
        assert_eq!(fix_title(None), "Apply agent fix");
        assert_eq!(fix_title(Some("Tidy up")), "Tidy up");
    }

    #[test]
    fn fix_plan_describes_each_mode() {
        assert!(fix_plan(false, false, "agent-fix", "main").contains("print it"));
        assert!(fix_plan(true, false, "agent-fix", "main").contains("apply it to the working tree"));
        let pr = fix_plan(false, true, "agent-fix", "main");
        assert!(pr.contains("branch 'agent-fix'"));
        assert!(pr.contains("against 'main'"));
    }

    #[test]
    fn fix_input_frames_task_and_carries_diff() {
        let s = fix_input("diff --git a/x b/x\n@@ -1 +1 @@");
        assert!(s.contains("ONLY a unified diff"));
        assert!(s.contains("diff --git a/x b/x"));
    }

    #[test]
    fn fix_input_redacted_omits_sensitive_files_and_masks_secrets() {
        let diff = "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -0,0 +1 @@\n+API_TOKEN=super-secret\n\
diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+let token = \"ghp_abcdefghijklmnopqrstuvwxyz123456\";\n";
        let prompt = fix_input_redacted(diff);

        assert!(prompt.contains("ONLY a unified diff"));
        assert!(!prompt.contains("super-secret"));
        assert!(!prompt.contains("API_TOKEN"));
        assert!(!prompt.contains("diff --git a/.env b/.env"));
        assert!(prompt.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(prompt.contains("***REDACTED***"));
        assert!(!prompt.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn looks_like_patch_recognizes_diff_shapes() {
        assert!(looks_like_patch("diff --git a/x b/x\n"));
        assert!(looks_like_patch("--- a/x\n+++ b/x\n"));
        assert!(looks_like_patch("Index: x\n"));
        assert!(!looks_like_patch("here is your patch:\n"));
        assert!(!looks_like_patch("[mock fix output]\n"));
    }

    #[test]
    fn extract_patch_handles_fenced_raw_and_missing() {
        // fenced ```diff block, with prose around it
        let fenced =
            "Sure, here's the fix:\n```diff\ndiff --git a/x b/x\n--- a/x\n+++ b/x\n```\nHope it helps!";
        let got = extract_patch(fenced).expect("fenced patch");
        assert!(got.starts_with("diff --git a/x b/x"));
        assert!(got.ends_with('\n'));
        assert!(!got.contains("```"));
        assert!(!got.contains("Hope it helps"));

        // raw patch, no fence, no trailing newline -> one is added
        let raw = "diff --git a/x b/x\n--- a/x\n+++ b/x";
        let got = extract_patch(raw).expect("raw patch");
        assert!(got.ends_with("+++ b/x\n"));

        // a fenced block that isn't a patch -> falls through to None
        assert!(extract_patch("```\njust some text\n```").is_none());
        // pure prose -> None (e.g. the mock's default echo)
        assert!(extract_patch("[mock fix output]\nthe current change").is_none());
    }

    #[test]
    fn first_fenced_block_extracts_contents_only() {
        assert_eq!(
            first_fenced_block("a\n```\nL1\nL2\n```\nb").as_deref(),
            Some("L1\nL2\n")
        );
        // unterminated fence -> None
        assert!(first_fenced_block("```\nL1\nL2").is_none());
        // no fence -> None
        assert!(first_fenced_block("no fences here").is_none());
    }

    #[test]
    fn explain_parses_target_and_json() {
        let cli = Cli::try_parse_from(["fj", "agent", "explain", "src/main.rs", "--json"])
            .expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Explain(a),
            }) => {
                assert!(a.json);
                assert_eq!(a.target.as_deref(), Some("src/main.rs"));
            }
            other => panic!("expected agent explain, got {other:?}"),
        }
    }

    #[test]
    fn explain_target_is_optional() {
        let cli = Cli::try_parse_from(["fj", "agent", "explain"]).expect("should parse");
        match cli.command {
            Command::Agent(AgentCmd {
                command: AgentSub::Explain(a),
            }) => assert!(a.target.is_none()),
            other => panic!("expected agent explain, got {other:?}"),
        }
    }

    #[test]
    fn classify_target_picks_source() {
        assert_eq!(classify_target(None, false), ExplainSource::WorkingDiff);
        assert_eq!(
            classify_target(Some("src/main.rs"), true),
            ExplainSource::File("src/main.rs".into())
        );
        // Not an existing file → treated as a revision.
        assert_eq!(
            classify_target(Some("HEAD~1"), false),
            ExplainSource::Revision("HEAD~1".into())
        );
    }
}
