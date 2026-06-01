pub mod remote;

use std::process::Command;

use anyhow::{bail, Context, Result};

pub use remote::discover;
#[allow(unused_imports)]
pub use remote::{parse_remote, RemoteRepo};

/// Spawn `git` synchronously and surface the exit status as an error.
pub fn run(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .status()
        .with_context(|| "spawning git (is it installed and on PATH?)")?;
    if !status.success() {
        bail!("git {} exited with status {}", args.join(" "), status);
    }
    Ok(())
}

/// Read a value from the current repo's local git config. Returns None if the
/// key is unset or we're not inside a git repo.
pub fn config_get_local(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--local", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Set a value in the current repo's local git config.
pub fn config_set_local(key: &str, value: &str) -> Result<()> {
    reject_dash_lead("config value", value)?;
    run(&["config", "--local", key, value])
}

/// Unset a local git config value. Tolerates the key not being set (git's
/// exit code 5), so it's idempotent.
pub fn config_unset_local(key: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "--local", "--unset", key])
        .status()
        .with_context(|| "spawning git config --unset")?;
    match status.code() {
        Some(0) | Some(5) => Ok(()),
        _ => bail!("git config --unset {key} failed: {status}"),
    }
}

pub fn clone(url: &str, dest: Option<&str>) -> Result<()> {
    // `--` stops git from treating a `-`-leading URL/dest as an option.
    let mut args = vec!["clone", "--", url];
    if let Some(d) = dest {
        args.push(d);
    }
    run(&args)
}

pub fn fetch_pr(remote: &str, number: u64, local_branch: &str) -> Result<()> {
    reject_dash_lead("branch name", local_branch)?;
    let refspec = format!("pull/{number}/head:{local_branch}");
    run(&["fetch", "--", remote, &refspec])
}

pub fn checkout(branch: &str) -> Result<()> {
    // `git checkout -- <x>` means "checkout pathspec", so `--` can't be used
    // to disarm a `-`-leading branch here; reject it outright instead.
    reject_dash_lead("branch name", branch)?;
    run(&["checkout", branch])
}

/// Current branch name, or None when detached HEAD or not inside a repo.
pub fn current_branch() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() || b == "HEAD" {
        None
    } else {
        Some(b)
    }
}

/// `(dirty, changed_file_count)` from `git status --porcelain`. None if we
/// can't run git / aren't in a repo.
pub fn working_tree_status() -> Option<(bool, usize)> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
}

/// `(ahead, behind)` of HEAD relative to its upstream `@{u}`. None when there
/// is no upstream configured or we aren't in a repo.
pub fn ahead_behind_upstream() -> Option<(u32, u32)> {
    let out = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "@{u}...HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_left_right_count(&String::from_utf8_lossy(&out.stdout))
}

/// Count non-empty `git status --porcelain` lines → (dirty, count).
fn parse_porcelain(s: &str) -> (bool, usize) {
    let n = s.lines().filter(|l| !l.trim().is_empty()).count();
    (n > 0, n)
}

/// `git rev-list --left-right --count @{u}...HEAD` prints "<behind>\t<ahead>"
/// (left = upstream-only commits, right = HEAD-only). Return `(ahead, behind)`.
fn parse_left_right_count(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split_whitespace();
    let behind: u32 = it.next()?.parse().ok()?;
    let ahead: u32 = it.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// Reject values that begin with `-` so they can't be smuggled in as git
/// options (argument injection) when `--` isn't usable.
fn reject_dash_lead(what: &str, value: &str) -> Result<()> {
    if value.starts_with('-') {
        bail!("{what} {value:?} starts with '-'; refusing to pass it to git");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_left_right_count, parse_porcelain, reject_dash_lead};

    #[test]
    fn rejects_dash_lead() {
        assert!(reject_dash_lead("branch", "--upload-pack=evil").is_err());
        assert!(reject_dash_lead("branch", "-x").is_err());
        assert!(reject_dash_lead("branch", "pr-42").is_ok());
        assert!(reject_dash_lead("branch", "feature/x").is_ok());
    }

    #[test]
    fn porcelain_clean_vs_dirty() {
        assert_eq!(parse_porcelain(""), (false, 0));
        assert_eq!(parse_porcelain("\n  \n"), (false, 0));
        assert_eq!(parse_porcelain(" M src/a.rs\n?? b.rs\n"), (true, 2));
    }

    #[test]
    fn left_right_count_maps_to_ahead_behind() {
        // "<behind>\t<ahead>" -> (ahead, behind)
        assert_eq!(parse_left_right_count("0\t0\n"), Some((0, 0)));
        assert_eq!(parse_left_right_count("2\t5\n"), Some((5, 2)));
        assert_eq!(parse_left_right_count("3 1"), Some((1, 3)));
        assert_eq!(parse_left_right_count(""), None);
        assert_eq!(parse_left_right_count("nope"), None);
    }
}
