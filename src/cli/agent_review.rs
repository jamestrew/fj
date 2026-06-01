//! Local diff analysis for `fj agent review`.
//!
//! Everything here is pure over the strings git hands back (`--numstat`, the
//! unified diff), so it's fully unit-testable. The flow:
//!   1. [`build_context`] parses `git diff --numstat` into a [`ReviewContext`]:
//!      changed files with add/del counts, test-file and risky-area tagging,
//!      and roll-up totals.
//!   2. [`local_findings`] derives severity-tagged, file-referenced comments
//!      from that context (sensitive files, missing tests, risky areas) so the
//!      command is useful even with no AI provider configured.
//!   3. The caller redacts the diff (see [`super::agent_redact`]) before
//!      handing it to a provider for narrative review.

use serde::Serialize;

use super::agent_redact::is_sensitive_filename;

/// Severity of a review finding. Ordered most-severe first for sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// One changed file with its line deltas (`None` = binary) and tags.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewFile {
    pub path: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub is_test: bool,
    pub is_sensitive: bool,
}

/// A changed path that warrants extra scrutiny, with why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RiskyArea {
    pub path: String,
    pub reason: String,
}

/// Structured local analysis of a diff. Serializes for editor/mobile clients.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewContext {
    pub files: Vec<ReviewFile>,
    pub files_changed: usize,
    pub additions: u64,
    pub deletions: u64,
    pub test_files_changed: usize,
    pub risky_areas: Vec<RiskyArea>,
}

/// A single review comment: severity, optional file/line reference, message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewComment {
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

/// Parse `git diff --numstat` output into changed files. Each line is
/// `<adds>\t<dels>\t<path>`; binary files report `-` for both counts.
pub fn parse_numstat(numstat: &str) -> Vec<ReviewFile> {
    let mut files = Vec::new();
    for line in numstat.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(adds), Some(dels), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        files.push(review_file(path, adds.parse().ok(), dels.parse().ok()));
    }
    files
}

/// Build a tagged [`ReviewFile`], computing the test / sensitive flags from the
/// path. The shared constructor for both the numstat and PR-files paths.
pub fn review_file(path: &str, additions: Option<u64>, deletions: Option<u64>) -> ReviewFile {
    ReviewFile {
        path: path.to_string(),
        additions,
        deletions,
        is_test: is_test_path(path),
        is_sensitive: is_sensitive_filename(path),
    }
}

/// Assemble a [`ReviewContext`] from `git diff --numstat` output.
pub fn build_context(numstat: &str) -> ReviewContext {
    assemble_context(parse_numstat(numstat))
}

/// Roll a set of tagged files up into a [`ReviewContext`].
pub fn assemble_context(files: Vec<ReviewFile>) -> ReviewContext {
    let additions = files.iter().filter_map(|f| f.additions).sum();
    let deletions = files.iter().filter_map(|f| f.deletions).sum();
    let test_files_changed = files.iter().filter(|f| f.is_test).count();
    // Test files touching a risky subsystem are expected; don't flag them.
    let risky_areas = files
        .iter()
        .filter(|f| !f.is_test)
        .filter_map(|f| {
            risk_reason(&f.path).map(|reason| RiskyArea {
                path: f.path.clone(),
                reason: reason.to_string(),
            })
        })
        .collect();
    ReviewContext {
        files_changed: files.len(),
        additions,
        deletions,
        test_files_changed,
        risky_areas,
        files,
    }
}

/// Derive local review comments from the context. `security` adds a focused
/// advisory and is also threaded into the provider prompt by the caller.
pub fn local_findings(ctx: &ReviewContext, security: bool) -> Vec<ReviewComment> {
    let mut out = Vec::new();

    // Sensitive files in the diff are the highest-priority signal.
    for f in ctx.files.iter().filter(|f| f.is_sensitive) {
        out.push(ReviewComment {
            severity: Severity::Critical,
            file: Some(f.path.clone()),
            line: None,
            message: "secret-bearing file in the diff; withheld from the AI provider and \
                      likely should not be committed"
                .to_string(),
        });
    }

    // Source touched without tests is a common review smell.
    let source_changed = ctx.files.iter().any(|f| !f.is_test && !f.is_sensitive);
    if source_changed && ctx.test_files_changed == 0 {
        out.push(ReviewComment {
            severity: Severity::Warning,
            file: None,
            line: None,
            message: "source changed without accompanying test changes".to_string(),
        });
    }

    // Each risky area becomes a comment at its own severity.
    for area in &ctx.risky_areas {
        out.push(ReviewComment {
            severity: risk_severity(&area.reason),
            file: Some(area.path.clone()),
            line: None,
            message: area.reason.clone(),
        });
    }

    if security {
        out.push(ReviewComment {
            severity: Severity::Info,
            file: None,
            line: None,
            message: "security pass: check authz, input validation, secret handling, and \
                      dependency changes in the files above"
                .to_string(),
        });
    }

    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.file.cmp(&b.file))
    });
    out
}

