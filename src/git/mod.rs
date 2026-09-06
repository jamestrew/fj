pub mod remote;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

pub use remote::discover;
#[allow(unused_imports)]
pub use remote::{parse_remote, RemoteRepo};

pub(crate) fn command() -> Command {
    let mut cmd = Command::new("git");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

/// Spawn `git` synchronously and surface the exit status as an error.
pub fn run(args: &[&str]) -> Result<()> {
    let _foreground = crate::interrupt::ForegroundChild::enter();
    let status = command()
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
    let out = command()
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
    let _foreground = crate::interrupt::ForegroundChild::enter();
    let status = command()
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
    let out = command()
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
    let out = command().args(["status", "--porcelain"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
}

/// Run git and capture stdout. Errors if git can't be spawned or exits
/// non-zero (surfacing stderr). Caller must ensure no `-`-leading arg is
/// passed from untrusted input (see `reject_dash_lead`).
pub fn output(args: &[&str]) -> Result<String> {
    let out = command()
        .args(args)
        .output()
        .with_context(|| "spawning git (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The repo's git directory (e.g. `.git`), or None when not in a repo. Used
/// to anchor fj's local-only state under `<git-dir>/fj/`.
pub fn git_dir() -> Option<PathBuf> {
    let out = command().args(["rev-parse", "--git-dir"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// URL of the primary remote (`upstream`, else `origin`). None if neither is
/// set or we aren't in a repo.
pub fn remote_url() -> Option<String> {
    primary_remote().map(|(_, url)| url)
}

/// `(name, url)` of the primary remote: `upstream` if present, else `origin`.
/// None when neither is set or we aren't in a repo.
pub fn primary_remote() -> Option<(String, String)> {
    for name in ["upstream", "origin"] {
        if let Ok(out) = command().args(["remote", "get-url", name]).output() {
            if out.status.success() {
                let u = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !u.is_empty() {
                    return Some((name.to_string(), u));
                }
            }
        }
    }
    None
}

/// True if `rev` resolves to something in this repo (branch, tag, sha).
pub fn rev_exists(rev: &str) -> bool {
    command()
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Push a branch to a remote. Idempotent (an up-to-date push is a no-op).
/// A non-fast-forward rejection is surfaced as a clear "diverged" error so
/// callers never silently force-push.
pub fn push(remote: &str, branch: &str) -> Result<()> {
    reject_dash_lead("remote", remote)?;
    reject_dash_lead("branch name", branch)?;
    let out = command()
        .args(["push", remote, branch])
        .output()
        .with_context(|| "spawning git push")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if is_non_fast_forward(&stderr) {
        bail!(
            "remote branch '{branch}' has diverged (non-fast-forward). Integrate \
             the remote changes (e.g. `git pull --rebase`), then re-run `fj stack sync`."
        );
    }
    bail!("git push {remote} {branch} failed: {}", stderr.trim());
}

/// Recognize git's non-fast-forward / rejected-push messages.
fn is_non_fast_forward(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("non-fast-forward")
        || s.contains("fetch first")
        || (s.contains("[rejected]") && s.contains("failed to push"))
}

/// Force-delete a local branch (`git branch -D <name>`). Used by
/// `stack ship --delete-branches` after a PR merges.
pub fn delete_local_branch(name: &str) -> Result<()> {
    reject_dash_lead("branch name", name)?;
    run(&["branch", "-D", name])
}

/// Delete a branch on a remote (`git push <remote> --delete <name>`). This is
/// a ref deletion, not a force-push of content.
pub fn delete_remote_branch(remote: &str, name: &str) -> Result<()> {
    reject_dash_lead("remote", remote)?;
    reject_dash_lead("branch name", name)?;
    run(&["push", remote, "--delete", name])
}

/// Create a new branch (`git checkout -b <name> [<start>]`). `name`/`start`
/// must not be `-`-leading (argument injection).
pub fn create_branch(name: &str, start: Option<&str>) -> Result<()> {
    reject_dash_lead("branch name", name)?;
    let mut args = vec!["checkout", "-b", name];
    if let Some(s) = start {
        reject_dash_lead("start point", s)?;
        args.push(s);
    }
    run(&args)
}

/// Commits reachable from HEAD but not `base`, oldest first, as raw
/// "<sha>\x1f<subject>" lines (unit separator, so any subject char is safe).
/// Used by `stack split` to turn a branch's commits into stacked entries.
pub fn commits_since(base: &str) -> Result<String> {
    reject_dash_lead("base", base)?;
    let range = format!("{base}..HEAD");
    output(&["log", "--reverse", "--format=%H%x1f%s", &range])
}

/// A commit's subject and message body, parsed from `git log`. `body` excludes
/// the subject line and is trimmed; it may be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub subject: String,
    pub body: String,
}

/// Commits in `base..head`, oldest first, with each commit's subject and body
/// split out. Errors if the range can't be resolved (e.g. an unknown ref) or
/// git otherwise fails. Used by `pr create --fill` / `--fill-first`.
pub fn log_commits(base: &str, head: &str) -> Result<Vec<Commit>> {
    reject_dash_lead("base", base)?;
    reject_dash_lead("head", head)?;
    let range = format!("{base}..{head}");
    // RS (0x1e) between commits, US (0x1f) between subject and body, so any
    // byte inside a subject or body is safe to parse back.
    let raw = output(&["log", "--reverse", "--format=%x1e%s%x1f%b", &range])?;
    Ok(parse_log_records(&raw))
}

/// Parse the record-separated output of `log_commits`'s format string.
fn parse_log_records(raw: &str) -> Vec<Commit> {
    raw.split('\x1e')
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let mut parts = record.splitn(2, '\x1f');
            let subject = parts.next().unwrap_or("").trim().to_string();
            let body = parts.next().unwrap_or("").trim().to_string();
            Commit { subject, body }
        })
        .collect()
}

/// Detach HEAD at its current commit (`git checkout --detach`). Used before
/// re-pointing the checked-out branch, since git refuses to move a branch
/// that is currently checked out.
pub fn checkout_detach() -> Result<()> {
    run(&["checkout", "--detach"])
}

/// Force a branch ref to point at `start` (`git branch -f <name> <start>`),
/// creating it if absent. No checkout, no content force-push; just a local
/// ref move. `name`/`start` must not be `-`-leading (argument injection).
pub fn set_branch(name: &str, start: &str) -> Result<()> {
    reject_dash_lead("branch name", name)?;
    reject_dash_lead("start point", start)?;
    run(&["branch", "-f", name, start])
}

/// Raw `git --version` output (e.g. "git version 2.54.0"). Used by
/// `stack absorb` to gate on rebase `--update-refs` / `--autosquash` support.
pub fn version_string() -> Result<String> {
    output(&["--version"])
}

/// Resolve a revision to its full sha (`git rev-parse <rev>`).
pub fn rev_parse(rev: &str) -> Result<String> {
    reject_dash_lead("rev", rev)?;
    Ok(output(&["rev-parse", rev])?.trim().to_string())
}

/// True if `ancestor` is an ancestor of `descendant` (or they're equal).
/// `git merge-base --is-ancestor` exits 0/1; any spawn error reads as false.
pub fn is_ancestor(ancestor: &str, descendant: &str) -> bool {
    if reject_dash_lead("ancestor", ancestor).is_err()
        || reject_dash_lead("descendant", descendant).is_err()
    {
        return false;
    }
    let _foreground = crate::interrupt::ForegroundChild::enter();
    command()
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a `fixup!` commit for `target` consuming only `files` from the index
/// (`git commit --no-verify --fixup=<target> -- <files>`). The remaining index
/// entries are left staged. Used by `stack absorb`.
pub fn commit_fixup(target: &str, files: &[String]) -> Result<()> {
    reject_dash_lead("target", target)?;
    let mut args: Vec<String> = vec![
        "commit".into(),
        "--no-verify".into(),
        format!("--fixup={target}"),
        "--".into(),
    ];
    args.extend(files.iter().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs)
}

/// Non-interactively autosquash-rebase the current branch onto `base`,
/// rewriting fixups into their targets and moving every stacked branch ref
/// (`--update-refs`). `--autostash` sets aside any leftover index/worktree
/// changes and restores them afterward. The sequence editor is disabled so the
/// rebase never opens an editor.
pub fn rebase_autosquash_update_refs(base: &str) -> Result<()> {
    reject_dash_lead("base", base)?;
    let _foreground = crate::interrupt::ForegroundChild::enter();
    let status = command()
        .args([
            "-c",
            "sequence.editor=:",
            "rebase",
            "--interactive",
            "--autosquash",
            "--update-refs",
            "--autostash",
            base,
        ])
        .env("GIT_SEQUENCE_EDITOR", ":")
        .env("GIT_EDITOR", "true")
        .status()
        .with_context(|| "spawning git rebase")?;
    if !status.success() {
        bail!("git rebase --autosquash failed (status {status})");
    }
    Ok(())
}

/// Abort an in-progress rebase, best effort (`git rebase --abort`). Errors are
/// swallowed: it's only called on a recovery path.
pub fn rebase_abort() {
    let _foreground = crate::interrupt::ForegroundChild::enter();
    let _ = command().args(["rebase", "--abort"]).status();
}

/// A git repository addressed by path rather than by the process's current
/// directory. `stack ship`'s restack rebases and force-pushes several
/// branches, which must happen at the repo root (and, in tests, against a
/// throwaway repo), so those calls go through here instead of the free
/// functions above.
#[derive(Debug, Clone)]
pub struct Repo {
    dir: PathBuf,
}

impl Repo {
    /// Address the repository rooted at `dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Repo { dir: dir.into() }
    }

    /// The repository containing the process's current directory, resolved to
    /// its top level. Falls back to the current directory when `rev-parse`
    /// can't answer (the caller has already established it is in a repo).
    pub fn discover() -> Self {
        let dir = output(&["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string());
        Repo::at(dir)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = command();
        c.arg("-C").arg(&self.dir).args(args);
        c
    }

    /// Run git in this repo, surfacing stderr on a non-zero exit.
    fn run(&self, args: &[&str]) -> Result<()> {
        let out = self
            .command(args)
            .output()
            .with_context(|| "spawning git (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Run git in this repo and capture stdout.
    fn output(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command(args)
            .output()
            .with_context(|| "spawning git (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Resolve a revision to its full sha.
    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        reject_dash_lead("rev", rev)?;
        Ok(self.output(&["rev-parse", rev])?.trim().to_string())
    }

    /// True if `rev` resolves in this repo.
    pub fn rev_exists(&self, rev: &str) -> bool {
        if reject_dash_lead("rev", rev).is_err() {
            return false;
        }
        self.command(&["rev-parse", "--verify", "--quiet", rev])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// True if `ancestor` is an ancestor of `descendant` (or they're equal).
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        if reject_dash_lead("ancestor", ancestor).is_err()
            || reject_dash_lead("descendant", descendant).is_err()
        {
            return false;
        }
        self.command(&["merge-base", "--is-ancestor", ancestor, descendant])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Current branch name, or None when HEAD is detached.
    pub fn current_branch(&self) -> Option<String> {
        let out = self
            .command(&["rev-parse", "--abbrev-ref", "HEAD"])
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

    /// Check out `branch` in this repo.
    pub fn checkout(&self, branch: &str) -> Result<()> {
        reject_dash_lead("branch name", branch)?;
        self.run(&["checkout", branch])
    }

    /// Fetch one branch from `remote` and return the sha it fetched, without
    /// touching any local branch. Used to learn the trunk's tip after a PR
    /// merges server-side.
    pub fn fetch_branch(&self, remote: &str, branch: &str) -> Result<String> {
        reject_dash_lead("remote", remote)?;
        reject_dash_lead("branch name", branch)?;
        self.run(&["fetch", "--quiet", "--", remote, branch])?;
        self.rev_parse("FETCH_HEAD")
    }

    /// Replay `<upstream>..<branch>` onto `newbase`, moving every stacked
    /// branch ref that points inside the rewritten range (`--update-refs`, git
    /// 2.38+, the same plumbing `stack absorb` uses). `--autostash` sets aside
    /// and restores any uncommitted work. Leaves `branch` checked out.
    pub fn rebase_onto_update_refs(
        &self,
        newbase: &str,
        upstream: &str,
        branch: &str,
    ) -> Result<()> {
        reject_dash_lead("new base", newbase)?;
        reject_dash_lead("upstream", upstream)?;
        reject_dash_lead("branch name", branch)?;
        let out = self
            .command(&[
                "rebase",
                "--update-refs",
                "--autostash",
                "--onto",
                newbase,
                upstream,
                branch,
            ])
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GIT_EDITOR", "true")
            .output()
            .with_context(|| "spawning git rebase")?;
        if !out.status.success() {
            bail!(
                "git rebase --onto {newbase} {upstream} {branch} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Abort an in-progress rebase in this repo, best effort.
    pub fn rebase_abort(&self) {
        let _ = self.command(&["rebase", "--abort"]).output();
    }

    /// Force-push `branch` to `remote`, but only if the remote ref is still at
    /// `expect`. The lease is stated explicitly rather than left to the
    /// remote-tracking ref, so a branch that was never fetched can't be
    /// clobbered by a stale local view.
    pub fn push_force_with_lease(&self, remote: &str, branch: &str, expect: &str) -> Result<()> {
        reject_dash_lead("remote", remote)?;
        reject_dash_lead("branch name", branch)?;
        reject_dash_lead("lease", expect)?;
        let lease = force_with_lease_arg(branch, expect);
        let out = self
            .command(&["push", &lease, "--", remote, branch])
            .output()
            .with_context(|| "spawning git push")?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if is_stale_lease(&stderr) {
            return Err(anyhow::Error::new(StaleLease {
                branch: branch.to_string(),
                expected: expect.to_string(),
            }));
        }
        bail!("git push {remote} {branch} failed: {}", stderr.trim());
    }
}

/// A force-push git rejected because the remote ref no longer matches the
/// stated lease: something was pushed to the branch after fj last read it.
///
/// Typed rather than folded into a plain error string because the recovery is
/// the *opposite* of a transport failure's. A push that never reached the
/// server leaves the local refs authoritative and safe to re-push; a rejected
/// lease means the local refs are missing whatever landed on the remote, and
/// pushing harder there destroys it. Callers `downcast_ref` to tell the two
/// apart before offering advice.
#[derive(Debug)]
pub struct StaleLease {
    pub branch: String,
    pub expected: String,
}

impl std::fmt::Display for StaleLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to force-push '{}': the remote branch moved since fj last \
             saw it (expected {}). Fetch and re-check the branch, then re-run.",
            self.branch, self.expected
        )
    }
}

impl std::error::Error for StaleLease {}

/// `--force-with-lease=<ref>:<expected sha>`: push only if the remote ref is
/// still exactly where we think it is.
fn force_with_lease_arg(branch: &str, expect: &str) -> String {
    format!("--force-with-lease={branch}:{expect}")
}

/// Recognize git's rejection of a force-push whose lease no longer holds.
fn is_stale_lease(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("stale info") || (s.contains("[rejected]") && s.contains("force-with-lease"))
}

/// Soft-reset HEAD to `rev`, keeping the index and worktree (`git reset
/// --soft`). Used by `stack absorb` to undo fixup commits while re-staging
/// their contents after a failed rebase.
pub fn reset_soft(rev: &str) -> Result<()> {
    reject_dash_lead("rev", rev)?;
    run(&["reset", "--soft", rev])
}

/// Apply a unified-diff `patch` to the working tree, feeding it on stdin
/// (`git apply`). `check_only` runs `git apply --check` so callers can verify a
/// patch applies before committing to it. Surfaces git's stderr on rejection.
pub fn apply_patch(patch: &str, check_only: bool) -> Result<()> {
    let mut args = vec!["apply"];
    if check_only {
        args.push("--check");
    }
    let mut child = command()
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "spawning git apply")?;
    child
        .stdin
        .take()
        .context("git apply stdin unavailable")?
        .write_all(patch.as_bytes())
        .context("writing patch to git apply")?;
    let out = child
        .wait_with_output()
        .with_context(|| "waiting on git apply")?;
    if !out.status.success() {
        bail!(
            "git apply failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Stage everything (`git add -A`).
pub fn add_all() -> Result<()> {
    run(&["add", "-A"])
}

/// Commit the staged changes with `message`, skipping hooks. The `--message=`
/// form is used so a `-`-leading message can't be parsed as a flag.
pub fn commit(message: &str) -> Result<()> {
    run(&["commit", "--no-verify", &format!("--message={message}")])
}

/// `(ahead, behind)` of HEAD relative to its upstream `@{u}`. None when there
/// is no upstream configured or we aren't in a repo.
pub fn ahead_behind_upstream() -> Option<(u32, u32)> {
    let out = command()
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
    use super::{
        parse_left_right_count, parse_log_records, parse_porcelain, reject_dash_lead, Commit,
    };

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
    fn detects_non_fast_forward_pushes() {
        use super::is_non_fast_forward;
        assert!(is_non_fast_forward(
            " ! [rejected]        main -> main (non-fast-forward)\nerror: failed to push some refs"
        ));
        assert!(is_non_fast_forward("Updates were rejected; fetch first"));
        assert!(!is_non_fast_forward("Everything up-to-date"));
        assert!(!is_non_fast_forward("Permission denied (publickey)"));
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

    #[test]
    fn log_records_split_subject_and_multiline_body() {
        // Two commits, oldest first; the second carries a multi-line body.
        let raw = "\x1efix: typo\x1f\x1efeat: thing\x1fwhy it matters\n\nmore detail\n";
        let commits = parse_log_records(raw);
        assert_eq!(
            commits,
            vec![
                Commit {
                    subject: "fix: typo".into(),
                    body: String::new(),
                },
                Commit {
                    subject: "feat: thing".into(),
                    body: "why it matters\n\nmore detail".into(),
                },
            ]
        );
    }

    #[test]
    fn log_records_empty_when_no_commits() {
        assert!(parse_log_records("").is_empty());
        assert!(parse_log_records("\n").is_empty());
    }

    #[test]
    fn lease_arg_names_the_ref_and_the_expected_sha() {
        use super::force_with_lease_arg;
        assert_eq!(
            force_with_lease_arg("u/feat-2", "abc123"),
            "--force-with-lease=u/feat-2:abc123"
        );
    }

    #[test]
    fn detects_a_stale_force_with_lease() {
        use super::is_stale_lease;
        assert!(is_stale_lease(
            " ! [rejected]  u/feat-2 -> u/feat-2 (stale info)"
        ));
        assert!(is_stale_lease(
            " ! [rejected] u/f -> u/f (force-with-lease rejected)"
        ));
        assert!(!is_stale_lease("Everything up-to-date"));
        assert!(!is_stale_lease("Permission denied (publickey)"));
    }
}

/// Real-git tests for the restack plumbing `stack ship` uses after a squash
/// land. They build a throwaway repository (and a bare "remote") in a temp
/// dir, so they exercise the actual rebase/force-push semantics rather than
/// just the argv. Skipped when git is missing or older than 2.38.
#[cfg(test)]
mod repo_tests {
    use super::Repo;
    use crate::gitfixture::{
        commit_file, count, git, git_supports_update_refs, published_stack, stacked_repo,
    };

    #[test]
    fn restack_reparents_every_branch_above_a_squash_land() {
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (_tmp, work, tip1) = stacked_repo();

        // Squash-land u/s-1: the trunk gains a *new* commit carrying the same
        // tree, exactly what a squash merge produces server-side.
        git(&work, &["checkout", "--quiet", "main"]);
        let trunk_tip = commit_file(&work, "a.txt", "a", "item 1 (#1)");
        assert_ne!(trunk_tip, tip1);

        let repo = Repo::at(&work);
        repo.rebase_onto_update_refs(&trunk_tip, &tip1, "u/s-3")
            .expect("restack should replay cleanly");

        // The landed branch is left alone; both branches above it now descend
        // from the new trunk tip, chained, one commit each.
        assert_eq!(repo.rev_parse("u/s-1").expect("u/s-1"), tip1);
        assert!(repo.is_ancestor(&trunk_tip, "u/s-2"));
        assert!(repo.is_ancestor("u/s-2", "u/s-3"));
        assert_eq!(count(&work, &format!("{trunk_tip}..u/s-2")), 1);
        assert_eq!(count(&work, "u/s-2..u/s-3"), 1);
        // The pre-squash commit is gone from the stack above the trunk: this is
        // the footgun (remaining branches still based on the squashed commit).
        assert!(!repo.is_ancestor(&tip1, "u/s-3"));
    }

    #[test]
    fn restack_aborts_and_leaves_the_branches_alone_on_conflict() {
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (_tmp, work, tip1) = stacked_repo();
        let before_2 = git(&work, &["rev-parse", "u/s-2"]);
        let before_3 = git(&work, &["rev-parse", "u/s-3"]);

        // Land something on the trunk that collides with u/s-2's commit.
        git(&work, &["checkout", "--quiet", "main"]);
        commit_file(&work, "a.txt", "a", "item 1 (#1)");
        let trunk_tip = commit_file(&work, "b.txt", "conflicting", "someone else");

        let repo = Repo::at(&work);
        let err = repo
            .rebase_onto_update_refs(&trunk_tip, &tip1, "u/s-3")
            .expect_err("conflicting replay should fail");
        assert!(err.to_string().contains("git rebase --onto"));
        repo.rebase_abort();

        assert_eq!(git(&work, &["rev-parse", "u/s-2"]), before_2);
        assert_eq!(git(&work, &["rev-parse", "u/s-3"]), before_3);
    }

    #[test]
    fn fetch_branch_reads_the_remote_tip_without_moving_local_refs() {
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (tmp, work, bare, _tip1) = published_stack();
        let local_main = git(&work, &["rev-parse", "main"]);

        // Advance the remote's main behind our back, the way a merge does.
        let landed = crate::gitfixture::squash_land_on_remote(tmp.path(), &bare);

        let repo = Repo::at(&work);
        assert_eq!(repo.fetch_branch("origin", "main").expect("fetch"), landed);
        // The local branch is untouched: fetch_branch only reports the tip.
        assert_eq!(git(&work, &["rev-parse", "main"]), local_main);
    }

    #[test]
    fn force_with_lease_push_honors_the_stated_lease() {
        if !git_supports_update_refs() {
            eprintln!("skipping: needs git >= 2.38");
            return;
        }
        let (_tmp, work, bare, _tip1) = published_stack();
        let published = git(&work, &["rev-parse", "u/s-2"]);

        // Rewrite u/s-2 locally, as a restack does.
        git(&work, &["checkout", "--quiet", "u/s-2"]);
        git(
            &work,
            &[
                "commit",
                "--amend",
                "--no-verify",
                "-m",
                "item 2, restacked",
            ],
        );
        let rewritten = git(&work, &["rev-parse", "u/s-2"]);
        assert_ne!(rewritten, published);

        let repo = Repo::at(&work);
        // A lease naming the wrong sha is refused, and the remote is untouched.
        let wrong = git(&work, &["rev-parse", "main"]);
        let err = repo
            .push_force_with_lease("origin", "u/s-2", &wrong)
            .expect_err("a wrong lease must be refused");
        assert!(err.to_string().contains("moved since"));
        assert_eq!(git(&bare, &["rev-parse", "u/s-2"]), published);

        // The true lease goes through.
        repo.push_force_with_lease("origin", "u/s-2", &published)
            .expect("push with the right lease");
        assert_eq!(git(&bare, &["rev-parse", "u/s-2"]), rewritten);
    }
}
