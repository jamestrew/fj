//! Merge preflight: what the base branch's rule is known to require of a pull
//! request, worked out from what this user can read.
//!
//! `GET /repos/{o}/{r}/branches/{base}` needs only read access and carries the
//! merge permission, the required status contexts and the required approvals.
//! It does not carry the rest of the rule (rejected reviews, outdated branch,
//! protected files, signed commits), so those surface only as the server's
//! refusal. Each gate here is one of four things: not required, passing,
//! blocked, or unknown. A gate is blocked only when the server is certain to
//! refuse (fj never sends `force_merge`, so an admin bypass does not apply);
//! anything unread or read incompletely stays unknown and is never shown as
//! passing.

use crate::api;
use crate::api::protect::EffectiveProtection;
use crate::api::pull::{CommitStatus, Pull, Review, ReviewScan, StatusScan};
use crate::cli::pr_merge_check::{bare_branch, is_glob_pattern, Glob};
use crate::client::ApiError;

/// What one required context pattern amounts to on the head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Success,
    Pending,
    Failure,
    /// The full status set was read and nothing matched: the check has not
    /// reported. Forgejo treats that as pending, so it is not a success.
    Missing,
    /// The status set could not be read fully, so the outcome is not known.
    Unknown,
}

impl CheckOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckOutcome::Success => "success",
            CheckOutcome::Pending => "pending",
            CheckOutcome::Failure => "failure",
            CheckOutcome::Missing => "missing",
            CheckOutcome::Unknown => "unknown",
        }
    }

    fn blocks(self) -> bool {
        matches!(
            self,
            CheckOutcome::Pending | CheckOutcome::Failure | CheckOutcome::Missing
        )
    }
}

/// One required context pattern with the reported checks it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCheck {
    /// The pattern from the branch rule. `*` when the rule requires status
    /// checks without naming any, which makes every reported check required.
    pub pattern: String,
    pub outcome: CheckOutcome,
    /// `(context, state)` of each reported check the pattern matched.
    pub matched: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    NotRequired,
    Pass,
    /// The server is certain to refuse; the text says why.
    Blocked(String),
    /// The evidence could not be read (or not fully); the text says what.
    Unknown(String),
}

