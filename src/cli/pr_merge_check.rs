//! Diagnose why `fj pr merge` was rejected.
//!
//! Forgejo answers every non-mergeable merge attempt with an opaque
//! `405 Method Not Allowed` (or a bare 500), which is indistinguishable across
//! merge conflicts, a disabled merge style, failing required checks, missing
//! approvals, an out-of-date branch, or an already-merged PR. On a merge
//! failure the CLI gathers the PR's mergeability detail and translates it into
//! one specific, actionable message.
//!
//! All gathering is best-effort: a permission error on branch-protection (a
//! common non-admin case) must not mask the diagnosis, so each fetch degrades
//! to "unknown" rather than aborting.

use crate::api;
use crate::api::pull::Pull;
use crate::cli::pr_preflight::{Preflight, Protection};

/// Strip a leading `refs/heads/` so a PR base ref matches the bare branch name
/// the branch-protection endpoint expects.
pub(crate) fn bare_branch(r: &str) -> &str {
    r.strip_prefix("refs/heads/").unwrap_or(r)
}

/// The mergeability facts diagnosis reasons from. Assembled by
/// [`diagnose_merge_failure`]; consumed by the pure [`diagnose`].
#[derive(Debug, Default)]
pub struct MergeFacts {
    pub number: u64,
    /// Bare base branch name (e.g. `main`).
    pub base: String,
    /// `owner/name`, for the disabled-style message.
    pub repo: String,
    /// Requested merge style as a `--style` value (`merge`/`squash`/…).
    pub style: String,
    pub merged: bool,
    pub closed: bool,
    pub draft: bool,
    pub mergeable: bool,
    /// False when the repo is known to disable the requested style. Defaults to
    /// true so an unknown (unfetched) repo never yields a false "not enabled".
    pub style_allowed: bool,
    /// Enabled styles, for the hint. Empty when unknown.
    pub allowed_styles: Vec<String>,
    pub required_approvals: i64,
    pub approvals: i64,
    /// Required status checks that are not green, as `(context, state)`.
    /// `context` is the reported check's own name, or the required pattern
    /// with `state` `missing` when no reported check matched it.
    pub failing_checks: Vec<(String, String)>,
    /// What the base branch's rule is known to refuse this merge for, already
    /// worded, from the preflight any reader of the repo can run.
    pub rule_blocker: Option<String>,
    /// Branch protection blocks merging an out-of-date branch.
    pub block_on_outdated: bool,
    /// Raw merge rejection from Forgejo, when available.
    pub merge_error: Option<String>,
}

/// The single most actionable reason the merge was blocked, or `None` when
/// nothing specific was detected (the caller then falls back to the raw API
/// error). Ordered most-fundamental first: a conflict must be resolved before
/// checks or approvals matter.
pub fn diagnose(f: &MergeFacts) -> Option<String> {
    if f.merged {
        return Some(format!("PR #{} is already merged.", f.number));
    }
    if f.closed {
        return Some(format!(
            "PR #{} is closed; reopen it before merging.",
            f.number
        ));
    }
    if !f.style_allowed {
        let allowed = if f.allowed_styles.is_empty() {
            "none".to_string()
        } else {
            f.allowed_styles.join(", ")
        };
        return Some(format!(
            "merge style \"{}\" is not enabled for {}; allowed: {}.",
            f.style, f.repo, allowed
        ));
    }
    if f.draft {
        return Some(format!(
            "PR #{} is a draft; mark it ready for review first (fj pr ready {}).",
            f.number, f.number
        ));
    }
    if !f.mergeable {
        return Some(format!(
            "PR #{} has merge conflicts with {}; rebase or resolve the conflicts, then retry.",
            f.number, f.base
        ));
    }
    if let Some(blocker) = &f.rule_blocker {
        return Some(blocker.clone());
    }
    if let Some((ctx, state)) = f.failing_checks.first() {
        if state == "missing" && is_glob_pattern(ctx) {
            return Some(format!(
                "PR #{} is blocked by a required check: no reported check matches the required pattern \"{}\".",
                f.number, ctx
            ));
        }
        return Some(format!(
            "PR #{} is blocked by a required check: \"{}\" is not green (state: {}).",
            f.number, ctx, state
        ));
    }
    if f.required_approvals > f.approvals {
        return Some(format!(
            "PR #{} needs {} approving review(s) but has {}.",
            f.number, f.required_approvals, f.approvals
        ));
    }
    if let Some(message) = f.merge_error.as_deref() {
        if f.style == "fast-forward-only"
            && (message.contains("Not possible to fast-forward")
                || message.contains("can not be merged")
                || message.contains("cannot be merged"))
        {
            return Some(format!(
                "PR #{} is not a fast-forward onto {}; rebase the branch onto {}, wait for CI, then retry.",
                f.number, f.base, f.base
            ));
        }
    }
    if f.block_on_outdated {
        if f.style == "fast-forward-only" {
            return Some(format!(
                "PR #{} is not a fast-forward onto {}; rebase the branch onto {}, wait for CI, then retry.",
                f.number, f.base, f.base
            ));
        }
        return Some(format!(
            "PR #{} may be behind {}; run fj pr update-branch, wait for CI, then retry.",
            f.number, f.base
        ));
    }
    if let Some(message) = f.merge_error.as_deref() {
        if message.contains("PushRejected") {
            let mut out = format!(
                "merge style \"{}\" was rejected by the protected branch: {message}",
                f.style
            );
            if f.style == "merge" {
                out.push_str(
                    "\n  note: style \"merge\" places the PR branch's own commits on the base branch, so protected branches that require signed commits verify those contributor commits. style \"squash\" creates one server-side commit instead.",
                );
            }
            return Some(out);
        }
    }
    None
}

