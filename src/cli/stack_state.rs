//! Local-only stack metadata, persisted at `<git-dir>/fj/stack.json`.
//!
//! Option A from the design: client-side, git-native, offline, no server
//! state and no committed manifest (never `.fjord/stack.yml`). One stack per
//! repo. The schema is versioned and intended to be stable for future
//! mobile/web consumers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Bump when the on-disk schema changes incompatibly.
pub const STACK_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    pub version: u32,
    /// Stable identifier (currently the slug).
    pub id: String,
    /// Human-readable name as given to `stack new`.
    pub name: String,
    /// Branch-safe slug derived from the name.
    pub slug: String,
    pub repo: RepoRef,
    /// Branch the stack ultimately targets (its trunk).
    pub base: String,
    pub created_at: String,
    pub updated_at: String,
    /// Linked issue, if the stack was created `--from issue <n>`.
    pub issue: Option<IssueRef>,
    #[serde(default)]
    pub items: Vec<StackItem>,
    /// Set by `stack ship` once every item has merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoRef {
    /// Absolute path to the working tree root.
    pub path: String,
    /// Git remote name (e.g. `origin`).
    pub remote: String,
    /// Remote URL.
    pub url: String,
    /// Host parsed from the remote.
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueRef {
    pub number: u64,
    /// Fetched best-effort; null when offline / unavailable.
    pub title: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackItem {
    pub id: String,
    pub title: String,
    pub branch: String,
    pub base: String,
    pub pr: Option<ItemPr>,
    /// `local` | `pushed` | `merged` …
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemPr {
    pub number: u64,
    pub url: String,
    pub state: String,
}

// fjord-core owns the canonical stack-item shape (ADR 0001). Core models PR
// numbers as i64 to match the API DTOs; fj has always carried them as u64, so
// the conversion narrows back (PR numbers are positive, hence `unwrap_or_default`
// can never lose a real value).
impl From<fjord_core::workflows::stack::ItemPr> for ItemPr {
    fn from(p: fjord_core::workflows::stack::ItemPr) -> Self {
        Self {
            number: u64::try_from(p.number).unwrap_or_default(),
            url: p.url,
            state: p.state,
        }
    }
}

impl From<fjord_core::workflows::stack::StackItem> for StackItem {
    fn from(it: fjord_core::workflows::stack::StackItem) -> Self {
        Self {
            id: it.id,
            title: it.title,
            branch: it.branch,
            base: it.base,
            pr: it.pr.map(Into::into),
            status: it.status,
        }
    }
}

// The forward direction (fj -> core), so the on-disk stack can be handed to the
// core planning/annotation engine. fj carries PR/issue numbers as u64; core
// models them as i64 (matching the API DTOs). The widening always succeeds for
// real, positive values, so `unwrap_or_default` can never lose one.
impl From<&ItemPr> for fjord_core::workflows::stack::ItemPr {
    fn from(p: &ItemPr) -> Self {
        Self {
            number: i64::try_from(p.number).unwrap_or_default(),
            url: p.url.clone(),
            state: p.state.clone(),
        }
    }
}

impl From<&StackItem> for fjord_core::workflows::stack::StackItem {
    fn from(it: &StackItem) -> Self {
        Self {
            id: it.id.clone(),
            title: it.title.clone(),
            branch: it.branch.clone(),
            base: it.base.clone(),
            pr: it.pr.as_ref().map(Into::into),
            status: it.status.clone(),
        }
    }
}

impl From<&RepoRef> for fjord_core::workflows::stack::RepoRef {
    fn from(r: &RepoRef) -> Self {
        Self {
            path: r.path.clone(),
            remote: r.remote.clone(),
            url: r.url.clone(),
            host: r.host.clone(),
        }
    }
}

impl From<&IssueRef> for fjord_core::workflows::stack::IssueRef {
    fn from(i: &IssueRef) -> Self {
        Self {
            number: i64::try_from(i.number).unwrap_or_default(),
            title: i.title.clone(),
            url: i.url.clone(),
        }
    }
}

impl From<&Stack> for fjord_core::workflows::stack::Stack {
    fn from(s: &Stack) -> Self {
        Self {
            version: s.version,
            id: s.id.clone(),
            name: s.name.clone(),
            slug: s.slug.clone(),
            repo: (&s.repo).into(),
            base: s.base.clone(),
            created_at: s.created_at.clone(),
            updated_at: s.updated_at.clone(),
            issue: s.issue.as_ref().map(Into::into),
            items: s.items.iter().map(Into::into).collect(),
            shipped_at: s.shipped_at.clone(),
        }
    }
}

/// Path to the stack file under a git directory (`.git/fj/stack.json`).
pub fn stack_path(git_dir: &Path) -> PathBuf {
    git_dir.join("fj").join("stack.json")
}

/// Load the stack for the current repo, if any. `Ok(None)` when not in a repo
/// or no stack exists yet; `Err` only when a present file is unreadable.
pub fn load() -> Result<Option<Stack>> {
    let Some(dir) = crate::git::git_dir() else {
        return Ok(None);
    };
    load_from(&stack_path(&dir))
}

/// Load a stack from an explicit path. Testable without git/cwd.
pub fn load_from(path: &Path) -> Result<Option<Stack>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| {
            format!("parsing stack file {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading stack file {}", path.display())),
    }
}