impl Gate {
    fn state(&self) -> &'static str {
        match self {
            Gate::NotRequired => "not_required",
            Gate::Pass => "pass",
            Gate::Blocked(_) => "blocked",
            Gate::Unknown(_) => "unknown",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Gate::Blocked(s) | Gate::Unknown(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    Unprotected,
    /// Protected, with the rule name when the server reported one.
    Protected(Option<String>),
    Unknown(String),
}

/// The reported checks a gate is judged on, with whether the scan saw them all.
#[derive(Debug, Clone, Copy)]
pub struct StatusEvidence<'a> {
    pub statuses: &'a [CommitStatus],
    pub complete: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ReviewEvidence<'a> {
    pub reviews: &'a [Review],
    pub complete: bool,
}

#[derive(Debug, Clone)]
pub struct Preflight {
    pub number: u64,
    pub base: String,
    pub protection: Protection,
    pub permission: Gate,
    pub checks: Gate,
    pub approvals: Gate,
    pub required_checks: Vec<RequiredCheck>,
    pub required_approvals: Option<i64>,
    /// Approvals known to count (official, not dismissed, not stale).
    pub granted_approvals: Option<i64>,
    /// Compiled required patterns, for [`Preflight::is_required`]. `None`
    /// when it is not known which checks are required.
    required_globs: Option<Vec<Glob>>,
}

const FAILING: [&str; 3] = ["failure", "error", "warning"];

/// Judge one required pattern against the reported statuses, the way
/// Forgejo's `MergeRequiredContextsCommitStatus` does: the worst state among
/// the matches wins, `skipped` never lowers it, and a pattern that matches
/// nothing is pending. A failing or pending match is certain even from a
/// partial scan; a green or absent result is only trusted from a complete one
/// (a literal name is the exception: its one status was seen).
fn judge_pattern(pattern: &str, glob: &Glob, ev: StatusEvidence<'_>) -> RequiredCheck {
    let matched: Vec<(String, String)> = ev
        .statuses
        .iter()
        .filter(|s| glob.matches(&s.context))
        .map(|s| (s.context.clone(), s.status.to_ascii_lowercase()))
        .collect();
    let outcome = if matched.iter().any(|(_, s)| FAILING.contains(&s.as_str())) {
        CheckOutcome::Failure
    } else if matched.iter().any(|(_, s)| s == "pending") {
        CheckOutcome::Pending
    } else if matched
        .iter()
        .any(|(_, s)| !matches!(s.as_str(), "success" | "skipped"))
    {
        // A state this client does not know. Never assume it is green.
        CheckOutcome::Unknown
    } else if matched.is_empty() {
        if ev.complete {
            CheckOutcome::Missing
        } else {
            CheckOutcome::Unknown
        }
    } else if ev.complete || !is_glob_pattern(pattern) {
        CheckOutcome::Success
    } else {
        CheckOutcome::Unknown
    };
    RequiredCheck {
        pattern: pattern.to_string(),
        outcome,
        matched,
    }
}

/// The rule's patterns as compiled globs. A pattern that does not compile is
/// dropped, as the server drops it. A rule that requires status checks but
/// names none makes every reported check required, which is the pattern `*`.
fn required_patterns(contexts: &[String]) -> Vec<(String, Glob)> {
    let mut out: Vec<(String, Glob)> = Vec::new();
    for c in contexts {
        if out.iter().any(|(seen, _)| seen == c) {
            continue;
        }
        if let Some(g) = Glob::compile(c) {
            out.push((c.clone(), g));
        }
    }
    if out.is_empty() {
        if let Some(g) = Glob::compile("*") {
            out.push(("*".to_string(), g));
        }
    }
    out
}

fn check_blocker_text(number: u64, checks: &[RequiredCheck]) -> Option<String> {
    let c = checks.iter().find(|c| c.outcome.blocks())?;
    if c.outcome == CheckOutcome::Missing {
        return Some(if is_glob_pattern(&c.pattern) {
            format!(
                "PR #{number} is blocked by a required check: no reported check matches the required pattern \"{}\".",
                c.pattern
            )
        } else {
            format!(
                "PR #{number} is blocked by a required check: \"{}\" has not reported (state: missing).",
                c.pattern
            )
        });
    }
    // Name the real check, not the pattern that happened to match it.
    let (ctx, state) = c
        .matched
        .iter()
        .find(|(_, s)| FAILING.contains(&s.as_str()))
        .or_else(|| c.matched.iter().find(|(_, s)| s == "pending"))?;
    Some(format!(
        "PR #{number} is blocked by a required check: \"{ctx}\" is not green (state: {state})."
    ))
}

fn judge_checks(
    number: u64,
    contexts: &[String],
    statuses: Result<StatusEvidence<'_>, String>,
) -> (Gate, Vec<RequiredCheck>) {
    let patterns = required_patterns(contexts);
    let ev = match statuses {
        Ok(ev) => ev,
        Err(why) => {
            let checks = patterns
                .iter()
                .map(|(p, _)| RequiredCheck {
                    pattern: p.clone(),
                    outcome: CheckOutcome::Unknown,
                    matched: Vec::new(),
                })
                .collect();
            return (
                Gate::Unknown(format!("the head commit's checks could not be read: {why}")),
                checks,
            );
        }
    };
    let checks: Vec<RequiredCheck> = patterns
        .iter()
        .map(|(p, g)| judge_pattern(p, g, ev))
        .collect();
    let gate = if let Some(text) = check_blocker_text(number, &checks) {
        Gate::Blocked(text)
    } else if checks.iter().any(|c| c.outcome == CheckOutcome::Unknown) {
        Gate::Unknown(if ev.complete {
            "a required check reported a state fj does not recognize".to_string()
        } else {
            "the head commit's checks could not be read completely".to_string()
        })
    } else if contexts.is_empty()
        && !checks
            .iter()
            .any(|c| c.matched.iter().any(|(_, s)| s == "success"))
    {
        // No named contexts and nothing but skipped checks: Forgejo's combined
        // state is then `skipped`, which is not `success`.
        Gate::Unknown("every reported check was skipped; the server decides".to_string())
    } else {
        Gate::Pass
    };
    (gate, checks)
}

fn counts_as_approval(r: &Review) -> bool {
    r.official && !r.dismissed && r.state.eq_ignore_ascii_case("APPROVED")
}

/// Approvals as `(gate, granted)`. Only official, undismissed approvals count
/// (`GetGrantedApprovalsCount`). Whether a stale approval counts depends on
/// `ignore_stale_approvals`, which this endpoint does not expose, so a total
/// that only reaches the requirement with stale approvals is unknown.
fn judge_approvals(
    number: u64,
    required: i64,
    reviews: Result<ReviewEvidence<'_>, String>,
) -> (Gate, Option<i64>) {
    let ev = match reviews {
        Ok(ev) => ev,
        Err(why) => {
            return (
                Gate::Unknown(format!("reviews could not be read: {why}")),
                None,
            )
        }
    };
    let fresh = ev
        .reviews
        .iter()
        .filter(|r| counts_as_approval(r) && !r.stale)
        .count() as i64;
    let all = ev.reviews.iter().filter(|r| counts_as_approval(r)).count() as i64;
    if fresh >= required {
        return (Gate::Pass, Some(fresh));
    }
    if !ev.complete {
        return (
            Gate::Unknown("the review list could not be read completely".to_string()),
            None,
        );
    }
    if all < required {
        return (
            Gate::Blocked(format!(
                "PR #{number} needs {required} approving review(s) but has {all}."
            )),
            Some(all),
        );
    }
    (
        Gate::Unknown(format!(
            "{fresh} of {required} required approval(s) are current and {} more are stale; whether stale approvals count is not readable here",
            all - fresh
        )),
        Some(fresh),
    )
}

/// Build the preflight from whatever could be read. `rule` is the base
/// branch's effective protection, or why it could not be read. `statuses` and
/// `reviews` are consulted only when the rule requires them.
pub fn evaluate(
    number: u64,
    base: &str,
    rule: Result<&EffectiveProtection, String>,
    statuses: Result<StatusEvidence<'_>, String>,
    reviews: Result<ReviewEvidence<'_>, String>,
) -> Preflight {
    let mut out = Preflight {
        number,
        base: base.to_string(),
        protection: Protection::Unprotected,
        permission: Gate::NotRequired,
        checks: Gate::NotRequired,
        approvals: Gate::NotRequired,
        required_checks: Vec::new(),
        required_approvals: None,
        granted_approvals: None,
        required_globs: None,
    };
    let rule = match rule {
        Ok(r) => r,
        Err(why) => {
            let why = format!("the branch rule for {base} could not be read: {why}");
            out.protection = Protection::Unknown(why.clone());
            out.permission = Gate::Unknown(why.clone());
            out.checks = Gate::Unknown(why.clone());
            out.approvals = Gate::Unknown(why);
            return out;
        }
    };
    out.permission = match rule.user_can_merge {
        Some(true) => Gate::Pass,
        Some(false) => Gate::Blocked(format!(
            "you are not allowed to merge PR #{number} into {base} (the branch's merge permission excludes this account)."
        )),
        None => Gate::Unknown("the server did not report whether you may merge".to_string()),
    };
    match rule.protected {
        Some(false) => {
            out.required_globs = Some(Vec::new());
            out.required_approvals = Some(0);
            return out;
        }
        None => {
            let why = format!("the server did not report whether {base} is protected");
            out.protection = Protection::Unknown(why.clone());
            out.checks = Gate::Unknown(why.clone());
            out.approvals = Gate::Unknown(why);
            return out;
        }
        Some(true) => {}
    }
    out.protection = Protection::Protected(
        rule.effective_branch_protection_name
            .clone()
            .filter(|n| !n.is_empty()),
    );

    match rule.enable_status_check {
        Some(false) => out.required_globs = Some(Vec::new()),
        None => {
            out.checks = Gate::Unknown(
                "the server did not report whether status checks are required".to_string(),
            );
        }
        Some(true) => {
            let contexts = rule.status_check_contexts.clone().unwrap_or_default();
            let (gate, checks) = judge_checks(number, &contexts, statuses);
            out.checks = gate;
            out.required_checks = checks;
            out.required_globs = Some(
                required_patterns(&contexts)
                    .into_iter()
                    .map(|(_, g)| g)
                    .collect(),
            );
        }
    }

    out.required_approvals = rule.required_approvals;
    match rule.required_approvals {
        None => {
            out.approvals =
                Gate::Unknown("the server did not report the required approvals".to_string());
        }
        Some(n) if n <= 0 => {}
        Some(n) => {
            let (gate, granted) = judge_approvals(number, n, reviews);
            out.approvals = gate;
            out.granted_approvals = granted;
        }
    }
    out
}

impl Preflight {
    /// Whether a reported check is required by the base rule. `None` when the
    /// rule could not be read, so a caller shows it as unknown, not optional.
    pub fn is_required(&self, context: &str) -> Option<bool> {
        self.required_globs
            .as_ref()
            .map(|globs| globs.iter().any(|g| g.matches(context)))
    }