/// A short mergeability line for `fj pr view` / `fj pr checks`. Uses only the
/// already-fetched PR, so it adds no API calls. `None` for a merged PR (the
/// state pill already says so).
pub fn mergeable_line(pull: &Pull) -> Option<String> {
    if pull.merged {
        return None;
    }
    if pull.draft {
        return Some("Mergeable: no (draft)".to_string());
    }
    if pull.mergeable {
        return Some("Mergeable: yes".to_string());
    }
    Some(format!(
        "Mergeable: no (conflicts with {})",
        bare_branch(&pull.base.ref_)
    ))
}

/// Gather mergeability facts and return the most specific blocker reason.
/// Runs only after a merge attempt failed, so the extra reads are justified.
#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn diagnose_merge_failure(
    client: &crate::client::Client,
    owner: &str,
    name: &str,
    pull: &Pull,
    style: &str,
    merge_error: Option<&str>,
    preflight: Option<&Preflight>,
) -> Option<String> {
    let base = bare_branch(&pull.base.ref_).to_string();

    let mut facts = MergeFacts {
        number: pull.number,
        base: base.clone(),
        repo: format!("{owner}/{name}"),
        style: style.to_string(),
        merged: pull.merged,
        closed: pull.state != "open",
        draft: pull.draft,
        mergeable: pull.mergeable,
        style_allowed: true,
        merge_error: merge_error.map(str::to_string),
        ..Default::default()
    };

    // Repo merge-style policy.
    if let Ok(repo) = api::repo::get(client, owner, name).await {
        facts.style_allowed = repo.allows_style(style);
        facts.allowed_styles = repo
            .allowed_styles()
            .into_iter()
            .map(str::to_string)
            .collect();
    }

    // What the rule requires, as far as a reader of the repo can see it. This
    // is the part that works without admin rights.
    facts.rule_blocker = preflight.and_then(|p| p.blocker()).map(str::to_string);
    let rule_read = preflight.is_some_and(|p| !matches!(p.protection, Protection::Unknown(_)));
    let unprotected = preflight.is_some_and(|p| p.protection == Protection::Unprotected);

    // The full rule (admin-only; 404 when the base is unprotected). It is the
    // only source for the outdated-branch setting, and the fallback for checks
    // and approvals when the branch endpoint told us nothing. The rule is
    // keyed by its own name, which is a glob and not always the branch name.
    let rule_name = preflight.and_then(|p| p.rule_name()).unwrap_or(&base);
    if unprotected {
        // Nothing to read: there is no rule.
    } else if let Ok(prot) = api::protect::get(client, owner, name, rule_name).await {
        facts.block_on_outdated = prot.block_on_outdated_branch;

        if !rule_read {
            if prot.required_approvals > 0 {
                // An unreadable review list is not zero approvals.
                if let Some(n) = count_approvals(client, owner, name, pull.number).await {
                    facts.required_approvals = prot.required_approvals;
                    facts.approvals = n;
                }
            }
            if prot.enable_status_check && !prot.status_check_contexts.is_empty() {
                facts.failing_checks = failing_required(
                    client,
                    owner,
                    name,
                    &pull.head.sha,
                    &prot.status_check_contexts,
                )
                .await;
            }
        }
    }

    diagnose(&facts)
}

