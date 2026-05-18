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
    use super::reject_dash_lead;

    #[test]
    fn rejects_dash_lead() {
        assert!(reject_dash_lead("branch", "--upload-pack=evil").is_err());
        assert!(reject_dash_lead("branch", "-x").is_err());
        assert!(reject_dash_lead("branch", "pr-42").is_ok());
        assert!(reject_dash_lead("branch", "feature/x").is_ok());
    }
}
