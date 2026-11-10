use crate::{
    api,
    cli::auth_probe,
    config::{
        hosts::{AuthKind, Host, Hosts},
        profiles,
    },
};
use anyhow::{anyhow, Result};
pub(super) async fn verify(
    host: &str,
    config: &Host,
    token: &str,
    account: &str,
) -> Result<String> {
    if config.auth_kind != AuthKind::Pat {
        let scope = crate::config::fjord_profiles::scope(host, config)?
            .ok_or_else(|| anyhow!("requested Fjord profile is unavailable"))?;
        let checked = crate::config::fjord_profiles::verify(scope.platform(), token, account).await;
        if checked
            .as_ref()
            .err()
            .is_some_and(|e| e.downcast_ref::<crate::fjord::SessionExpired>().is_some())
        {
            scope
                .refresh(token)
                .await?
                .ok_or_else(|| anyhow!("profile cannot be refreshed; log in again"))?;
        } else {
            checked?;
        }
        return Ok(account.to_string());
    }
    let hosts = Hosts::load()?;
    let proxy = crate::client::proxy_credential_for_host(
        host,
        config,
        profiles::generic_env(&hosts, host, Some(host)),
    )?;
    let client = auth_probe::build_probe_client(host, config, token, proxy)?;
    let user = api::user::current(&client)
        .await
        .map_err(|_| anyhow!("profile identity verification failed"))?;
    if !user.login.eq_ignore_ascii_case(account) {
        return Err(anyhow!("credential does not match requested account"));
    }
    Ok(user.login.to_ascii_lowercase())
}