    /// The first thing certain to make the server refuse, in the server's own
    /// order (`CheckPullMergeable`): merge permission, status checks, approvals.
    pub fn blocker(&self) -> Option<&str> {
        [&self.permission, &self.checks, &self.approvals]
            .into_iter()
            .find_map(|g| match g {
                Gate::Blocked(s) => Some(s.as_str()),
                _ => None,
            })
    }

    /// A blocker worth refusing a merge for before it is sent. With `--auto`
    /// only the merge permission counts: the server schedules the merge and
    /// waits for checks, which is the point of the flag.
    pub fn pre_merge_blocker(&self, auto: bool) -> Option<String> {
        if let Gate::Blocked(s) = &self.permission {
            return Some(s.clone());
        }
        if auto {
            return None;
        }
        let reason = self.blocker()?;
        let waits = matches!(self.checks, Gate::Blocked(_))
            && !self
                .required_checks
                .iter()
                .any(|c| c.outcome == CheckOutcome::Failure);
        let mut out = format!(
            "{reason}\n  note: the merge was not attempted; {} would refuse it.",
            self.rule_label()
        );
        if waits {
            out.push_str(&format!(
                "\n  To merge once the required checks pass: fj pr merge {} --auto",
                self.number
            ));
        }
        Some(out)
    }

    /// The name of the rule protecting the base branch, when the server gave
    /// one. A rule is keyed by this name, which may be a glob such as
    /// `release/*` rather than the branch itself.
    pub fn rule_name(&self) -> Option<&str> {
        match &self.protection {
            Protection::Protected(Some(name)) => Some(name),
            _ => None,
        }
    }

    /// A note for a refusal fj could not pin on anything: what was left
    /// unread, or, when everything readable passed, what cannot be read at
    /// all. `None` on an unprotected base, where no rule is involved.
    pub fn refusal_note(&self) -> Option<String> {
        let unknowns = self.unknowns();
        if !unknowns.is_empty() {
            return Some(format!(
                "fj could not check everything {} may require: {}.",
                self.rule_label(),
                unknowns.join("; ")
            ));
        }
        match self.protection {
            Protection::Protected(_) => Some(format!(
                "the required checks and approvals fj can read are satisfied. {} can also refuse a merge for rejected reviews, an outdated branch, protected files or unsigned commits, none of which are readable here.",
                self.rule_label()
            )),
            _ => None,
        }
    }

