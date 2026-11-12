//! Web-session cookies bound to one account profile and one destination instance.
use super::{
    hosts::{Host, Hosts},
    profile_publish,
    profiles::{self, Profile},
};
use crate::auth;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// The published pointer to the active cookie slot. Each account has two fixed
/// slots that alternate, so a replacement is staged without touching the active
/// cookie and a credential rotation never moves it. `scope` pins the destination
/// the cookie was imported for, so a moved instance never receives it.
#[derive(Clone, Serialize, Deserialize)]
pub struct WebSession {
    pub slot: u8,
    pub scope: String,
}
fn scope(host: &str, config: &Host, profile: &Profile) -> String {
    let (id, url) = match profile.fjord.as_ref() {
        Some(f) => (&f.default_instance_id, &f.default_instance_public_url),
        None => (&None, &config.web_url),
    };
    format!(
        "{host}|{}|{}",
        id.as_deref().unwrap_or_default(),
        url.as_deref().unwrap_or_default()
    )
}
fn slot(host: &str, user: &str, n: u8) -> String {
    format!(
        "profile:{}:{}:web:{n}",
        profiles::hex(host),
        profiles::hex(user)
    )
}
fn current(hosts: &Hosts, host: &str, user: &str) -> Result<(Profile, Host)> {
    let profile = profiles::selection(hosts, host, Some(user))?
        .ok_or_else(|| anyhow!("requested account profile is unavailable"))?;
    let config = hosts
        .hosts
        .get(host)
        .cloned()
        .ok_or_else(|| anyhow!("host configuration changed; retry"))?;
    Ok((profile, config))
}
fn fenced(hosts: &Hosts, host: &str, seen: &Profile) -> Result<Profile> {
    profiles::selection(hosts, host, Some(&seen.user))?
        .filter(|p| p.generation == seen.generation)
        .ok_or_else(|| anyhow!("profile changed; retry"))
}
fn active_slot(hosts: &Hosts, host: &str, user: &str) -> Option<u8> {
    hosts.accounts.get(host)?.sessions.get(user).map(|s| s.slot)
}
fn delete_slots(host: &str, user: &str) -> Result<()> {
    [0, 1]
        .into_iter()
        .try_for_each(|n| auth::delete_token(&slot(host, user, n)))
}
/// Stage the cookie in the inactive slot, read it back, then flip the pointer.
/// A failure before publication leaves the active cookie untouched.
pub fn import(host: &str, user: &str, cookie: &str) -> Result<()> {
    let (profile, config) = current(&Hosts::load()?, host, user)?;
    let _guard = super::fjord_scope::lock(host, &profile)?;
    let hosts = Hosts::load()?;
    let seen = active_slot(&hosts, host, &profile.user);
    let next = WebSession {
        slot: seen.map_or(0, |n| 1 - n),
        scope: scope(host, &config, &profile),
    };
    let staged = slot(host, &profile.user, next.slot);
    let publish = || -> Result<()> {
        auth::store_token(&staged, cookie).map_err(|_| anyhow!("web session write failed"))?;
        let read = auth::load_token(&staged).map_err(|_| anyhow!("web session read failed"))?;
        if read.as_deref() != Some(cookie) {
            return Err(anyhow!("web session read-back mismatch"));
        }
        profile_publish::edit(|hosts| {
            let now = fenced(hosts, host, &profile)?;
            let same = hosts.hosts.get(host).map(|c| scope(host, c, &now));
            if same.as_ref() != Some(&next.scope) || active_slot(hosts, host, &profile.user) != seen
            {
                return Err(anyhow!("profile or destination changed; retry"));
            }
            let set = hosts
                .accounts
                .get_mut(host)
                .ok_or_else(|| anyhow!("profile is missing"))?;
            set.sessions.insert(profile.user.clone(), next.clone());
            Ok(())
        })
    };
    publish().inspect_err(|_| {
        let _ = auth::delete_token(&staged);
    })?;
    // The replaced cookie sits in a known slot; if this fails, importing again retries it.
    seen.map_or(Ok(()), |n| {
        auth::delete_token(&slot(host, &profile.user, n))
    })
}
/// Delete the cookie secrets, then unpublish the pointer, so a failed delete is retried.
pub fn clear(host: &str, user: &str) -> Result<()> {
    let (profile, _) = current(&Hosts::load()?, host, user)?;
    let _guard = super::fjord_scope::lock(host, &profile)?;
    fenced(&Hosts::load()?, host, &profile)?;
    delete_slots(host, &profile.user)?;
    profile_publish::edit(|hosts| {
        fenced(hosts, host, &profile)?;
        hosts
            .accounts
            .get_mut(host)
            .and_then(|s| s.sessions.remove(&profile.user));
        Ok(())
    })
}
/// The cookie for the selected profile, only while its destination is unchanged.
pub fn cookie(hosts: &Hosts, host: &str, user: &str) -> Result<Option<String>> {
    let (profile, config) = current(hosts, host, user)?;
    match hosts
        .accounts
        .get(host)
        .and_then(|s| s.sessions.get(&profile.user))
    {
        Some(s) if s.scope == scope(host, &config, &profile) => {
            auth::load_token(&slot(host, &profile.user, s.slot))
        }
        _ => Ok(None),
    }
}
/// Profiles on `host` whose imported cookie still matches their destination.
pub fn active(hosts: &Hosts, host: &str) -> usize {
    let (Some(config), Some(set)) = (hosts.hosts.get(host), hosts.accounts.get(host)) else {
        return 0;
    };
    set.sessions
        .iter()
        .filter(|(user, s)| {
            set.profiles
                .get(*user)
                .is_some_and(|p| s.scope == scope(host, config, p))
        })
        .count()
}
/// Delete the profile's secrets, then remove it from the active set. The default
/// pointer stays, so a logged-out default never falls back to legacy credentials.
/// A failed delete leaves the profile selectable, so logging out again finishes it.
pub fn logout(host: &str, seen: &Profile) -> Result<()> {
    let _guard = super::fjord_scope::lock(host, seen)?;
    fenced(&Hosts::load()?, host, seen)?;
    super::profile_retired::secret_keys(host, seen)
        .iter()
        .try_for_each(|k| auth::delete_token(k))?;
    delete_slots(host, &seen.user)?;
    // Earlier replaced generations that a failed delete left behind go before the
    // profile is unpublished, so a failure here leaves it selectable and logging
    // out again retries.
    super::profile_retired::drain(host)?;
    profile_publish::edit(|hosts| {
        fenced(hosts, host, seen)?;
        let set = hosts
            .accounts
            .get_mut(host)
            .ok_or_else(|| anyhow!("profile is missing"))?;
        set.profiles.remove(&seen.user);
        set.sessions.remove(&seen.user);
        Ok(())
    })
}