/// Persist the stack under the current repo's git dir.
pub fn save(stack: &Stack) -> Result<()> {
    let dir = crate::git::git_dir().context("not inside a git repository")?;
    save_to(&stack_path(&dir), stack)
}

/// Persist a stack to an explicit path, creating parent dirs. Testable.
pub fn save_to(path: &Path, stack: &Stack) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(stack)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

// fjord-core owns the canonical stack naming rules (ADR 0001) so a stack
// authored on the CLI and one authored on mobile land on byte-identical branch
// names. These thin wrappers delegate while keeping the in-crate call sites and
// fj's `usize` index ergonomics. (The unrelated `slugify` in issue.rs is a
// different transform — "issue" fallback, 40-char cap — and stays local.)

/// Lowercase, hyphen-separated, branch-safe slug derived from a name. Falls
/// back to `stack` for empty input; capped to keep branch names sane.
pub fn slugify(name: &str) -> String {
    fjord_core::workflows::stack::slugify(name)
}

/// A branch-safe handle for branch prefixes: prefer the account login, then
/// the local git user, then `user`. Sanitized like a slug.
pub fn safe_handle(account: Option<&str>, git_user: Option<&str>) -> String {
    fjord_core::workflows::stack::safe_handle(account, git_user)
}

/// Deterministic branch name for stack item `n` (1-based): `handle/slug-n`.
pub fn item_branch(handle: &str, slug: &str, n: usize) -> String {
    fjord_core::workflows::stack::item_branch(handle, slug, n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stack() -> Stack {
        Stack {
            version: STACK_VERSION,
            id: "login-flow".into(),
            name: "Login Flow".into(),
            slug: "login-flow".into(),
            repo: RepoRef {
                path: "/repo".into(),
                remote: "origin".into(),
                url: "ssh://git@host/o/r.git".into(),
                host: "host".into(),
            },
            base: "main".into(),
            created_at: "2026-06-01T00:00:00+00:00".into(),
            updated_at: "2026-06-01T00:00:00+00:00".into(),
            issue: None,
            items: vec![StackItem {
                id: "login-flow-1".into(),
                title: "Login Flow".into(),
                branch: "stephen/login-flow-1".into(),
                base: "main".into(),
                pr: None,
                status: "local".into(),
            }],
            shipped_at: None,
        }
    }

    #[test]
    fn stack_path_is_under_git_dir_fj() {
        assert_eq!(
            stack_path(Path::new("/repo/.git")),
            PathBuf::from("/repo/.git/fj/stack.json")
        );
    }

    #[test]
    fn slugify_sanitizes() {
        assert_eq!(slugify("Login Flow"), "login-flow");
        assert_eq!(slugify("  Add  OAuth!! "), "add-oauth");
        assert_eq!(slugify("feature/AUTH_v2"), "feature-auth-v2");
        assert_eq!(slugify("???"), "stack");
        assert!(slugify(&"x".repeat(100)).len() <= 50);
    }

    #[test]
    fn safe_handle_precedence_and_sanitization() {
        // account login wins
        assert_eq!(safe_handle(Some("stephen"), Some("Stephen Way")), "stephen");
        // falls back to git user, sanitized
        assert_eq!(safe_handle(None, Some("Stephen Way")), "stephen-way");
        // empty/symbol inputs fall through to "user"
        assert_eq!(safe_handle(Some("  "), None), "user");
        assert_eq!(safe_handle(None, None), "user");
    }

    #[test]
    fn item_branch_is_deterministic() {
        assert_eq!(
            item_branch("stephen", "login-flow", 1),
            "stephen/login-flow-1"
        );
        assert_eq!(item_branch("user", "x", 3), "user/x-3");
    }

    #[test]
    fn round_trips_through_json() {
        let s = sample_stack();
        let json = serde_json::to_string(&s).unwrap();
        let back: Stack = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn save_to_then_load_from_roundtrips_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fj").join("stack.json");
        assert!(load_from(&path).unwrap().is_none()); // missing -> None
        let s = sample_stack();
        save_to(&path, &s).unwrap();
        assert_eq!(load_from(&path).unwrap().unwrap(), s);
    }
}