/// What redaction did before the diff was sent to a provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Redaction {
    pub secrets_masked: usize,
    pub withheld_files: Vec<String>,
}

/// The complete review of one target (working tree, a PR, or a stack item),
/// ready to print as a table or serialize for editor/mobile clients.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewReport {
    pub target: String,
    pub context: ReviewContext,
    pub comments: Vec<ReviewComment>,
    pub redaction: Redaction,
    /// Provider name, or `"none"` when no AI backend is configured.
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Free-form provider narrative; empty when unconfigured.
    pub narrative: String,
}

/// Render a report as human-readable text.
pub fn render_report(r: &ReviewReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let c = &r.context;
    let _ = writeln!(s, "Review: {}", r.target);
    let _ = writeln!(
        s,
        "  {} files changed, +{} -{}  ({} test file{})",
        c.files_changed,
        c.additions,
        c.deletions,
        c.test_files_changed,
        if c.test_files_changed == 1 { "" } else { "s" },
    );
    if r.redaction.secrets_masked > 0 || !r.redaction.withheld_files.is_empty() {
        let withheld = if r.redaction.withheld_files.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.redaction.withheld_files.join(", "))
        };
        let _ = writeln!(
            s,
            "  redaction: {} secret(s) masked, {} file(s) withheld{}",
            r.redaction.secrets_masked,
            r.redaction.withheld_files.len(),
            withheld,
        );
    }

    if r.comments.is_empty() {
        let _ = writeln!(s, "\nFindings: none");
    } else {
        let _ = writeln!(s, "\nFindings ({}):", r.comments.len());
        for cm in &r.comments {
            let loc = match (&cm.file, cm.line) {
                (Some(f), Some(l)) => format!("{f}:{l}  "),
                (Some(f), None) => format!("{f}  "),
                _ => String::new(),
            };
            let _ = writeln!(s, "  [{}] {}{}", cm.severity.as_str(), loc, cm.message);
        }
    }

    if r.provider == "none" {
        let _ = writeln!(
            s,
            "\nAI provider: not configured (set FJ_AGENT_PROVIDER for narrative review)"
        );
    } else {
        let model = r
            .model
            .as_deref()
            .map(|m| format!("/{m}"))
            .unwrap_or_default();
        let _ = writeln!(s, "\nAI review ({}{}):", r.provider, model);
        for line in r.narrative.lines() {
            let _ = writeln!(s, "  {line}");
        }
    }
    s
}

// ── classification heuristics ──────────────────────────────────────────────

/// True if a path looks like a test file (multi-language heuristics).
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || base.starts_with("test_")
        || base.ends_with("_test.rs")
        || base.ends_with("_test.go")
        || base.ends_with("_test.py")
        || base.contains(".test.")
        || base.contains(".spec.")
        || base.ends_with("_spec.rb")
}

/// If a path touches a sensitive subsystem, the reason to flag it.
fn risk_reason(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);

    if lower.contains("migration") || lower.contains("/migrate") {
        return Some("database migration — review for irreversibility and data loss");
    }
    if lower.contains("auth")
        || lower.contains("login")
        || lower.contains("password")
        || lower.contains("crypto")
        || lower.contains("secret")
        || lower.contains("/security")
    {
        return Some("security-sensitive code — review authz and credential handling");
    }
    if lower.starts_with(".github/")
        || lower.starts_with(".forgejo/")
        || lower.contains("/.github/")
        || base == "jenkinsfile"
        || base == ".gitlab-ci.yml"
    {
        return Some("CI/automation change — review for supply-chain and permissions risk");
    }
    const MANIFESTS: &[&str] = &[
        "cargo.toml",
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "go.mod",
        "go.sum",
        "requirements.txt",
        "pyproject.toml",
        "gemfile",
        "gemfile.lock",
        "pom.xml",
        "build.gradle",
    ];
    if MANIFESTS.contains(&base) {
        return Some("dependency manifest change — review new/updated packages");
    }
    if base == "dockerfile" || lower.ends_with(".tf") || base.starts_with("docker-compose") {
        return Some("infrastructure change — review build/runtime surface");
    }
    None
}