/// Count distinct reviewers whose latest review is an approval.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn count_approvals(
    client: &crate::client::Client,
    owner: &str,
    name: &str,
    number: u64,
) -> Option<i64> {
    let reviews = api::pull::list_reviews(client, owner, name, number)
        .await
        .ok()?;
    // Reviews arrive oldest-first; last write per user wins.
    let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for r in reviews {
        let state = r.state.to_ascii_uppercase();
        // PENDING / COMMENT do not change an approval verdict.
        if state == "APPROVED" || state == "REQUEST_CHANGES" || state == "DISMISSED" {
            latest.insert(r.user.login, state);
        }
    }
    Some(latest.values().filter(|s| *s == "APPROVED").count() as i64)
}

/// Required status checks that are not green on `sha`, as `(context, state)`.
/// See [`required_check_failures`].
#[cfg_attr(coverage_nightly, coverage(off))]
async fn failing_required(
    client: &crate::client::Client,
    owner: &str,
    name: &str,
    sha: &str,
    required: &[String],
) -> Vec<(String, String)> {
    let cs = match api::pull::combined_status(client, owner, name, sha).await {
        Ok(cs) => cs,
        Err(_) => return Vec::new(),
    };
    required_check_failures(required, &cs.statuses)
}

/// Evaluate branch protection's `status_check_contexts` against the statuses
/// reported on a commit, the way Forgejo's merge gate does
/// (`MergeRequiredContextsCommitStatus` in `services/pull/commit_status.go`).
///
/// Each required context is a glob pattern, not a literal name: `*` requires
/// every reported context to pass and `CI / *` every context with that prefix.
/// A pattern passes when it matches at least one reported status and every
/// status it matches is `success` or `skipped` (Forgejo ranks `skipped` above
/// `success`, so it never lowers the verdict). A pattern that matches nothing
/// is reported as `(pattern, "missing")`; a matched status that is not green
/// is reported under its own context name, so the message names the real
/// check. A pattern that is not a valid glob is ignored, as Forgejo drops it.
pub fn required_check_failures(
    required: &[String],
    statuses: &[api::pull::CommitStatus],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for pattern in required {
        let Some(glob) = Glob::compile(pattern) else {
            continue;
        };
        let mut matched = false;
        for s in statuses.iter().filter(|s| glob.matches(&s.context)) {
            matched = true;
            let passing = matches!(s.status.as_str(), "success" | "skipped");
            if !passing && !out.iter().any(|(ctx, _)| *ctx == s.context) {
                out.push((s.context.clone(), s.status.clone()));
            }
        }
        if !matched {
            out.push((pattern.clone(), "missing".to_string()));
        }
    }
    out
}

/// True when a required context uses glob syntax rather than naming a check.
pub(crate) fn is_glob_pattern(p: &str) -> bool {
    p.contains(['*', '?', '[', '{'])
}

/// A compiled glob with the syntax Forgejo's `gobwas/glob` accepts for status
/// contexts (compiled without separators): `*` and `**` match any run of
/// characters, `?` one character, `[abc]` / `[a-z]` / `[!abc]` a class,
/// `{a,b}` alternatives, and `\` escapes the next character.
#[derive(Debug, Clone)]
pub(crate) struct Glob(Vec<Tok>);

#[derive(Debug, Clone)]
enum Tok {
    Lit(char),
    Any,
    One,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Alt(Vec<Glob>),
}

impl Glob {
    /// `None` when the pattern is malformed (an unclosed `[` or `{`).
    pub(crate) fn compile(pattern: &str) -> Option<Glob> {
        let chars: Vec<char> = pattern.chars().collect();
        let (glob, rest) = Self::parse(&chars, false)?;
        rest.is_empty().then_some(glob)
    }

