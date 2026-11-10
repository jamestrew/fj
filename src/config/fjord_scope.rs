//! Profile-scoped refresh and instance publication.
use super::fjord_profiles::{load, verify, write, Scope, Session};
use super::profiles;
use crate::fjord::oidc;
use anyhow::{anyhow, Result};
impl Scope {
    pub fn platform(&self) -> &str {
        &self.profile.fjord.as_ref().unwrap().platform_url
    }
    pub async fn refresh(&self, bearer: &str) -> Result<Option<String>> {
        let _guard = lock(&self.host, &self.profile)?;
        let hosts = super::hosts::Hosts::load()?;
        if profiles::selection(&hosts, &self.host, Some(&self.profile.user))?
            .as_ref()
            .map(|p| &p.generation)
            != Some(&self.profile.generation)
        {
            return Err(anyhow!("profile session changed; retry command"));
        }
        let stored = load(&self.host, &self.profile)?;
        if oidc::bearer_identity(&stored.bearer) != oidc::bearer_identity(bearer) {
            return Err(anyhow!("profile session changed; log in again"));
        }
        if stored.bearer != bearer {
            verify(self.platform(), &stored.bearer, &self.profile.user).await?;
            return Ok(Some(stored.bearer));
        }
        let Some(refresh) = stored.refresh.as_deref() else {
            return Ok(None);
        };
        let tokens = oidc::exchange_refresh(self.platform(), &oidc::client_id(), refresh)
            .await
            .map_err(|e| {
                if e.downcast_ref::<crate::fjord::UnrenewableGrant>().is_some() {
                    crate::fjord::UnrenewableGrant.into()
                } else {
                    anyhow!("profile refresh failed")
                }
            })?;
        if oidc::bearer_identity(&tokens.access_token) != oidc::bearer_identity(bearer)
            || !oidc::bearer_email(&tokens.access_token)
                .is_some_and(|u| u.eq_ignore_ascii_case(&self.profile.user))
        {
            return Err(anyhow!("refreshed profile identity mismatch"));
        }
        let next = Session {
            bearer: tokens.access_token,
            refresh: tokens.refresh_token.or(stored.refresh),
        };
        verify(self.platform(), &next.bearer, &self.profile.user).await?;
        write(&self.host, &self.profile, &next)?;
        Ok(Some(next.bearer))
    }
    pub fn select_instance(&self, target: &crate::fjord::ForgeInstance) -> Result<()> {
        super::profile_publish::edit(|hosts| {
            let current = profiles::selection(hosts, &self.host, Some(&self.profile.user))?
                .ok_or_else(|| anyhow!("profile disappeared"))?;
            let host = hosts
                .hosts
                .get(&self.host)
                .ok_or_else(|| anyhow!("host disappeared"))?;
            if serde_json::to_value(&current)? != serde_json::to_value(&self.profile)?
                || host.auth_kind != self.destination.auth_kind
                || host.web_url != self.destination.web_url
                || host.api_base_path != self.destination.api_base_path
                || host.proxy != self.destination.proxy
                || host.fjord_account.as_ref().map(|a| &a.platform_url)
                    != self
                        .destination
                        .fjord_account
                        .as_ref()
                        .map(|a| &a.platform_url)
            {
                return Err(anyhow!(
                    "profile destination changed; retry instance selection"
                ));
            }
            let p = hosts
                .accounts
                .get_mut(&self.host)
                .unwrap()
                .profiles
                .get_mut(&self.profile.user)
                .unwrap();
            let fa = p.fjord.as_mut().unwrap();
            fa.default_instance_id = Some(target.id.clone());
            fa.default_instance_display_name = Some(target.display_name.clone());
            fa.default_instance_slug = Some(target.slug.clone());
            fa.default_instance_public_url = target.public_url.clone();
            Ok(())
        })
    }
}

pub fn resolve(host: &str, config: &mut super::hosts::Host) -> Result<Option<String>> {
    if config.selected_account.is_some() {
        let profile = config
            .selected_profile
            .as_ref()
            .ok_or_else(|| anyhow!("requested Fjord profile is unavailable"))?;
        config.fjord_account = Some(
            profile
                .fjord
                .clone()
                .ok_or_else(|| anyhow!("profile auth kind mismatch"))?,
        );
        return load(host, profile).map(|s| Some(s.bearer));
    }
    let fa = config
        .fjord_account
        .as_ref()
        .ok_or_else(|| anyhow!("missing Fjord platform config"))?;
    crate::auth::load_fjord_bearer(&fa.platform_url)?
        .map(Some)
        .ok_or_else(|| anyhow!("no Fjord session available; run fj auth login --fjord"))
}

pub(super) fn lock(
    host: &str,
    profile: &super::profiles::Profile,
) -> Result<crate::fjord::lock::RefreshLock> {
    let identity = serde_json::to_vec(&(host, &profile.user))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &identity);
    let name: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    crate::fjord::lock::acquire(&name)
}
