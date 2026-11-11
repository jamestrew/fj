//! Replaced credential generations whose secrets are not yet confirmed deleted.
//!
//! Replacing a profile publishes a new generation under a new random key. The
//! old generation's descriptor moves to `HostProfiles::retired` in the same
//! publication, so its identity survives a failed delete and a later store,
//! login or logout retries it. Nothing selects a retired generation.
use super::{hosts::Hosts, profile_publish, profiles::Profile};
use crate::auth;
use anyhow::{anyhow, Result};

/// Replaced credentials that could not be deleted yet. Callers that sanitize
/// errors let this one through: it names no account and tells the user the retry.
#[derive(Debug)]
pub struct CleanupPending(usize);
impl std::fmt::Display for CleanupPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} previous credential(s) could not be removed yet; running this command again retries",
            self.0
        )
    }
}
impl std::error::Error for CleanupPending {}

/// Every secret key a generation of `profile` can hold.
pub(super) fn secret_keys(host: &str, profile: &Profile) -> Vec<String> {
    if profile.fjord.is_some() {
        let base = super::fjord_profiles::key(host, profile);
        return ["bearer", "refresh"].map(|k| format!("{base}:{k}")).into();
    }
    vec![super::profiles::key(host, profile)]
}

/// Delete the secrets of every retired generation on `host`, dropping each one
/// from the list only once all its secrets are gone.
pub fn drain(host: &str) -> Result<()> {
    drain_with(host, auth::delete_token)
}

fn drain_with(host: &str, delete: impl Fn(&str) -> Result<()>) -> Result<()> {
    let pending: Vec<Profile> = Hosts::load()?
        .accounts
        .get(host)
        .map(|set| set.retired.clone())
        .unwrap_or_default();
    let mut gone = Vec::new();
    let mut failed = 0;
    for profile in pending {
        if delete_all(host, &profile, &delete) {
            gone.push((profile.user, profile.generation));
        } else {
            failed += 1;
        }
    }
    if !gone.is_empty() {
        profile_publish::edit(|hosts| {
            if let Some(set) = hosts.accounts.get_mut(host) {
                set.retired
                    .retain(|p| !gone.contains(&(p.user.clone(), p.generation.clone())));
            }
            Ok(())
        })?;
    }
    match failed {
        0 => Ok(()),
        n => Err(anyhow::Error::new(CleanupPending(n))),
    }
}

/// Attempt every secret of the generation, even after one fails: a refresh
/// credential must not outlive a retry just because its bearer would not delete.
fn delete_all(host: &str, profile: &Profile, delete: &impl Fn(&str) -> Result<()>) -> bool {
    secret_keys(host, profile)
        .iter()
        .map(|k| delete(k).is_ok())
        .fold(true, |all, ok| all && ok)
}

/// A staged generation that never got published and whose delete failed: keep
/// its identity so a later run removes it. Best effort, the caller is already
/// returning the original error. Only for a host that is configured: listing a
/// generation under a host that does not exist would leave an account entry
/// with no host, which every later save rejects. A first login that fails this
/// way leaves its staged secret unrecorded.
pub(super) fn keep_unremoved(host: &str, profile: &Profile) {
    let _ = profile_publish::edit(|hosts| {
        if hosts.hosts.contains_key(host) {
            hosts
                .accounts
                .entry(host.into())
                .or_default()
                .retired
                .push(profile.clone());
        }
        Ok(())
    });
}

/// Delete a staged, unpublished generation's secrets, retaining it if that fails.
pub(super) fn abandon(host: &str, profile: &Profile) {
    if !delete_all(host, profile, &auth::delete_token) {
        keep_unremoved(host, profile);
    }
}

#[cfg(test)]
#[path = "profile_retired_tests.rs"]
mod tests;
