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

/// Strip a leading `refs/heads/` so a PR base ref matches the bare branch name
/// the branch-protection endpoint expects.
fn bare_branch(r: &str) -> &str {
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
    /// Required status contexts that are not `success`, as `(context, state)`.
    /// `state` is `missing` when the context reported nothing.
    pub failing_checks: Vec<(String, String)>,
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
    if let Some((ctx, state)) = f.failing_checks.first() {
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

    // Branch protection (404 when the base is unprotected; permission errors
    // for non-admins). Either way, fall through to whatever else we learned.
    if let Ok(prot) = api::protect::get(client, owner, name, &base).await {
        facts.required_approvals = prot.required_approvals;
        facts.block_on_outdated = prot.block_on_outdated_branch;

        if prot.required_approvals > 0 {
            facts.approvals = count_approvals(client, owner, name, pull.number).await;
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

    diagnose(&facts)
}

/// Count distinct reviewers whose latest review is an approval.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn count_approvals(
    client: &crate::client::Client,
    owner: &str,
    name: &str,
    number: u64,
) -> i64 {
    let reviews = match api::pull::list_reviews(client, owner, name, number).await {
        Ok(r) => r,
        Err(_) => return 0,
    };
    // Reviews arrive oldest-first; last write per user wins.
    let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for r in reviews {
        let state = r.state.to_ascii_uppercase();
        // PENDING / COMMENT do not change an approval verdict.
        if state == "APPROVED" || state == "REQUEST_CHANGES" || state == "DISMISSED" {
            latest.insert(r.user.login, state);
        }
    }
    latest.values().filter(|s| *s == "APPROVED").count() as i64
}

/// Required status contexts that are not `success`, as `(context, state)`.
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
    let mut state_of: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for s in &cs.statuses {
        state_of.insert(s.context.as_str(), s.status.as_str());
    }
    required
        .iter()
        .filter_map(|ctx| {
            let state = state_of.get(ctx.as_str()).copied().unwrap_or("missing");
            if state == "success" {
                None
            } else {
                Some((ctx.clone(), state.to_string()))
            }
        })
        .collect()
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

    #[test]
    fn checks_take_precedence_over_approvals() {
        let mut f = base_facts();
        f.failing_checks = vec![("ci".into(), "failure".into())];
        f.required_approvals = 2;
        f.approvals = 0;
        assert!(diagnose(&f).unwrap().contains("required check"));
    }
}
