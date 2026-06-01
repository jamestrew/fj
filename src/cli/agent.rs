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
//! a narrative when one is configured (local-only otherwise). `fix` emits a
//! patch only (never mutates); `--write` / `--pr` come later.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use crate::api;
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fix(args: FixArgs, _host: Option<&str>) -> Result<()> {
    // Default contract: emit a patch only, never mutate. --write / --pr come
    // later; for now --dry-run just reports the plan without calling out.
    if args.dry_run {
        println!(
            "would: ask the configured agent to propose a patch and print it (no changes applied)"
        );
        return Ok(());
    }
    run_task(AgentTask::Fix, "the current change".to_string(), false)
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
