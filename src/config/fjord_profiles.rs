//! Named Fjord sessions retain network destinations separately from storage identities.
pub use super::fjord_scope::resolve;
use super::{
    hosts::{FjordAccount, Host, Hosts},
    profiles::{self, Profile},
};
use crate::{auth, fjord::oidc};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

pub struct Session {
    pub bearer: String,
    pub refresh: Option<String>,
}
#[derive(Clone)]
pub struct Scope {
    pub host: String,
    pub profile: Profile,
    pub destination: Host,
}
pub(super) fn key(host: &str, profile: &Profile) -> String {
    format!("{}:fjord-session", profiles::key(host, profile))
}
#[derive(Serialize, Deserialize)]
struct Refresh {
    identity: String,
    token: Option<String>,
}
pub fn load(host: &str, profile: &Profile) -> Result<Session> {
    let read = |kind: &str| {
        auth::load_token(&format!("{}:{kind}", key(host, profile)))
            .map_err(|_| anyhow!("profile session read failed"))?
            .ok_or_else(|| anyhow!("profile session unavailable; log in again"))
    };
    let bearer = read("bearer")?;
    let record: Refresh = serde_json::from_str(&read("refresh")?)
        .map_err(|_| anyhow!("invalid profile refresh record"))?;
    if !oidc::bearer_email(&bearer).is_some_and(|u| u.eq_ignore_ascii_case(&profile.user))
        || oidc::bearer_identity(&bearer).as_deref() != Some(record.identity.as_str())
    {
        return Err(anyhow!("profile session identity mismatch"));
    }
    Ok(Session {
        bearer,
        refresh: record.token,
    })
}
pub(super) fn write(host: &str, profile: &Profile, session: &Session) -> Result<()> {
    let identity = oidc::bearer_identity(&session.bearer)
        .ok_or_else(|| anyhow!("missing session identity"))?;
    let record = serde_json::to_string(&Refresh {
        identity,
        token: session.refresh.clone(),
    })?;
    // Persist rotated refresh first so an interrupted bearer write does not lose the new grant.
    for (kind, value) in [
        ("refresh", record.as_str()),
        ("bearer", session.bearer.as_str()),
    ] {
        let slot = format!("{}:{kind}", key(host, profile));
        auth::store_token(&slot, value).map_err(|_| anyhow!("profile session write failed"))?;
        if auth::load_token(&slot)
            .map_err(|_| anyhow!("profile session read failed"))?
            .as_deref()
            != Some(value)
        {
            return Err(anyhow!("profile session read-back mismatch"));
        }
    }
    Ok(())
}
pub async fn verify(platform: &str, bearer: &str, account: &str) -> Result<()> {
    // A decoded payload alone is not proof. Require the platform to accept the bearer.
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()?
        .get(format!("{platform}/api/v1/me/forge-instances"))
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|_| anyhow!("profile identity verification failed"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(crate::fjord::SessionExpired.into());
    }
    if response.status() != reqwest::StatusCode::OK
        || !oidc::bearer_email(bearer).is_some_and(|u| u.eq_ignore_ascii_case(account))
        || oidc::bearer_identity(bearer).is_none()
    {
        return Err(anyhow!("profile identity verification failed"));
    }
    Ok(())
}
pub async fn login(
    platform: &str,
    account: &str,
    tokens: &oidc::TokenResponse,
    protocol: &str,
) -> Result<()> {
    let hosts = Hosts::load()?;
    let original = hosts.hosts.get(platform).cloned();
    if original.as_ref().is_some_and(|h| {
        h.auth_kind != super::hosts::AuthKind::FjordAccount
            || h.fjord_account.as_ref().map(|a| a.platform_url.as_str()) != Some(platform)
    }) {
        return Err(anyhow!("host auth kind mismatch"));
    }
    verify(platform, &tokens.access_token, account).await?;
    let prior = profiles::selection(&hosts, platform, Some(account))?;
    let migrating = original
        .as_ref()
        .and_then(|h| h.user.as_deref())
        .is_some_and(|u| u.eq_ignore_ascii_case(account));
    let mut metadata = prior
        .as_ref()
        .and_then(|p| p.fjord.clone())
        .or_else(|| {
            original
                .as_ref()
                .filter(|_| migrating)
                .and_then(|h| h.fjord_account.clone())
        })
        .unwrap_or_else(|| FjordAccount {
            platform_url: platform.into(),
            ..Default::default()
        });
    if metadata.platform_url != platform {
        return Err(anyhow!("profile platform mismatch"));
    }
    metadata.auth_flow = Some("oidc".into());
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| anyhow!("could not stage profile session"))?;
    let profile = Profile {
        user: account.into(),
        generation: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        fjord: Some(metadata),
    };
    let _guard = super::fjord_scope::lock(platform, &profile)?;
    let retained = if tokens.refresh_token.is_none() {
        prior.as_ref().map(|p| load(platform, p)).transpose()?
    } else {
        None
    };
    let session = Session {
        bearer: tokens.access_token.clone(),
        refresh: tokens.refresh_token.clone().or_else(|| {
            retained
                .filter(|s| {
                    oidc::bearer_identity(&s.bearer) == oidc::bearer_identity(&tokens.access_token)
                })
                .and_then(|s| s.refresh)
        }),
    };
    write(platform, &profile, &session)?;
    super::profile_publish::edit(|latest| {
        if serde_json::to_value(latest.hosts.get(platform))? != serde_json::to_value(&original)?
            || serde_json::to_value(profiles::selection(latest, platform, Some(account))?)?
                != serde_json::to_value(&prior)?
        {
            return Err(anyhow!("profile destination changed; retry login"));
        }
        load(platform, &profile)?;
        if original.is_none() {
            latest.hosts.insert(
                platform.into(),
                Host {
                    auth_kind: super::hosts::AuthKind::FjordAccount,
                    git_protocol: protocol.into(),
                    fjord_account: profile.fjord.clone(),
                    ..Default::default()
                },
            );
        }
        let set = latest.accounts.entry(platform.into()).or_default();
        if set.default_account.is_none() && migrating {
            set.default_account = Some(account.into());
        }
        if let Some(old) = set.profiles.insert(account.into(), profile) {
            set.retired.push(old);
        }
        Ok(())
    })?;
    super::profile_retired::drain(platform)?;
    println!("Saved verified Fjord profile [redacted]; legacy credentials retained");
    Ok(())
}
pub fn scope(host: &str, config: &Host) -> Result<Option<Scope>> {
    Ok(config
        .selected_profile
        .as_ref()
        .filter(|p| p.fjord.is_some())
        .map(|p| Scope {
            host: host.into(),
            profile: p.clone(),
            destination: config.clone(),
        }))
}
