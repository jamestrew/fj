//! Local-only work-session state, persisted at `<git-dir>/fj/session.json`.
//!
//! `fj work context` writes it; `fj work resume` / `fj work continue` read it.
//! It is never committed to the repo and never sent to the server — it's a
//! convenience cache of "where you were" plus an advisory, non-destructive
//! list of suggested next steps.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// RFC3339 timestamp of when this session was saved.
    pub saved_at: String,
    pub host: String,
    /// `owner/name`.
    pub repo: String,
    /// Primary git remote URL at save time, if any.
    #[serde(default)]
    pub remote: Option<String>,
    /// None when HEAD was detached / not a git repo.
    #[serde(default)]
    pub branch: Option<String>,
    pub dirty: bool,
    pub changed_files: usize,
    /// The open PR for the branch at save time, if any.
    #[serde(default)]
    pub pull_request: Option<PrRef>,
    /// Ordered, advisory next steps. Never executed automatically.
    pub next_steps: Vec<String>,
    /// Index into `next_steps` that `work continue` advances.
    pub cursor: usize,
    /// Opaque digest of the host, account and Fjord instance the session was
    /// saved under. `None` is the legacy default identity. Never a login name.
    #[serde(default)]
    pub identity: Option<String>,
    /// The session was saved with `-R`, so its destination ignored the clone's
    /// host; `work continue`, which has no `-R`, must resolve it the same way.
    #[serde(default)]
    pub explicit_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrRef {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
}

/// Path to the session file under a git directory (e.g. `.git/fj/session.json`).
pub fn session_path(git_dir: &Path) -> PathBuf {
    git_dir.join("fj").join("session.json")
}

/// Load the saved session. `Ok(None)` when not in a repo or no session exists
/// yet; `Err` only when the file is present but unreadable / corrupt.
pub fn load() -> Result<Option<Session>> {
    let Some(dir) = crate::git::git_dir() else {
        return Ok(None);
    };
    let path = session_path(&dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let session = serde_json::from_str(&s)
                .with_context(|| format!("parsing session file {}", path.display()))?;
            Ok(Some(session))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading session file {}", path.display())),
    }
}

/// Persist the session under the git dir, creating `<git-dir>/fj/` as needed.
pub fn save(session: &Session) -> Result<()> {
    let dir = crate::git::git_dir().context("not inside a git repository")?;
    let fj_dir = dir.join("fj");
    std::fs::create_dir_all(&fj_dir).with_context(|| format!("creating {}", fj_dir.display()))?;
    let path = session_path(&dir);
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Refuse a session saved under another account or instance, before anything
/// is shown or written. Both sides are digests, so no login name is printed.
pub fn check_identity(saved: &Session, current: Option<&str>) -> Result<()> {
    if saved.identity.as_deref() == current {
        return Ok(());
    }
    anyhow::bail!(
        "the saved work session belongs to a different account or instance than the \
         current one. Run `fj work context` to start a session for this one."
    )
}

/// Advisory, non-destructive next steps derived from the current context.
/// `ahead` is `None` when the branch has no upstream.
pub fn derive_next_steps(
    branch: Option<&str>,
    dirty: bool,
    changed_files: usize,
    ahead: Option<u32>,
    has_pr: bool,
) -> Vec<String> {
    if branch.is_none() {
        return vec!["Check out a branch to start work (detached HEAD)".to_string()];
    }
    let mut steps = Vec::new();
    if dirty {
        steps.push(format!(
            "Commit or stash your {changed_files} uncommitted change(s)"
        ));
    }
    match ahead {
        None => steps.push("Push your branch to create an upstream (git push -u)".to_string()),
        Some(n) if n > 0 => steps.push(format!("Push {n} unpushed commit(s) to your upstream")),
        _ => {}
    }
    if has_pr {
        steps.push("Track review on your PR (fj pr checks / fj pr view)".to_string());
    } else {
        steps.push("Open a pull request (fj pr create)".to_string());
    }
    if steps.is_empty() {
        steps.push("Working tree is clean and up to date — nothing pending".to_string());
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_path_is_under_git_dir_fj() {
        let p = session_path(Path::new("/repo/.git"));
        assert_eq!(p, PathBuf::from("/repo/.git/fj/session.json"));
    }

    #[test]
    fn round_trips_through_json() {
        let s = Session {
            saved_at: "2026-06-01T00:00:00+00:00".into(),
            host: "git.example.com".into(),
            repo: "o/n".into(),
            remote: Some("git@git.example.com:o/n.git".into()),
            branch: Some("feature/x".into()),
            dirty: true,
            changed_files: 2,
            pull_request: Some(PrRef {
                number: 7,
                title: "Add feature".into(),
                state: "open".into(),
                url: "https://x/pulls/7".into(),
            }),
            next_steps: vec!["a".into(), "b".into()],
            cursor: 1,
            identity: None,
            explicit_repo: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    fn saved(identity: Option<&str>) -> Session {
        Session {
            saved_at: "t".into(),
            host: "h".into(),
            repo: "o/n".into(),
            remote: None,
            branch: None,
            dirty: false,
            changed_files: 0,
            pull_request: None,
            next_steps: vec![],
            cursor: 0,
            identity: identity.map(str::to_string),
            explicit_repo: false,
        }
    }

    #[test]
    fn identity_must_match_exactly() {
        assert!(check_identity(&saved(None), None).is_ok());
        assert!(check_identity(&saved(Some("id-a")), Some("id-a")).is_ok());
        let cases = [
            (None, Some("id-b")),
            (Some("id-a"), None),
            (Some("id-a"), Some("id-b")),
        ];
        for (s, c) in cases {
            let e = check_identity(&saved(s), c).unwrap_err().to_string();
            assert!(e.contains("different account or instance"), "{e}");
            assert!(e.contains("fj work context") && !e.contains("id-"), "{e}");
        }
    }

    #[test]
    fn sessions_saved_before_identity_existed_still_parse() {
        let old = r#"{"saved_at":"t","host":"h","repo":"o/n","dirty":false,"changed_files":0,
            "next_steps":[],"cursor":0}"#;
        let s: Session = serde_json::from_str(old).unwrap();
        assert_eq!(s.identity, None);
    }

    #[test]
    fn detached_head_yields_single_checkout_step() {
        let steps = derive_next_steps(None, false, 0, None, false);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].contains("Check out a branch"));
    }

    #[test]
    fn dirty_no_upstream_no_pr_suggests_commit_push_and_pr() {
        let steps = derive_next_steps(Some("feature/x"), true, 3, None, false);
        assert!(steps.iter().any(|s| s.contains("3 uncommitted")));
        assert!(steps.iter().any(|s| s.contains("create an upstream")));
        assert!(steps.iter().any(|s| s.contains("Open a pull request")));
    }

    #[test]
    fn ahead_with_open_pr_suggests_push_and_review() {
        let steps = derive_next_steps(Some("feature/x"), false, 0, Some(2), true);
        assert!(steps.iter().any(|s| s.contains("Push 2 unpushed")));
        assert!(steps.iter().any(|s| s.contains("Track review")));
        assert!(!steps.iter().any(|s| s.contains("uncommitted")));
    }

    #[test]
    fn clean_pushed_with_pr_is_not_empty() {
        // clean, up to date (ahead 0), PR open → only the review step.
        let steps = derive_next_steps(Some("feature/x"), false, 0, Some(0), true);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].contains("Track review"));
    }
}
