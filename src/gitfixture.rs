//! Throwaway git repositories for tests.
//!
//! `stack ship`'s restack is git plumbing (rebase `--onto … --update-refs`,
//! force-push with a lease), and the only honest way to test it is against a
//! real repository. These helpers build one in a temp dir, with identity,
//! signing, and hooks pinned in *local* config so whatever the developer has
//! configured globally can't change what the tests observe.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// True when git is present and new enough for `rebase --update-refs` (2.38),
/// which everything here depends on. Tests skip themselves when it isn't.
pub fn git_supports_update_refs() -> bool {
    let Ok(out) = Command::new("git").arg("--version").output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let Some(rest) = raw.split("version").nth(1) else {
        return false;
    };
    let mut parts = rest
        .trim()
        .split(['.', ' ', '\n', '\t'])
        .filter(|p| !p.is_empty());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor) >= (2, 38)
}

/// Run git in `dir`, asserting it succeeded, and return trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Initialize a repo at `dir` with a deterministic local configuration.
pub fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create repo dir");
    let out = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .arg(dir)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed");
    pin_config(dir);
}

/// Initialize a bare repo at `dir`, to stand in for the forge's remote.
pub fn init_bare(dir: &Path) {
    let out = Command::new("git")
        .args(["init", "--quiet", "--bare", "--initial-branch=main"])
        .arg(dir)
        .output()
        .expect("git init --bare");
    assert!(out.status.success(), "git init --bare failed");
}

/// Clone `src` to `dest` and pin the clone's config.
pub fn clone_repo(src: &Path, dest: &Path) {
    let out = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(src)
        .arg(dest)
        .output()
        .expect("git clone");
    assert!(out.status.success(), "git clone failed");
    pin_config(dest);
}

fn pin_config(dir: &Path) {
    git(dir, &["config", "user.name", "fj tests"]);
    git(dir, &["config", "user.email", "tests@example.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    let hooks = dir.join("no-such-hooks");
    git(
        dir,
        &["config", "core.hooksPath", hooks.to_str().expect("utf-8")],
    );
}

/// Commit `contents` as `name` with an explicit message, returning the new sha.
///
/// The message is a parameter because two commits sharing a tree, parent,
/// message, identity, and second-resolution timestamp hash to the *same* sha,
/// which would make a simulated squash land indistinguishable from the commit
/// it replaces.
pub fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) -> String {
    std::fs::write(dir.join(name), contents).expect("write file");
    git(dir, &["add", name]);
    git(dir, &["commit", "--no-verify", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Number of commits in `range`.
pub fn count(dir: &Path, range: &str) -> usize {
    git(dir, &["rev-list", "--count", range])
        .parse()
        .expect("a count")
}

/// A three-item stack: `main` ← `u/s-1` ← `u/s-2` ← `u/s-3`, one commit each.
/// Returns the temp dir (keep it alive), the worktree, and `u/s-1`'s tip.
pub fn stacked_repo() -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let work = tmp.path().join("work");
    init_repo(&work);
    commit_file(&work, "trunk.txt", "t", "trunk");
    git(&work, &["checkout", "--quiet", "-b", "u/s-1"]);
    let tip1 = commit_file(&work, "a.txt", "a", "item 1");
    git(&work, &["checkout", "--quiet", "-b", "u/s-2"]);
    commit_file(&work, "b.txt", "b", "item 2");
    git(&work, &["checkout", "--quiet", "-b", "u/s-3"]);
    commit_file(&work, "c.txt", "c", "item 3");
    (tmp, work, tip1)
}

/// `stacked_repo`, plus a bare "origin" carrying every branch. Returns the
/// temp dir, the worktree, the bare remote, and `u/s-1`'s tip.
pub fn published_stack() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let (tmp, work, tip1) = stacked_repo();
    let bare = tmp.path().join("origin.git");
    init_bare(&bare);
    git(
        &work,
        &["remote", "add", "origin", bare.to_str().expect("utf-8")],
    );
    git(
        &work,
        &[
            "push", "--quiet", "origin", "main", "u/s-1", "u/s-2", "u/s-3",
        ],
    );
    (tmp, work, bare, tip1)
}

/// Land `u/s-1` on the remote's trunk the way a squash merge does: a single
/// new commit carrying its tree, pushed by someone else. Returns the new tip.
pub fn squash_land_on_remote(tmp: &Path, bare: &Path) -> String {
    let forge = tmp.join("forge");
    clone_repo(bare, &forge);
    let landed = commit_file(&forge, "a.txt", "a", "item 1 (#1)");
    git(&forge, &["push", "--quiet", "origin", "main"]);
    landed
}
