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
    let mut args = vec!["clone", url];
    if let Some(d) = dest {
        args.push(d);
    }
    run(&args)
}

pub fn fetch_pr(remote: &str, number: u64, local_branch: &str) -> Result<()> {
    let refspec = format!("pull/{number}/head:{local_branch}");
    run(&["fetch", remote, &refspec])
}

pub fn checkout(branch: &str) -> Result<()> {
    run(&["checkout", branch])
}
