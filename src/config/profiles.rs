//! PAT descriptors and immutable credential generations, scoped to exact hosts.
use super::hosts::{AuthKind, Host, Hosts};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub user: String,
    pub generation: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct HostProfiles {
    pub default_account: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
}
impl std::fmt::Debug for HostProfiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Profiles")
            .field("count", &self.profiles.len())
            .field("has_default", &self.default_account.is_some())
            .finish()
    }
}
pub fn canonical(raw: &str) -> Result<String> {
    if raw.trim().is_empty() || raw.chars().any(char::is_control) {
        return Err(anyhow!(
            "account must be nonempty and contain no control characters"
        ));
    }
    Ok(raw.trim().to_ascii_lowercase())
}
pub fn generic_env(hosts: &Hosts, name: &str, flag: Option<&str>) -> bool {
    flag.is_none() || matches!(hosts.resolve_host(None), Ok(default) if default == name)
}
pub fn selection(hosts: &Hosts, host: &str, account: Option<&str>) -> Result<Option<Profile>> {
    let set = hosts.accounts.get(host);
    let selected = account
        .map(str::to_ascii_lowercase)
        .or_else(|| set.and_then(|s| s.default_account.clone()));
    let profile = selected
        .and_then(|a| set.and_then(|s| s.profiles.get(&a)))
        .cloned();
    if account.is_none() && set.is_some_and(|s| s.default_account.is_some()) && profile.is_none() {
        return Err(anyhow!("configured default account profile is missing"));
    }
    Ok(profile)
}
fn hex(value: &str) -> String {
    value.bytes().map(|b| format!("{b:02x}")).collect()
}
pub fn key(host: &str, profile: &Profile) -> String {
    format!(
        "profile:{}:{}:pat:{}",
        hex(host),
        hex(&profile.user),
        profile.generation
    )
}
pub fn token(host: &str, profile: &Profile) -> Result<String> {
    crate::auth::load_token(&key(host, profile))
        .map_err(|_| anyhow!("profile credential read failed"))?
        .ok_or_else(|| anyhow!("selected profile credential is unavailable; log in again"))
}
pub fn has_profiles(hosts: &Hosts, host: &str) -> bool {
    hosts.accounts.contains_key(host)
}
pub fn status(hosts: &Hosts, host: &str) -> String {
    let set = hosts.accounts.get(host);
    format!(
        "{host}: {} PAT profile(s); default: {}",
        set.map_or(0, |s| s.profiles.len()),
        if set.is_some_and(|s| s.default_account.is_some()) {
            "profile [redacted]"
        } else {
            "legacy"
        }
    )
}
pub fn store(host: &str, user: &str, value: &str, candidate: Host) -> Result<()> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| anyhow!("could not stage profile credentials"))?;
    let profile = Profile {
        user: user.to_ascii_lowercase(),
        generation: nonce.iter().map(|b| format!("{b:02x}")).collect(),
    };
    let key = key(host, &profile);
    stage(
        &key,
        value,
        |k, v| {
            crate::auth::store_token(k, v)
                .map(|_| ())
                .map_err(|_| anyhow!("profile credential write failed"))
        },
        |k| crate::auth::load_token(k).map_err(|_| anyhow!("profile credential read failed")),
    )?;
    super::profile_publish::edit(|hosts| {
        if let Some(current) = hosts.hosts.get(host) {
            if current.auth_kind != AuthKind::Pat
                || current.web_url != candidate.web_url
                || current.api_base_path != candidate.api_base_path
            {
                return Err(anyhow!("host configuration changed; retry profile login"));
            }
        } else {
            hosts.upsert(host, candidate);
        }
        if token(host, &profile)? != value {
            return Err(anyhow!("profile credential read-back mismatch"));
        }
        let legacy = hosts.hosts.get(host).and_then(|h| h.user.as_deref());
        let set = hosts.accounts.entry(host.into()).or_default();
        if set.default_account.is_none() && legacy.is_some_and(|u| u.eq_ignore_ascii_case(user)) {
            set.default_account = Some(profile.user.clone());
        }
        set.profiles.insert(profile.user.clone(), profile);
        Ok(())
    })
}
fn stage(
    key: &str,
    value: &str,
    mut write: impl FnMut(&str, &str) -> Result<()>,
    mut read: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<()> {
    write(key, value)?;
    if read(key)?.as_deref() != Some(value) {
        return Err(anyhow!("profile credential read-back mismatch"));
    }
    Ok(())
}
pub fn switch(host: &str, verified: &Profile, destination: &Host) -> Result<()> {
    super::profile_publish::edit(|hosts| {
        let actual = hosts
            .hosts
            .get(host)
            .ok_or_else(|| anyhow!("host configuration changed; retry switch"))?;
        if actual.web_url != destination.web_url
            || actual.api_base_path != destination.api_base_path
            || actual.auth_kind != destination.auth_kind
            || actual.proxy != destination.proxy
        {
            return Err(anyhow!("host configuration changed; retry switch"));
        }
        let current = selection(hosts, host, Some(&verified.user))?
            .ok_or_else(|| anyhow!("profile is missing"))?;
        if current.generation != verified.generation {
            return Err(anyhow!("profile changed; retry switch"));
        }
        token(host, &current)?;
        hosts.accounts.get_mut(host).unwrap().default_account = Some(current.user);
        hosts.current = Some(host.into());
        Ok(())
    })
}
#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