    /// Parse until the end, or (inside `{}`) until an unescaped `,` or `}`,
    /// which is left in the returned remainder for the caller.
    fn parse(mut s: &[char], in_alt: bool) -> Option<(Glob, &[char])> {
        let mut toks = Vec::new();
        while let Some((&c, rest)) = s.split_first() {
            if in_alt && (c == ',' || c == '}') {
                break;
            }
            s = rest;
            match c {
                '*' => {
                    if !matches!(toks.last(), Some(Tok::Any)) {
                        toks.push(Tok::Any);
                    }
                }
                '?' => toks.push(Tok::One),
                '\\' => {
                    let (&e, rest) = s.split_first()?;
                    toks.push(Tok::Lit(e));
                    s = rest;
                }
                '[' => {
                    let (negated, mut body) = match s.split_first() {
                        Some(('!', rest)) => (true, rest),
                        _ => (false, s),
                    };
                    let mut ranges = Vec::new();
                    loop {
                        let (&lo, rest) = body.split_first()?;
                        if lo == ']' {
                            s = rest;
                            break;
                        }
                        body = rest;
                        if let ['-', hi, rest @ ..] = body {
                            if *hi != ']' {
                                ranges.push((lo, *hi));
                                body = rest;
                                continue;
                            }
                        }
                        ranges.push((lo, lo));
                    }
                    toks.push(Tok::Class { negated, ranges });
                }
                '{' => {
                    let mut alts = Vec::new();
                    loop {
                        let (alt, rest) = Self::parse(s, true)?;
                        alts.push(alt);
                        let (&sep, rest) = rest.split_first()?;
                        s = rest;
                        if sep == '}' {
                            break;
                        }
                    }
                    toks.push(Tok::Alt(alts));
                }
                _ => toks.push(Tok::Lit(c)),
            }
        }
        Some((Glob(toks), s))
    }

    pub(crate) fn matches(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        Self::match_from(&self.0, &chars)
    }

