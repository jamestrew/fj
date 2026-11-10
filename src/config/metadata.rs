//! Resolve profile metadata without reading credentials or requiring an instance.
use super::{
    hosts::{AuthKind, Host, Hosts},
    profiles,
};
use anyhow::{anyhow, Result};
pub fn apply(hosts: &Hosts, name: &str, config: &mut Host, account: Option<&str>) -> Result<()> {
    let canonical = account.map(profiles::canonical).transpose()?;
    let profile = profiles::selection(hosts, name, canonical.as_deref())?;
    if profile
        .as_ref()
        .is_some_and(|p| p.fjord.is_some() != (config.auth_kind == AuthKind::FjordAccount))
    {
        return Err(anyhow!("profile auth kind mismatch"));
    }
    config.selected_account = canonical
        .or_else(|| profile.as_ref().map(|p| p.user.clone()))
        .map(|u| u.parse().unwrap());
    if config.auth_kind == AuthKind::FjordAccount && config.selected_account.is_some() {
        let metadata = profile
            .as_ref()
            .and_then(|p| p.fjord.clone())
            .ok_or_else(|| anyhow!("requested Fjord profile is unavailable"))?;
        if config.fjord_account.as_ref().map(|f| &f.platform_url) != Some(&metadata.platform_url) {
            return Err(anyhow!("profile platform mismatch"));
        }
        config.fjord_account = Some(metadata);
    }
    if config.selected_account.is_some() {
        config.user = Some("[redacted account]".into());
    }
    config.selected_profile = profile;
    Ok(())
}
pub fn resolve(hosts: &Hosts, name: &str) -> Result<Host> {
    let mut config = hosts.hosts.get(name).cloned().unwrap_or_default();
    let account = crate::client::account::selected();
    apply(
        hosts,
        name,
        &mut config,
        account.as_ref().map(|a| a.as_str()),
    )?;
    Ok(config)
}
pub fn mute_scope(name: &str, config: Option<&Host>, legacy: String) -> Result<String> {
    let Some(config) = config.filter(|h| h.selected_account.is_some()) else {
        return Ok(legacy);
    };
    let identity = serde_json::to_vec(&(
        name,
        config.selected_account.as_ref().unwrap().as_str(),
        config
            .fjord_account
            .as_ref()
            .and_then(|f| f.default_instance_id.as_deref()),
    ))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &identity);
    Ok(format!(
        "profile:{}",
        digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}
#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
