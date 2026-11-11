//! Destination inference for a pinned `fj.resolved` slug.
use super::remote::list_remotes;
use super::{discover, parse_remote, RemoteRepo};
use anyhow::Result;

/// The clone's remote for a pinned slug. A clone with no remote yields `None`
/// (callers use the default host). The highest-priority remote must parse, so a
/// pinned slug never lands on another forge through a fallback remote.
/// An explicit `--host` keeps the lenient lookup.
pub fn pinned_remote(preferred_host: Option<&str>, explicit: bool) -> Result<Option<RemoteRepo>> {
    if explicit {
        return Ok(discover(preferred_host).ok());
    }
    let mut remotes = list_remotes()?;
    // Same priority as `discover`: upstream, origin, then the rest.
    remotes.sort_by_key(|(name, _)| match name.as_str() {
        "upstream" => 0,
        "origin" => 1,
        _ => 2,
    });
    let Some((_, url)) = remotes.first() else {
        return Ok(None);
    };
    let parsed = parse_remote(url);
    parsed
        .map(Some)
        .map_err(|e| e.context("pinned repository: cannot infer the instance"))
}