    fn match_from(toks: &[Tok], text: &[char]) -> bool {
        let Some((tok, rest)) = toks.split_first() else {
            return text.is_empty();
        };
        match tok {
            Tok::Lit(c) => text.first() == Some(c) && Self::match_from(rest, &text[1..]),
            Tok::One => !text.is_empty() && Self::match_from(rest, &text[1..]),
            Tok::Class { negated, ranges } => match text.first() {
                Some(&c) => {
                    let hit = ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
                    hit != *negated && Self::match_from(rest, &text[1..])
                }
                None => false,
            },
            Tok::Any => (0..=text.len()).any(|i| Self::match_from(rest, &text[i..])),
            Tok::Alt(alts) => alts.iter().any(|alt| {
                // Splice the alternative in front of the remaining tokens by
                // trying every split point it could consume.
                (0..=text.len()).any(|i| {
                    Self::match_from(&alt.0, &text[..i]) && Self::match_from(rest, &text[i..])
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_facts() -> MergeFacts {
        MergeFacts {
            number: 12,
            base: "main".into(),
            repo: "o/r".into(),
            style: "merge".into(),
            mergeable: true,
            style_allowed: true,
            ..Default::default()
        }
    }

    #[test]
    fn already_merged_wins() {
        let mut f = base_facts();
        f.merged = true;
        // even with other blockers present, "merged" is reported first
        f.mergeable = false;
        assert_eq!(diagnose(&f).unwrap(), "PR #12 is already merged.");
    }

    #[test]
    fn closed_reported() {
        let mut f = base_facts();
        f.closed = true;
        assert!(diagnose(&f).unwrap().contains("is closed"));
    }

    #[test]
    fn disabled_style_lists_allowed() {
        let mut f = base_facts();
        f.style = "merge".into();
        f.style_allowed = false;
        f.allowed_styles = vec!["squash".into()];
        let msg = diagnose(&f).unwrap();
        assert!(msg.contains("merge style \"merge\" is not enabled for o/r"));
        assert!(msg.contains("allowed: squash"));
    }

    #[test]
    fn disabled_style_with_none_allowed() {
        let mut f = base_facts();
        f.style_allowed = false;
        f.allowed_styles = vec![];
        assert!(diagnose(&f).unwrap().contains("allowed: none"));
    }

    #[test]
    fn draft_before_conflicts() {
        let mut f = base_facts();
        f.draft = true;
        f.mergeable = false;
        let msg = diagnose(&f).unwrap();
        assert!(msg.contains("is a draft"));
        assert!(msg.contains("fj pr ready 12"));
    }

    #[test]
    fn conflicts_named_with_base() {
        let mut f = base_facts();
        f.mergeable = false;
        let msg = diagnose(&f).unwrap();
        assert!(msg.contains("merge conflicts with main"));
    }

    #[test]
    fn failing_required_check_named() {
        let mut f = base_facts();
        f.failing_checks = vec![("ci/test".into(), "pending".into())];
        let msg = diagnose(&f).unwrap();
        assert!(msg.contains("required check"));
        assert!(msg.contains("ci/test"));
        assert!(msg.contains("pending"));
    }

    #[test]
    fn missing_approvals_counted() {
        let mut f = base_facts();
        f.required_approvals = 2;
        f.approvals = 1;
        assert_eq!(
            diagnose(&f).unwrap(),
            "PR #12 needs 2 approving review(s) but has 1."
        );
    }

    #[test]
    fn outdated_branch_last_resort() {
        let mut f = base_facts();
        f.block_on_outdated = true;
        assert!(diagnose(&f).unwrap().contains("may be behind main"));
    }

    #[test]
    fn fast_forward_only_rejection_names_non_fast_forward() {
        let mut f = base_facts();
        f.style = "fast-forward-only".into();
        f.merge_error = Some("405 Method Not Allowed: The pull request can not be merged".into());
        let msg = diagnose(&f).unwrap();
        assert!(msg.contains("not a fast-forward onto main"), "got: {msg}");
        assert!(msg.contains("wait for CI"), "got: {msg}");
    }

    #[test]
    fn push_rejected_names_merge_style() {
        let mut f = base_facts();
        f.style = "merge".into();
        f.merge_error = Some(
            "conflict: PushRejected with remote message: Forgejo: branch develop is protected from unverified commit abc123"
                .into(),
        );
        let msg = diagnose(&f).unwrap();
        assert!(
            msg.contains("merge style \"merge\" was rejected"),
            "got: {msg}"
        );
        assert!(
            msg.contains("protected from unverified commit"),
            "got: {msg}"
        );
        assert!(
            msg.contains("style \"squash\" creates one server-side commit"),
            "got: {msg}"
        );
    }

    #[test]
    fn nothing_specific_is_none() {
        assert!(diagnose(&base_facts()).is_none());
    }

    fn st(context: &str, status: &str) -> api::pull::CommitStatus {
        api::pull::CommitStatus {
            status: status.into(),
            context: context.into(),
            description: String::new(),
            target_url: String::new(),
            updated_at: None,
        }
    }

    fn req(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn star_with_all_green_is_mergeable() {
        let statuses = vec![
            st("CI / build (pull_request)", "success"),
            st("CI / test (pull_request)", "success"),
            st("lint / fmt (pull_request)", "success"),
        ];
        assert!(required_check_failures(&req(&["*"]), &statuses).is_empty());
        // Nothing else blocks, so diagnosis falls through to the raw error.
        let mut f = base_facts();
        f.failing_checks = required_check_failures(&req(&["*"]), &statuses);
        assert!(diagnose(&f).is_none());
    }

    #[test]
    fn star_treats_skipped_as_passing() {
        // Forgejo ranks `skipped` above `success`, so it never blocks.
        let statuses = vec![
            st("CI / build", "success"),
            st("lint / develop-superset-of-main", "skipped"),
        ];
        assert!(required_check_failures(&req(&["*"]), &statuses).is_empty());
    }

    #[test]
    fn star_with_one_failing_names_the_real_check() {
        let statuses = vec![
            st("CI / build", "success"),
            st("CI / test", "failure"),
            st("lint", "success"),
        ];
        let failing = required_check_failures(&req(&["*"]), &statuses);
        assert_eq!(
            failing,
            vec![("CI / test".to_string(), "failure".to_string())]
        );
        let mut f = base_facts();
        f.failing_checks = failing;
        assert_eq!(
            diagnose(&f).unwrap(),
            "PR #12 is blocked by a required check: \"CI / test\" is not green (state: failure)."
        );
    }

    #[test]
    fn star_blocks_on_pending_and_warning() {
        let statuses = vec![st("a", "pending"), st("b", "warning"), st("c", "success")];
        assert_eq!(
            required_check_failures(&req(&["*"]), &statuses),
            vec![
                ("a".to_string(), "pending".to_string()),
                ("b".to_string(), "warning".to_string()),
            ]
        );
    }

    #[test]
    fn star_with_no_statuses_is_missing() {
        let failing = required_check_failures(&req(&["*"]), &[]);
        assert_eq!(failing, vec![("*".to_string(), "missing".to_string())]);
        let mut f = base_facts();
        f.failing_checks = failing;
        assert_eq!(
            diagnose(&f).unwrap(),
            "PR #12 is blocked by a required check: no reported check matches the required pattern \"*\"."
        );
    }

    #[test]
    fn prefix_pattern_only_gates_its_prefix() {
        let statuses = vec![
            st("CI / build", "success"),
            st("CI / test", "success"),
            st("advisory / forseti", "failure"),
        ];
        assert!(required_check_failures(&req(&["CI / *"]), &statuses).is_empty());

        let statuses = vec![st("CI / build", "success"), st("CI / test", "error")];
        assert_eq!(
            required_check_failures(&req(&["CI / *"]), &statuses),
            vec![("CI / test".to_string(), "error".to_string())]
        );

        let statuses = vec![st("lint", "success")];
        assert_eq!(
            required_check_failures(&req(&["CI / *"]), &statuses),
            vec![("CI / *".to_string(), "missing".to_string())]
        );
    }

    #[test]
    fn literal_context_unchanged() {
        let statuses = vec![st("ci/test", "success"), st("ci/lint", "failure")];
        assert!(required_check_failures(&req(&["ci/test"]), &statuses).is_empty());
        assert_eq!(
            required_check_failures(&req(&["ci/lint"]), &statuses),
            vec![("ci/lint".to_string(), "failure".to_string())]
        );
        let failing = required_check_failures(&req(&["ci/build"]), &statuses);
        assert_eq!(
            failing,
            vec![("ci/build".to_string(), "missing".to_string())]
        );
        // A literal name keeps the original wording.
        let mut f = base_facts();
        f.failing_checks = failing;
        assert_eq!(
            diagnose(&f).unwrap(),
            "PR #12 is blocked by a required check: \"ci/build\" is not green (state: missing)."
        );
    }

    #[test]
    fn overlapping_patterns_report_a_check_once() {
        let statuses = vec![st("CI / test", "failure")];
        assert_eq!(
            required_check_failures(&req(&["*", "CI / *", "CI / test"]), &statuses),
            vec![("CI / test".to_string(), "failure".to_string())]
        );
    }

    #[test]
    fn invalid_pattern_is_ignored() {
        let statuses = vec![st("ci", "failure")];
        assert!(required_check_failures(&req(&["ci["]), &statuses).is_empty());
    }

    #[test]
    fn glob_syntax() {
        let m = |p: &str, t: &str| Glob::compile(p).unwrap().matches(t);
        assert!(m("*", ""));
        assert!(m("*", "CI / build (pull_request)"));
        assert!(m("**", "a/b/c"));
        assert!(m("CI / *", "CI / build"));
        assert!(!m("CI / *", "lint / CI / build"));
        assert!(m("*(pull_request)", "CI / test (pull_request)"));
        assert!(!m("*(pull_request)", "CI / test (push)"));
        assert!(m("ci/te?t", "ci/test"));
        assert!(!m("ci/te?t", "ci/tet"));
        assert!(m("ci/[bt]est", "ci/test"));
        assert!(!m("ci/[!bt]est", "ci/test"));
        assert!(m("v[0-9]", "v7"));
        assert!(m("{CI,lint} / *", "lint / fmt"));
        assert!(!m("{CI,lint} / *", "Test / fmt"));
        assert!(m("a\\*b", "a*b"));
        assert!(!m("a\\*b", "axb"));
        assert!(m("ci/test", "ci/test"));
        assert!(!m("ci/test", "ci/test2"));
        assert!(Glob::compile("ci[").is_none());
        assert!(Glob::compile("{a,b").is_none());
        assert!(is_glob_pattern("*"));
        assert!(!is_glob_pattern("ci/test"));
    }

    #[test]
    fn checks_take_precedence_over_approvals() {
        let mut f = base_facts();
        f.failing_checks = vec![("ci".into(), "failure".into())];
        f.required_approvals = 2;
        f.approvals = 0;
        assert!(diagnose(&f).unwrap().contains("required check"));
    }
}
