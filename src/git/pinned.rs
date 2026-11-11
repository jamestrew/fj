//! Destination inference for a pinned `fj.resolved` slug.
use super::{discover, RemoteRepo};
use anyhow::Result;

/// The clone's remote for a pinned slug. A clone with no remote yields `None`
/// (callers use the default host); remotes that exist but cannot be parsed are
/// an error, so a pinned slug never reaches an unrelated instance.
pub fn pinned_remote(preferred_host: Option<&str>, explicit: bool) -> Result<Option<RemoteRepo>> {
    match discover(preferred_host) {
        Ok(r) => Ok(Some(r)),
        Err(_) if explicit || !has_remotes() => Ok(None),
        Err(e) => Err(e.context("pinned repository: cannot infer the instance")),
    }
}

fn has_remotes() -> bool {
    super::command()
        .arg("remote")
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
}
