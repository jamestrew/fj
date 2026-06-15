//! Derive a PR title and body from a branch's commits, for
//! `pr create --fill` / `--fill-first`. Kept pure given the commit list (the
//! git log lives in `crate::git`) so the `--template` work in #172 can reuse
//! the same body-resolution path.

use anyhow::{bail, Result};

use crate::git::Commit;

/// Which commits feed the title and body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMode {
    /// `--fill`: draw from every commit in `base..head`.
    All,
    /// `--fill-first`: draw from the first (oldest) commit only.
    First,
}

/// A title and optional body derived from commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filled {
    pub title: String,
    pub body: Option<String>,
}

/// Derive a PR title and body from `commits` (oldest first) per `mode`.
///
/// - `First`, or `All` with a single commit: title is the commit subject, body
///   is its message body (omitted when empty).
/// - `All` with multiple commits: title is the first (oldest) commit's subject,
///   body is a bullet list of every subject, oldest first.
///
/// Errors when there are no commits to fill from.
pub fn fill_from_commits(commits: &[Commit], mode: FillMode) -> Result<Filled> {
    let Some(first) = commits.first() else {
        bail!("no commits between base and head; nothing to fill the title and body from");
    };
    let single = matches!(mode, FillMode::First) || commits.len() == 1;
    if single {
        return Ok(Filled {
            title: first.subject.clone(),
            body: non_empty(first.body.clone()),
        });
    }
    let list = commits
        .iter()
        .map(|c| format!("- {}", c.subject))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Filled {
        title: first.subject.clone(),
        body: Some(list),
    })
}

/// `Some(s)` unless `s` is blank.
fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, body: &str) -> Commit {
        Commit {
            subject: subject.into(),
            body: body.into(),
        }
    }

    #[test]
    fn empty_range_is_an_error() {
        let err = fill_from_commits(&[], FillMode::All).unwrap_err();
        assert!(err.to_string().contains("no commits between base and head"));
        assert!(fill_from_commits(&[], FillMode::First).is_err());
    }

    #[test]
    fn single_commit_uses_subject_and_body() {
        let commits = vec![commit("feat: add widget", "Longer rationale.\n\nDetails.")];
        let filled = fill_from_commits(&commits, FillMode::All).unwrap();
        assert_eq!(filled.title, "feat: add widget");
        assert_eq!(
            filled.body.as_deref(),
            Some("Longer rationale.\n\nDetails.")
        );
    }

    #[test]
    fn single_commit_without_body_has_no_body() {
        let commits = vec![commit("fix: typo", "")];
        let filled = fill_from_commits(&commits, FillMode::All).unwrap();
        assert_eq!(filled.title, "fix: typo");
        assert_eq!(filled.body, None);
    }

    #[test]
    fn multiple_commits_list_subjects_oldest_first() {
        let commits = vec![
            commit("feat: scaffold", "body ignored for the list"),
            commit("feat: wire up", ""),
            commit("test: cover it", ""),
        ];
        let filled = fill_from_commits(&commits, FillMode::All).unwrap();
        // Title is the first (oldest) subject.
        assert_eq!(filled.title, "feat: scaffold");
        assert_eq!(
            filled.body.as_deref(),
            Some("- feat: scaffold\n- feat: wire up\n- test: cover it")
        );
    }

    #[test]
    fn fill_first_ignores_later_commits() {
        let commits = vec![
            commit("feat: scaffold", "the rationale"),
            commit("feat: wire up", "second body"),
        ];
        let filled = fill_from_commits(&commits, FillMode::First).unwrap();
        assert_eq!(filled.title, "feat: scaffold");
        assert_eq!(filled.body.as_deref(), Some("the rationale"));
    }
}