/// Severity for a risk reason (keyed off the reason string we produced).
fn risk_severity(reason: &str) -> Severity {
    if reason.starts_with("database migration") || reason.starts_with("security-sensitive") {
        Severity::Warning
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numstat_handles_counts_and_binary() {
        let files = parse_numstat("10\t2\tsrc/main.rs\n-\t-\tlogo.png\n");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].additions, Some(10));
        assert_eq!(files[0].deletions, Some(2));
        // binary file: counts are None
        assert_eq!(files[1].additions, None);
        assert_eq!(files[1].deletions, None);
    }

    #[test]
    fn parse_numstat_skips_blank_and_malformed() {
        let files = parse_numstat("\n  \ngarbage-no-tabs\n3\t1\tok.rs\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "ok.rs");
    }

    #[test]
    fn is_test_path_covers_common_layouts() {
        for p in [
            "tests/foo.rs",
            "src/api/tests/x.rs",
            "pkg/foo_test.go",
            "app/__tests__/a.js",
            "foo.test.ts",
            "bar.spec.tsx",
            "test_thing.py",
            "spec/models/user_spec.rb",
        ] {
            assert!(is_test_path(p), "{p} should be a test path");
        }
        for p in ["src/main.rs", "README.md", "src/contest.rs"] {
            assert!(!is_test_path(p), "{p} should not be a test path");
        }
    }

    #[test]
    fn build_context_rolls_up_totals_and_tags() {
        let numstat = "\
20\t4\tsrc/auth/login.rs
5\t0\tsrc/auth/login_test.rs
2\t1\tCargo.toml
-\t-\tassets/logo.png
";
        let ctx = build_context(numstat);
        assert_eq!(ctx.files_changed, 4);
        assert_eq!(ctx.additions, 27); // 20 + 5 + 2 (binary excluded)
        assert_eq!(ctx.deletions, 5);
        assert_eq!(ctx.test_files_changed, 1);
        // login.rs (security) + Cargo.toml (manifest) are risky; test + png are not
        let risky: Vec<&str> = ctx.risky_areas.iter().map(|r| r.path.as_str()).collect();
        assert!(risky.contains(&"src/auth/login.rs"));
        assert!(risky.contains(&"Cargo.toml"));
        assert_eq!(ctx.risky_areas.len(), 2);
    }

    #[test]
    fn local_findings_flags_secrets_missing_tests_and_risk() {
        let ctx = build_context("3\t0\t.env\n10\t0\tsrc/auth/login.rs\n");
        let f = local_findings(&ctx, false);
        // sorted: critical (.env) first
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].file.as_deref(), Some(".env"));
        // a missing-tests warning is present (file: None)
        assert!(f.iter().any(|c| c.severity == Severity::Warning
            && c.file.is_none()
            && c.message.contains("without accompanying test")));
        // the security risk on login.rs is present
        assert!(f
            .iter()
            .any(|c| c.file.as_deref() == Some("src/auth/login.rs")
                && c.message.contains("security-sensitive")));
    }

    #[test]
    fn local_findings_security_adds_advisory_and_no_missing_test_warning_when_tested() {
        let ctx = build_context("4\t0\tsrc/lib.rs\n4\t0\tsrc/lib_test.rs\n");
        let f = local_findings(&ctx, true);
        assert!(f.iter().any(|c| c.message.starts_with("security pass")));
        // tests changed -> no missing-test warning
        assert!(!f
            .iter()
            .any(|c| c.message.contains("without accompanying test")));
    }

    #[test]
    fn severity_orders_critical_first_and_serializes_lowercase() {
        assert!(Severity::Critical < Severity::Warning);
        assert!(Severity::Warning < Severity::Info);
        let v = serde_json::to_value(Severity::Critical).unwrap();
        assert_eq!(v, serde_json::json!("critical"));
    }

    #[test]
    fn render_report_shows_findings_redaction_and_unconfigured_note() {
        let ctx = build_context("10\t2\t.env\n3\t0\tsrc/auth/login.rs\n");
        let report = ReviewReport {
            target: "working tree".into(),
            comments: local_findings(&ctx, false),
            context: ctx,
            redaction: Redaction {
                secrets_masked: 1,
                withheld_files: vec![".env".into()],
            },
            provider: "none".into(),
            model: None,
            narrative: String::new(),
        };
        let out = render_report(&report);
        assert!(out.contains("Review: working tree"));
        assert!(out.contains("2 files changed, +13 -2"));
        assert!(out.contains("redaction: 1 secret(s) masked, 1 file(s) withheld (.env)"));
        assert!(out.contains("[critical] .env"));
        assert!(out.contains("[warning]")); // missing tests
        assert!(out.contains("not configured"));
    }

    #[test]
    fn render_report_shows_provider_narrative_when_configured() {
        let ctx = build_context("1\t0\tsrc/a.rs\n");
        let report = ReviewReport {
            target: "PR #7".into(),
            comments: vec![],
            context: ctx,
            redaction: Redaction::default(),
            provider: "mock".into(),
            model: Some("m1".into()),
            narrative: "looks fine\nminor nit".into(),
        };
        let out = render_report(&report);
        assert!(out.contains("Findings: none"));
        assert!(out.contains("AI review (mock/m1):"));
        assert!(out.contains("  looks fine"));
        assert!(out.contains("  minor nit"));
    }

    #[test]
    fn context_serializes_stable_shape() {
        let ctx = build_context("1\t0\tsrc/a.rs\n");
        let v = serde_json::to_value(&ctx).unwrap();
        assert_eq!(v["files_changed"], 1);
        assert_eq!(v["files"][0]["path"], "src/a.rs");
        assert_eq!(v["files"][0]["is_test"], false);
        assert_eq!(v["additions"], 1);
    }
}
