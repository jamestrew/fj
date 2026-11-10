//! Invocation-only PAT selection. Stored host and credential formats are untouched.
use super::*;
use crate::{
    auth,
    config::hosts::{AuthKind, Host},
};
use anyhow::{anyhow, Result};
use std::{fmt, str::FromStr, sync::OnceLock};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub struct Account(String);
impl Account {
    pub(crate) fn as_str(&self) -> &str {
        self.0.trim()
    }
}
impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted account]")
    }
}
impl FromStr for Account {
    type Err = std::convert::Infallible;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(value.into()))
    }
}
static SELECTED: OnceLock<Option<Account>> = OnceLock::new();
pub fn initialize(account: Option<Account>) -> Result<()> {
    if account
        .as_ref()
        .is_some_and(|a| a.0.trim().is_empty() || a.0.chars().any(char::is_control))
    {
        return Err(anyhow!(
            "account override must be nonempty and contain no control characters"
        ));
    }
    SELECTED
        .set(account)
        .map_err(|_| anyhow!("account override already initialized"))
}
pub(super) fn selected() -> Option<Account> {
    SELECTED.get().cloned().flatten()
}

pub(super) fn resolve_token(
    hosts: &crate::config::hosts::Hosts,
    host: &str,
    config: &mut Host,
    generic: bool,
) -> Result<Option<String>> {
    let account = selected();
    let profile =
        crate::config::profiles::selection(hosts, host, account.as_ref().map(Account::as_str))?;
    config.selected_account = account
        .clone()
        .or_else(|| profile.as_ref().map(|p| p.user.parse().unwrap()));
    if config.selected_account.is_some() && config.auth_kind != AuthKind::Pat {
        return Err(anyhow!(
            "account override requires a direct Forgejo PAT host, not a Fjord Account gateway"
        ));
    }
    if config.auth_kind == AuthKind::Pat {
        return select_pat(
            account.is_some() && profile.is_none(),
            auth::env_token_for_host(host),
            generic.then(auth::env_token).flatten(),
            || match profile {
                Some(profile) => crate::config::profiles::token(host, &profile).map(Some),
                None => auth::load_token(host),
            },
        );
    }
    let fa = config.fjord_account.as_ref().ok_or_else(|| anyhow!(
        "host '{host}' is Fjord-Account but has no fjord_account config; remove and re-run `fj auth login --fjord`"
    ))?;
    Ok(Some(
        auth::load_fjord_bearer(&fa.platform_url)?.ok_or_else(|| {
            anyhow!(
                "no Fjord Account session stored for '{}'. Run `fj auth login --fjord` again.",
                fa.platform_url
            )
        })?,
    ))
}

fn select_pat(
    explicit: bool,
    scoped: Option<String>,
    generic: Option<String>,
    stored: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    if let Some(token) = scoped.or(generic) {
        return Ok(Some(token));
    }
    if explicit {
        return Err(anyhow!("account override needs an eligible environment PAT or matching profile; no fallback credentials are used"));
    }
    stored()
}

pub(super) struct Guard {
    account: Account,
    verified: OnceCell<()>,
}
impl Guard {
    pub(super) fn new(account: Account) -> Self {
        Self {
            account,
            verified: OnceCell::new(),
        }
    }
    pub(super) fn verified(&self) -> bool {
        self.verified.get().is_some()
    }
    async fn verify(&self, client: &Client) -> Result<()> {
        self.verified
            .get_or_try_init(|| async {
                // Remove only the guard on this clone to avoid recursive verification.
                let mut probe = client.clone();
                probe.account = None;
                let response = probe
                    .request_unchecked(Method::GET, "user", &[], None, &HeaderMap::new())
                    .await
                    .map_err(|_| anyhow!("could not verify account override credential"))?;
                if !response.status().is_success() {
                    return Err(anyhow!("account override credential verification failed"));
                }
                #[derive(serde::Deserialize)]
                struct Identity {
                    login: String,
                }
                let identity: Identity = response
                    .json()
                    .await
                    .map_err(|_| anyhow!("could not decode account override identity"))?;
                if !identity.login.eq_ignore_ascii_case(self.account.0.trim()) {
                    return Err(anyhow!(
                        "override credential does not belong to the requested account"
                    ));
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }
}
impl Client {
    pub async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
        extra: &HeaderMap,
    ) -> Result<Response> {
        if self.account.is_some() {
            query::check(self, path, query, extra)?;
        }
        self.verify_account().await?;
        self.request_unchecked(method, path, query, body, extra)
            .await
    }
    pub async fn verify_account(&self) -> Result<()> {
        if let Some(guard) = &self.account {
            if self.current_token().is_none() {
                return Err(anyhow!(
                    "account override cannot use anonymous or stored web-session fallbacks"
                ));
            }
            guard.verify(self).await?;
        }
        Ok(())
    }

    /// Verify invocation identity before preparing a multipart upload.
    pub async fn upload_headers(&self, context: &str) -> Result<HeaderMap> {
        self.verify_account().await?;
        self.upload_auth_headers(context)
    }
    #[cfg(test)]
    fn with_account(mut self, name: &str) -> Self {
        self.account = Some(Arc::new(Guard::new(Account(name.into()))));
        self
    }
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "account_fix1_tests.rs"]
mod fix1_tests;
#[path = "account_query.rs"]
mod query;
