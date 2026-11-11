//! Destination inference for a pinned `fj.resolved` slug.
use super::remote::list_remotes;
use super::{discover, parse_remote, RemoteRepo};
use anyhow::Result;

/// The clone's remote for a pinned slug. A clone with no remote yields `None`
/// (callers use the default host). Any remote that cannot be parsed is an
/// error, so a pinned slug never lands on another forge through a fallback
/// remote. An explicit `--host` keeps the lenient lookup.
pub fn pinned_remote(preferred_host: Option<&str>, explicit: bool) -> Result<Option<RemoteRepo>> {
    if explicit {
        return Ok(discover(preferred_host).ok());
    }
    let remotes = list_remotes()?;
    for (_, url) in &remotes {
        parse_remote(url).map_err(|e| e.context("pinned repository: cannot infer the instance"))?;
    }
    if remotes.is_empty() {
        return Ok(None);
    }
    discover(None).map(Some)
}
