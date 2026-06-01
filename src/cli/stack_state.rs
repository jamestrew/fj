//! Local-only stacked-branch state, persisted at `<git-dir>/fj/stack.json`.
//!
//! Option A from the design: the source of truth is client-side and
//! git-native — no server state, works offline, and Forgejo needs no stack
//! support. `stack sync` may later mirror non-authoritative context into PR
//! descriptions, but never the other way around. Keep this schema stable;
//! mobile/web may consume it later.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StackState {
    #[serde(default)]
    pub stacks: Vec<Stack>,
    /// Name of the active stack, if any.
    #[serde(default)]
    pub current: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    pub name: String,
    /// Branch the stack ultimately targets (its trunk), e.g. `main`.
    pub base: String,
    /// Ordered branches in the stack, bottom to top.
    #[serde(default)]
    pub branches: Vec<String>,
    /// RFC3339 creation timestamp.
    pub created_at: String,
}

impl StackState {
    pub fn find(&self, name: &str) -> Option<&Stack> {
        self.stacks.iter().find(|s| s.name == name)
    }

    /// Add a new stack and make it current. Errors if the name is taken.
    pub fn add(&mut self, stack: Stack) -> Result<()> {
        if self.find(&stack.name).is_some() {
            bail!("a stack named '{}' already exists", stack.name);
        }
        self.current = Some(stack.name.clone());
        self.stacks.push(stack);
        Ok(())
    }
}

/// Path to the stack file under a git directory (`.git/fj/stack.json`).
pub fn stack_path(git_dir: &Path) -> PathBuf {
    git_dir.join("fj").join("stack.json")
}

/// Load the stack state. Returns an empty state when not in a repo or no file
/// exists yet; `Err` only when a present file is unreadable / corrupt.
pub fn load() -> Result<StackState> {
    let Some(dir) = crate::git::git_dir() else {
        return Ok(StackState::default());
    };
    let path = stack_path(&dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("parsing stack file {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StackState::default()),
        Err(e) => Err(e).with_context(|| format!("reading stack file {}", path.display())),
    }
}

/// Persist the stack state under the git dir, creating `<git-dir>/fj/`.
pub fn save(state: &StackState) -> Result<()> {
    let dir = crate::git::git_dir().context("not inside a git repository")?;
    let fj_dir = dir.join("fj");
    std::fs::create_dir_all(&fj_dir).with_context(|| format!("creating {}", fj_dir.display()))?;
    let path = stack_path(&dir);
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str) -> Stack {
        Stack {
            name: name.into(),
            base: "main".into(),
            branches: vec!["feature/x".into()],
            created_at: "2026-06-01T00:00:00+00:00".into(),
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
    fn add_sets_current_and_rejects_duplicates() {
        let mut st = StackState::default();
        st.add(stack("a")).unwrap();
        assert_eq!(st.current.as_deref(), Some("a"));
        assert_eq!(st.stacks.len(), 1);

        st.add(stack("b")).unwrap();
        assert_eq!(st.current.as_deref(), Some("b"));
        assert_eq!(st.stacks.len(), 2);

        let err = st.add(stack("a")).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(st.stacks.len(), 2);
    }

    #[test]
    fn find_locates_by_name() {
        let mut st = StackState::default();
        st.add(stack("a")).unwrap();
        assert!(st.find("a").is_some());
        assert!(st.find("missing").is_none());
    }

    #[test]
    fn round_trips_through_json() {
        let mut st = StackState::default();
        st.add(stack("a")).unwrap();
        let json = serde_json::to_string(&st).unwrap();
        let back: StackState = serde_json::from_str(&json).unwrap();
        assert_eq!(st, back);
    }
}
