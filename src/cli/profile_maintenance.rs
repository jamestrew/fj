//! Token, session, refresh and logout maintenance for named account profiles.
use super::verify;
use crate::{
    auth,
    cli::auth::AuthSub,
    config::{
        fjord_profiles,
        hosts::Hosts,
        profile_session,
        profiles::{self},
    },
};
use anyhow::{anyhow, Result};
use std::io::{IsTerminal, Read};

fn stdin_value() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf.trim().to_string())
}
pub(super) async fn run(
    command: AuthSub,
    hosts: &Hosts,
    selected: Option<&str>,
    global: Option<&str>,
) -> Result<()> {
    let local = match &command {
        AuthSub::Logout(a) => a.host.clone(),
        AuthSub::Token(a) => a.host.clone(),
        AuthSub::Refresh(a) => a.host.clone(),
        AuthSub::Session(a) => a.host.clone(),
        _ => None,
    };
    let explicit = local.or_else(|| global.map(str::to_string));
    let explicit = explicit.as_deref();
    if matches!(command, AuthSub::Logout(_)) && explicit.is_none() && hosts.hosts.len() != 1 {
        return Err(anyhow!(
            "multiple hosts configured; pass `--host <hostname>`"
        ));
    }
    let host = hosts.resolve_host(explicit)?.to_string();
    let profile = profiles::selection(hosts, &host, selected)?
        .ok_or_else(|| anyhow!("profiled host: no default account profile; pass --account"))?;
    let mut config = hosts
        .hosts
        .get(&host)
        .cloned()
        .ok_or_else(|| anyhow!("host not configured"))?;
    config.selected_profile = Some(profile.clone());
    match command {
        AuthSub::Token(args) => {
            if std::io::stdout().is_terminal() && !args.force {
                return Err(anyhow!(
                    "refusing to print a plaintext token to a terminal; pipe to a file/command, or pass --force"
                ));
            }
            println!("{}", profiles::token(&host, &profile)?);
        }
        AuthSub::Session(args) if args.clear => {
            profile_session::clear(&host, &profile.user)?;
            println!("Cleared the web session for the account profile [redacted]");
        }
        AuthSub::Session(args) => {
            let cookie = match (args.cookie, args.with_cookie) {
                (Some(c), _) => c,
                (None, true) => stdin_value()?,
                (None, false) => auth::env_session_for_import(
                    &host,
                    profiles::generic_env(hosts, &host, explicit),
                )
                .ok_or_else(|| {
                    anyhow!("pass the session cookie via --cookie, --with-cookie, or FJ_SESSION")
                })?,
            };
            if cookie.is_empty() {
                return Err(anyhow!("empty cookie value; nothing stored"));
            }
            profile_session::import(&host, &profile.user, &cookie)?;
            if profile_session::cookie(&Hosts::load()?, &host, &profile.user)?.as_deref()
                != Some(cookie.as_str())
            {
                return Err(anyhow!("published web session is unreadable; import again"));
            }
            println!("Imported the web session for the account profile [redacted]");
        }
        AuthSub::Logout(_) => {
            profile_session::logout(&host, &profile)?;
            println!("Logged out the account profile [redacted]; legacy credentials stay retained and unused");
        }
        AuthSub::Refresh(args) if profile.fjord.is_some() => {
            if args.token.is_some() || args.with_token {
                return Err(anyhow!(
                    "a Fjord profile can't be refreshed with a pasted token"
                ));
            }
            let scope = fjord_profiles::scope(&host, &config)?
                .ok_or_else(|| anyhow!("requested Fjord profile is unavailable"))?;
            scope
                .refresh(&profiles::token(&host, &profile)?)
                .await?
                .ok_or_else(|| anyhow!("profile has no refresh token; log in again"))?;
            println!("Refreshed the Fjord profile [redacted]");
        }
        AuthSub::Refresh(args) => {
            let pasted = match (args.token, args.with_token) {
                (Some(t), _) => Some(t),
                (None, true) => Some(stdin_value()?),
                (None, false) => None,
            };
            let token = match &pasted {
                Some(t) => t.clone(),
                None => profiles::token(&host, &profile)?,
            };
            verify(&host, &config, &token, &profile.user).await?;
            let proxy = crate::client::proxy_credential_for_host(
                &host,
                &config,
                profiles::generic_env(hosts, &host, Some(&host)),
            )?;
            let probe = crate::cli::auth_probe::build_probe_client(&host, &config, &token, proxy)?;
            let server = crate::cli::auth_probe::probe_server(&probe).await;
            let mut updated = hosts.clone();
            crate::cli::auth_probe::record_server(updated.hosts.get_mut(&host).unwrap(), &server);
            updated.save()?;
            if pasted.is_some() {
                profiles::store(&host, &profile.user, &token, config)?;
            }
            println!("Verified the PAT profile [redacted]");
        }
        _ => unreachable!("only maintenance commands are routed here"),
    }
    Ok(())
}