    fn rule_label(&self) -> String {
        match &self.protection {
            Protection::Protected(Some(name)) => {
                format!("the branch rule \"{name}\" on {}", self.base)
            }
            _ => format!("the branch rule on {}", self.base),
        }
    }

    /// The distinct pieces of evidence that could not be read, for telling a
    /// user why a refusal has no specific explanation.
    pub fn unknowns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in [&self.permission, &self.checks, &self.approvals] {
            if let Gate::Unknown(s) = g {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    /// Human lines for `fj pr checks`. Never says the merge is ready: a gate
    /// that passes here is one of several, and the server has the last word.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.protection {
            Protection::Unknown(why) => {
                out.push(format!("Branch rule: unknown ({why})"));
                out.push(
                    "Required checks and approvals are unknown; the server decides at merge time."
                        .to_string(),
                );
                return out;
            }
            Protection::Unprotected => {
                out.push(format!("Branch rule: {} is not protected", self.base));
            }
            Protection::Protected(name) => out.push(match name {
                Some(n) => format!("Branch rule: {} is protected by \"{n}\"", self.base),
                None => format!("Branch rule: {} is protected", self.base),
            }),
        }
        match &self.permission {
            Gate::Blocked(_) => out.push("Merge permission: not allowed for you".to_string()),
            Gate::Unknown(why) => out.push(format!("Merge permission: unknown ({why})")),
            _ => {}
        }
        if self.protection == Protection::Unprotected {
            return out;
        }
        out.push(match &self.checks {
            Gate::NotRequired => "Required checks: none required".to_string(),
            Gate::Pass => format!(
                "Required checks: passing ({})",
                self.required_checks
                    .iter()
                    .map(|c| c.pattern.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Gate::Blocked(_) => {
                let bad: Vec<String> = self
                    .required_checks
                    .iter()
                    .filter(|c| c.outcome.blocks())
                    .map(|c| format!("{} ({})", c.pattern, c.outcome.as_str()))
                    .collect();
                format!("Required checks: blocked: {}", bad.join(", "))
            }
            Gate::Unknown(why) => format!("Required checks: unknown ({why})"),
        });
        out.push(match &self.approvals {
            Gate::NotRequired => "Required approvals: none required".to_string(),
            Gate::Pass => format!(
                "Required approvals: {} of {}",
                self.granted_approvals.unwrap_or_default(),
                self.required_approvals.unwrap_or_default()
            ),
            Gate::Blocked(_) => format!(
                "Required approvals: blocked: {} of {}",
                self.granted_approvals.unwrap_or_default(),
                self.required_approvals.unwrap_or_default()
            ),
            Gate::Unknown(why) => format!("Required approvals: unknown ({why})"),
        });
        out.push(
            "Not readable here: rejected reviews, outdated branch, protected files, signed commits. The server decides those at merge time."
                .to_string(),
        );
        out
    }

    /// Required patterns that no reported check matched, as extra table rows
    /// for `fj pr checks` so an absent required check is visible.
    pub fn unreported(&self) -> Vec<&RequiredCheck> {
        self.required_checks
            .iter()
            .filter(|c| c.matched.is_empty())
            .collect()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let gate = |g: &Gate| serde_json::json!({ "state": g.state(), "detail": g.detail() });
        let (protection, rule_name, protection_detail) = match &self.protection {
            Protection::Unprotected => ("unprotected", None, None),
            Protection::Protected(name) => ("protected", name.clone(), None),
            Protection::Unknown(why) => ("unknown", None, Some(why.clone())),
        };
        // Deliberately no "ready" value: fj reads only part of the rule.
        let verdict = if self.blocker().is_some() {
            "blocked"
        } else if self.unknowns().is_empty() {
            "no_known_blocker"
        } else {
            "unknown"
        };
        let mut approvals = gate(&self.approvals);
        approvals["required"] = serde_json::json!(self.required_approvals);
        approvals["granted"] = serde_json::json!(self.granted_approvals);
        serde_json::json!({
            "base": self.base,
            "protection": protection,
            "protection_detail": protection_detail,
            "rule_name": rule_name,
            "permission": gate(&self.permission),
            "checks": gate(&self.checks),
            "approvals": approvals,
            "required_checks": self.required_checks.iter().map(|c| serde_json::json!({
                "pattern": c.pattern,
                "outcome": c.outcome.as_str(),
                "matched": c.matched.iter().map(|(context, status)| serde_json::json!({
                    "context": context,
                    "status": status,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "verdict": verdict,
        })
    }
}

/// The REQUIRED cell for a reported check in `fj pr checks`.
pub fn required_cell(required: Option<bool>) -> &'static str {
    match required {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    }
}

/// `fj pr checks --json`: the combined status, each status tagged with
/// whether it is required (`null` when unknown), plus the preflight.
pub fn checks_json(scan: &StatusScan, preflight: &Preflight) -> serde_json::Value {
    let mut v = serde_json::to_value(&scan.combined).unwrap_or_default();
    if let Some(list) = v.get_mut("statuses").and_then(|s| s.as_array_mut()) {
        for (item, s) in list.iter_mut().zip(&scan.combined.statuses) {
            item["required"] = serde_json::json!(preflight.is_required(&s.context));
        }
    }
    v["statuses_complete"] = serde_json::json!(scan.complete);
    v["merge_preflight"] = preflight.to_json();
    v
}

/// Why a read failed, short enough to sit inside a sentence. A permission
/// denial and a missing resource are named apart from other failures.
pub fn read_failure(e: &anyhow::Error) -> String {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.status == reqwest::StatusCode::FORBIDDEN => {
            "permission denied (HTTP 403)".to_string()
        }
        Some(api) if api.status == reqwest::StatusCode::UNAUTHORIZED => {
            "not authenticated (HTTP 401)".to_string()
        }
        Some(api) if api.status == reqwest::StatusCode::NOT_FOUND => {
            "not found (HTTP 404)".to_string()
        }
        Some(api) => format!("HTTP {}", api.status.as_u16()),
        None => "the request failed".to_string(),
    }
}

/// Read the base branch's effective protection and only the evidence it calls
/// for, then judge it. Every read is best-effort: a failure becomes an unknown
/// gate, never an error and never a pass. Pass `statuses` when the caller has
/// already scanned the head commit, to avoid reading it twice.
#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn gather(
    client: &crate::client::Client,
    owner: &str,
    name: &str,
    pull: &Pull,
    statuses: Option<&StatusScan>,
) -> Preflight {
    let base = bare_branch(&pull.base.ref_).to_string();
    let rule = api::protect::effective(client, owner, name, &base)
        .await
        .map_err(|e| read_failure(&e));
    let protected = rule
        .as_ref()
        .is_ok_and(|r: &EffectiveProtection| r.protected == Some(true));
    let wants_checks = protected
        && rule
            .as_ref()
            .is_ok_and(|r| r.enable_status_check == Some(true));
    let wants_reviews = protected
        && rule
            .as_ref()
            .is_ok_and(|r| r.required_approvals.is_some_and(|n| n > 0));

    let fetched_statuses: Option<Result<StatusScan, String>> = if wants_checks && statuses.is_none()
    {
        Some(
            api::pull::combined_status_all(client, owner, name, &pull.head.sha)
                .await
                .map_err(|e| read_failure(&e)),
        )
    } else {
        None
    };
    let status_ev: Result<StatusEvidence<'_>, String> = match (statuses, &fetched_statuses) {
        (Some(scan), _) | (None, Some(Ok(scan))) => Ok(StatusEvidence {
            statuses: &scan.combined.statuses,
            complete: scan.complete,
        }),
        (None, Some(Err(why))) => Err(why.clone()),
        (None, None) => Err("not read".to_string()),
    };

    let fetched_reviews: Option<Result<ReviewScan, String>> = if wants_reviews {
        Some(
            api::pull::list_reviews_all(client, owner, name, pull.number)
                .await
                .map_err(|e| read_failure(&e)),
        )
    } else {
        None
    };
    let review_ev: Result<ReviewEvidence<'_>, String> = match &fetched_reviews {
        Some(Ok(scan)) => Ok(ReviewEvidence {
            reviews: &scan.reviews,
            complete: scan.complete,
        }),
        Some(Err(why)) => Err(why.clone()),
        None => Err("not read".to_string()),
    };

    evaluate(
        pull.number,
        &base,
        rule.as_ref().map_err(String::clone),
        status_ev,
        review_ev,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(context: &str, status: &str) -> CommitStatus {
        CommitStatus {
            status: status.into(),
            context: context.into(),
            description: String::new(),
            target_url: String::new(),
            updated_at: None,
        }
    }

    fn review(id: u64, state: &str, official: bool, dismissed: bool, stale: bool) -> Review {
        serde_json::from_value(serde_json::json!({
            "id": id, "state": state, "user": { "id": id, "login": format!("u{id}") },
            "submitted_at": null, "html_url": "https://x/r",
            "official": official, "dismissed": dismissed, "stale": stale,
        }))
        .unwrap()
    }

    fn rule(contexts: &[&str], approvals: i64) -> EffectiveProtection {
        EffectiveProtection {
            protected: Some(true),
            effective_branch_protection_name: Some("main".into()),
            required_approvals: Some(approvals),
            enable_status_check: Some(!contexts.is_empty()),
            status_check_contexts: Some(contexts.iter().map(|c| c.to_string()).collect()),
            user_can_merge: Some(true),
            user_can_push: Some(true),
        }
    }

    fn run(
        rule: Result<&EffectiveProtection, String>,
        statuses: &[CommitStatus],
        statuses_complete: bool,
        reviews: &[Review],
        reviews_complete: bool,
    ) -> Preflight {
        evaluate(
            7,
            "main",
            rule,
            Ok(StatusEvidence {
                statuses,
                complete: statuses_complete,
            }),
            Ok(ReviewEvidence {
                reviews,
                complete: reviews_complete,
            }),
        )
    }

    #[test]
    fn required_and_optional_checks_are_told_apart() {
        let r = rule(&["CI / *"], 0);
        let statuses = [st("CI / build", "success"), st("advisory", "failure")];
        let p = run(Ok(&r), &statuses, true, &[], true);
        assert_eq!(p.is_required("CI / build"), Some(true));
        assert_eq!(p.is_required("advisory"), Some(false));
        // The failing check is optional, so it does not block.
        assert_eq!(p.checks, Gate::Pass);
        assert!(p.blocker().is_none());
        assert_eq!(required_cell(p.is_required("advisory")), "no");
    }

    #[test]
    fn failing_required_check_blocks_and_names_the_real_check() {
        let r = rule(&["CI / *"], 0);
        let statuses = [st("CI / build", "success"), st("CI / test", "failure")];
        let p = run(Ok(&r), &statuses, true, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Failure);
        assert_eq!(
            p.blocker().unwrap(),
            "PR #7 is blocked by a required check: \"CI / test\" is not green (state: failure)."
        );
        let pre = p.pre_merge_blocker(false).unwrap();
        assert!(pre.contains("the merge was not attempted"), "{pre}");
        assert!(pre.contains("the branch rule \"main\" on main"), "{pre}");
        // A failed check will not pass by waiting, so --auto is not suggested.
        assert!(!pre.contains("--auto"), "{pre}");
    }

    #[test]
    fn pending_required_check_blocks_and_suggests_auto() {
        let r = rule(&["ci"], 0);
        let p = run(Ok(&r), &[st("ci", "pending")], true, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Pending);
        let pre = p.pre_merge_blocker(false).unwrap();
        assert!(
            pre.contains("\"ci\" is not green (state: pending)"),
            "{pre}"
        );
        assert!(pre.contains("fj pr merge 7 --auto"), "{pre}");
        // --auto itself is what waits for checks, so it is not blocked.
        assert!(p.pre_merge_blocker(true).is_none());
    }

    #[test]
    fn required_check_that_has_not_reported_is_not_success() {
        let r = rule(&["ci", "lint / *"], 0);
        let p = run(Ok(&r), &[st("other", "success")], true, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Missing);
        assert_eq!(p.required_checks[1].outcome, CheckOutcome::Missing);
        assert!(matches!(p.checks, Gate::Blocked(_)));
        assert!(p
            .blocker()
            .unwrap()
            .contains("\"ci\" has not reported (state: missing)"));
        assert_eq!(p.unreported().len(), 2);
        // No statuses at all is the same story.
        let p = run(Ok(&r), &[], true, &[], true);
        assert!(matches!(p.checks, Gate::Blocked(_)));
        assert_eq!(p.to_json()["verdict"], "blocked");
    }

    #[test]
    fn incomplete_status_scan_is_unknown_not_passing_or_missing() {
        let r = rule(&["ci", "CI / *"], 0);
        // `ci` was not seen and `CI / *` looks green, but pages are missing.
        let p = run(Ok(&r), &[st("CI / build", "success")], false, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Unknown);
        assert_eq!(p.required_checks[1].outcome, CheckOutcome::Unknown);
        assert!(matches!(p.checks, Gate::Unknown(_)));
        assert!(p.blocker().is_none());
        assert!(p.pre_merge_blocker(false).is_none());
        assert_eq!(p.to_json()["verdict"], "unknown");
        assert!(p
            .summary_lines()
            .iter()
            .any(|l| l.starts_with("Required checks: unknown")));
    }

    #[test]
    fn incomplete_scan_still_reports_a_seen_failure_and_a_seen_literal() {
        let r = rule(&["ci"], 0);
        // A literal context has one status; seeing it green is conclusive.
        let p = run(Ok(&r), &[st("ci", "success")], false, &[], true);
        assert_eq!(p.checks, Gate::Pass);
        let r = rule(&["CI / *"], 0);
        let p = run(Ok(&r), &[st("CI / test", "error")], false, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Failure);
        assert!(matches!(p.checks, Gate::Blocked(_)));
    }

    #[test]
    fn unreadable_statuses_leave_checks_unknown() {
        let r = rule(&["ci"], 0);
        let p = evaluate(
            7,
            "main",
            Ok(&r),
            Err("permission denied (HTTP 403)".into()),
            Err("not read".into()),
        );
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Unknown);
        assert!(matches!(&p.checks, Gate::Unknown(w) if w.contains("permission denied")));
        // Which checks are required is still known.
        assert_eq!(p.is_required("ci"), Some(true));
    }

    #[test]
    fn unknown_status_state_is_not_assumed_green() {
        let r = rule(&["ci"], 0);
        let p = run(Ok(&r), &[st("ci", "queued")], true, &[], true);
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Unknown);
        assert!(matches!(p.checks, Gate::Unknown(_)));
    }

    #[test]
    fn skipped_and_warning_follow_forgejo() {
        let r = rule(&["a", "b"], 0);
        let p = run(
            Ok(&r),
            &[st("a", "skipped"), st("b", "success")],
            true,
            &[],
            true,
        );
        assert_eq!(p.checks, Gate::Pass);
        let p = run(
            Ok(&r),
            &[st("a", "warning"), st("b", "success")],
            true,
            &[],
            true,
        );
        assert_eq!(p.required_checks[0].outcome, CheckOutcome::Failure);
    }

    #[test]
    fn status_checks_without_named_contexts_require_every_check() {
        let mut r = rule(&[], 0);
        r.enable_status_check = Some(true);
        r.status_check_contexts = None;
        let p = run(
            Ok(&r),
            &[st("x", "success"), st("y", "failure")],
            true,
            &[],
            true,
        );
        assert_eq!(p.is_required("anything"), Some(true));
        assert!(matches!(p.checks, Gate::Blocked(_)));
        let p = run(Ok(&r), &[], true, &[], true);
        assert!(matches!(p.checks, Gate::Blocked(_)));
        let p = run(Ok(&r), &[st("x", "skipped")], true, &[], true);
        assert!(matches!(p.checks, Gate::Unknown(_)));
        let p = run(Ok(&r), &[st("x", "success")], true, &[], true);
        assert_eq!(p.checks, Gate::Pass);
    }

    #[test]
    fn unreadable_rule_leaves_every_gate_unknown() {
        let p = run(
            Err("permission denied (HTTP 403)".into()),
            &[st("ci", "success")],
            true,
            &[],
            true,
        );
        assert!(matches!(p.protection, Protection::Unknown(_)));
        assert!(matches!(p.checks, Gate::Unknown(_)));
        assert!(matches!(p.approvals, Gate::Unknown(_)));
        assert!(matches!(p.permission, Gate::Unknown(_)));
        // A green check is not called optional or required when unknown.
        assert_eq!(p.is_required("ci"), None);
        assert_eq!(required_cell(p.is_required("ci")), "?");
        assert!(p.blocker().is_none());
        assert_eq!(p.unknowns().len(), 1);
        let lines = p.summary_lines();
        assert!(lines[0].starts_with("Branch rule: unknown"), "{lines:?}");
        assert!(lines[0].contains("permission denied"), "{lines:?}");
        assert_eq!(p.to_json()["verdict"], "unknown");
        assert_eq!(p.to_json()["protection"], "unknown");
    }

    #[test]
    fn rule_fields_the_server_omitted_are_unknown() {
        // An older server that answers with only the branch name and commit.
        let r = EffectiveProtection::default();
        let p = run(Ok(&r), &[], true, &[], true);
        assert!(matches!(p.protection, Protection::Unknown(_)));
        assert!(matches!(p.permission, Gate::Unknown(_)));
        assert_eq!(p.is_required("ci"), None);

        let r = EffectiveProtection {
            protected: Some(true),
            ..Default::default()
        };
        let p = run(Ok(&r), &[], true, &[], true);
        assert!(matches!(p.checks, Gate::Unknown(_)));
        assert!(matches!(p.approvals, Gate::Unknown(_)));
    }

    #[test]
    fn unprotected_base_requires_nothing() {
        let r = EffectiveProtection {
            protected: Some(false),
            user_can_merge: Some(true),
            ..Default::default()
        };
        let p = run(Ok(&r), &[st("ci", "failure")], true, &[], true);
        assert_eq!(p.protection, Protection::Unprotected);
        assert_eq!(p.is_required("ci"), Some(false));
        assert!(p.blocker().is_none());
        assert_eq!(
            p.summary_lines(),
            vec!["Branch rule: main is not protected"]
        );
        assert_eq!(p.to_json()["verdict"], "no_known_blocker");
    }

    #[test]
    fn merge_permission_blocks_first_and_even_with_auto() {
        let mut r = rule(&["ci"], 1);
        r.user_can_merge = Some(false);
        let p = run(Ok(&r), &[st("ci", "failure")], true, &[], true);
        assert!(p.blocker().unwrap().contains("not allowed to merge"));
        assert!(p
            .pre_merge_blocker(true)
            .unwrap()
            .contains("not allowed to merge"));
        assert!(p
            .summary_lines()
            .contains(&"Merge permission: not allowed for you".to_string()));
    }

    #[test]
    fn approvals_count_only_official_undismissed() {
        let r = rule(&[], 2);
        let reviews = [
            review(1, "APPROVED", true, false, false),
            review(2, "APPROVED", false, false, false), // no write access
            review(3, "APPROVED", true, true, false),   // dismissed
            review(4, "COMMENT", true, false, false),
        ];
        let p = run(Ok(&r), &[], true, &reviews, true);
        assert_eq!(
            p.approvals,
            Gate::Blocked("PR #7 needs 2 approving review(s) but has 1.".into())
        );
        assert_eq!(p.granted_approvals, Some(1));

        let reviews = [
            review(1, "APPROVED", true, false, false),
            review(5, "approved", true, false, false),
        ];
        let p = run(Ok(&r), &[], true, &reviews, true);
        assert_eq!(p.approvals, Gate::Pass);
        assert!(p
            .summary_lines()
            .contains(&"Required approvals: 2 of 2".to_string()));
    }

    #[test]
    fn incomplete_or_stale_review_evidence_is_unknown() {
        let r = rule(&[], 2);
        let one = [review(1, "APPROVED", true, false, false)];
        // More pages might hold the second approval.
        let p = run(Ok(&r), &[], true, &one, false);
        assert!(matches!(p.approvals, Gate::Unknown(_)));
        assert!(p.blocker().is_none());
        // Enough current approvals in a partial list is still enough.
        let two = [
            review(1, "APPROVED", true, false, false),
            review(2, "APPROVED", true, false, false),
        ];
        assert_eq!(run(Ok(&r), &[], true, &two, false).approvals, Gate::Pass);
        // The requirement is only met by counting a stale approval.
        let stale = [
            review(1, "APPROVED", true, false, false),
            review(2, "APPROVED", true, false, true),
        ];
        let p = run(Ok(&r), &[], true, &stale, true);
        assert!(matches!(&p.approvals, Gate::Unknown(w) if w.contains("stale")));
        // Unreadable reviews.
        let p = evaluate(
            7,
            "main",
            Ok(&r),
            Err("not read".into()),
            Err("permission denied (HTTP 403)".into()),
        );
        assert!(matches!(&p.approvals, Gate::Unknown(w) if w.contains("permission denied")));
    }

    #[test]
    fn checks_block_before_approvals() {
        let r = rule(&["ci"], 1);
        let p = run(Ok(&r), &[st("ci", "failure")], true, &[], true);
        assert!(p.blocker().unwrap().contains("required check"));
        let p = run(Ok(&r), &[st("ci", "success")], true, &[], true);
        assert!(p.blocker().unwrap().contains("approving review"));
        // Approvals are not something --auto waits out, but the server
        // schedules regardless, so it is left to the server.
        assert!(p.pre_merge_blocker(true).is_none());
        assert!(!p.pre_merge_blocker(false).unwrap().contains("--auto"));
    }

    #[test]
    fn summary_never_claims_ready_and_names_what_is_unreadable() {
        let r = rule(&["ci"], 1);
        let reviews = [review(1, "APPROVED", true, false, false)];
        let p = run(Ok(&r), &[st("ci", "success")], true, &reviews, true);
        assert!(p.blocker().is_none());
        let lines = p.summary_lines();
        assert_eq!(lines[0], "Branch rule: main is protected by \"main\"");
        assert!(lines.contains(&"Required checks: passing (ci)".to_string()));
        assert!(lines.last().unwrap().starts_with("Not readable here:"));
        assert!(!lines.iter().any(|l| l.to_lowercase().contains("ready")));
        assert_eq!(p.to_json()["verdict"], "no_known_blocker");
    }

    #[test]
    fn refusal_note_names_what_was_unread_or_unreadable() {
        // Unknown evidence is listed.
        let p = run(
            Err("permission denied (HTTP 403)".into()),
            &[],
            true,
            &[],
            true,
        );
        let note = p.refusal_note().unwrap();
        assert!(note.contains("could not check everything"), "{note}");
        assert!(note.contains("permission denied"), "{note}");
        assert!(p.rule_name().is_none());
        // Everything readable passed: say what is not readable.
        let r = rule(&["ci"], 0);
        let p = run(Ok(&r), &[st("ci", "success")], true, &[], true);
        let note = p.refusal_note().unwrap();
        assert!(note.contains("outdated branch"), "{note}");
        assert!(note.contains("the branch rule \"main\" on main"), "{note}");
        assert_eq!(p.rule_name(), Some("main"));
        // No rule, no note.
        let r = EffectiveProtection {
            protected: Some(false),
            user_can_merge: Some(true),
            ..Default::default()
        };
        assert!(run(Ok(&r), &[], true, &[], true).refusal_note().is_none());
    }

    #[test]
    fn checks_json_tags_required_and_carries_the_preflight() {
        let r = rule(&["ci"], 0);
        let scan = StatusScan {
            combined: api::pull::CombinedStatus {
                state: "failure".into(),
                sha: "abc".into(),
                statuses: vec![st("ci", "success"), st("extra", "failure")],
                total_count: 2,
            },
            complete: true,
        };
        let p = run(Ok(&r), &scan.combined.statuses, true, &[], true);
        let v = checks_json(&scan, &p);
        assert_eq!(v["statuses"][0]["required"], true);
        assert_eq!(v["statuses"][1]["required"], false);
        assert_eq!(v["statuses_complete"], true);
        assert_eq!(v["merge_preflight"]["checks"]["state"], "pass");
        assert_eq!(
            v["merge_preflight"]["required_checks"][0]["outcome"],
            "success"
        );
        assert_eq!(v["state"], "failure");

        // Unknown rule: `required` is null, not false.
        let p = run(Err("x".into()), &scan.combined.statuses, true, &[], true);
        assert!(checks_json(&scan, &p)["statuses"][0]["required"].is_null());
    }

    #[test]
    fn read_failure_names_permission_denials_apart() {
        let api = |status: u16| {
            anyhow::Error::new(ApiError {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                url: "https://x/secret".into(),
                message: "m".into(),
                body: String::new(),
            })
        };
        assert_eq!(read_failure(&api(403)), "permission denied (HTTP 403)");
        assert_eq!(read_failure(&api(404)), "not found (HTTP 404)");
        assert_eq!(read_failure(&api(401)), "not authenticated (HTTP 401)");
        assert_eq!(read_failure(&api(500)), "HTTP 500");
        assert_eq!(read_failure(&anyhow::anyhow!("boom")), "the request failed");
    }
}
