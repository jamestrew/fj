//! Auth commands for PAT profiles; other auth flows retain the legacy path.
use crate::cli::{
    auth::{AuthCmd, AuthSub, LoginArgs},
    auth_login, auth_probe,
};
use crate::{
    api, auth,
    client::account::Account,
    config::{
        hosts::{AuthKind, Host, Hosts},
        profiles,
    },
};
use anyhow::{anyhow, Result};

pub(super) async fn run(
    cmd: AuthCmd,
    account: Option<Account>,
    global: Option<&str>,
) -> Result<()> {
    let hosts = Hosts::load()?;
    let canonical = account
        .as_ref()
        .map(|a| profiles::canonical(a.as_str()))
        .transpose()?;
    let selected = canonical.as_deref();
    match cmd.command {
        AuthSub::Login(mut args) if selected.is_some() => {
            args.host = args
                .host
                .or_else(|| global.map(str::to_string))
                .or_else(|| hosts.current.clone());
            login(args, selected.unwrap()).await
        }
        AuthSub::Switch(args) if selected.is_some() => {
            let profile = profiles::selection(&hosts, &args.host, selected)?
                .ok_or_else(|| anyhow!("requested account profile is unavailable"))?;
            let config = hosts
                .hosts
                .get(&args.host)
                .ok_or_else(|| anyhow!("host not configured"))?;
            verify(
                &args.host,
                config,
                &profiles::token(&args.host, &profile)?,
                &profile.user,
            )
            .await?;
            profiles::switch(&args.host, &profile)?;
            println!("Default account profile selected [redacted]");
            Ok(())
        }
        AuthSub::Status(args)
            if selected.is_some()
                || hosts
                    .hosts
                    .keys()
                    .any(|h| profiles::has_profiles(&hosts, h)) =>
        {
            if args.show_token {
                return Err(anyhow!(
                    "profile status is redacted; token export is unavailable"
                ));
            }
            if let Some(host) = global {
                println!(
                    "{}",
                    profiles::status(&hosts, hosts.resolve_host(Some(host))?)
                );
            } else {
                for host in hosts.hosts.keys() {
                    println!("{}", profiles::status(&hosts, host));
                }
            }
            Ok(())
        }
        AuthSub::List
            if selected.is_some()
                || hosts
                    .hosts
                    .keys()
                    .any(|h| profiles::has_profiles(&hosts, h)) =>
        {
            for host in hosts.hosts.keys() {
                println!("{}", profiles::status(&hosts, host));
            }
            Ok(())
        }
        command => {
            let local = match &command {
                AuthSub::Logout(a) => a.host.as_deref(),
                AuthSub::Token(a) => a.host.as_deref(),
                AuthSub::Refresh(a) => a.host.as_deref(),
                AuthSub::Session(a) => a.host.as_deref(),
                AuthSub::Login(a) => a.host.as_deref(),
                _ => None,
            };
            let maintenance = matches!(
                command,
                AuthSub::Logout(_)
                    | AuthSub::Token(_)
                    | AuthSub::Refresh(_)
                    | AuthSub::Session(_)
                    | AuthSub::Login(_)
            );
            let target = local.or(global).or(hosts.current.as_deref());
            let normalized = if matches!(command, AuthSub::Login(_)) {
                target
                    .map(auth_login::normalize_pat_host)
                    .transpose()?
                    .map(|v| v.0)
            } else {
                target.map(str::to_string)
            };
            let target = normalized.as_deref();
            if selected.is_some()
                || maintenance && target.is_some_and(|h| profiles::has_profiles(&hosts, h))
            {
                return Err(anyhow!("use auth login --account to migrate or update a PAT profile; this legacy maintenance operation is unavailable for profiled credentials"));
            }
            crate::cli::auth::run(AuthCmd { command }, global).await
        }
    }
}
async fn verify(host: &str, config: &Host, token: &str, account: &str) -> Result<String> {
    if config.auth_kind != AuthKind::Pat {
        return Err(anyhow!(
            "Fjord gateway profiles are deferred to the next step"
        ));
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
async fn login(mut args: LoginArgs, account: &str) -> Result<()> {
    if args.fjord {
        return Err(anyhow!(
            "Fjord gateway profiles are deferred to the next step"
        ));
    }
    let input = args
        .host
        .as_deref()
        .ok_or_else(|| anyhow!("profile login requires --host"))?;
    let (name, url) = auth_login::normalize_pat_host(input)?;
    let hosts = Hosts::load()?;
    let config = hosts.hosts.get(&name).cloned().unwrap_or_else(|| Host {
        web_url: url,
        git_protocol: args.git_protocol.clone(),
        ..Default::default()
    });
    if args.token.is_none() && !args.with_token {
        args.token = auth::env_token_for_host(&name).or_else(|| {
            profiles::generic_env(&hosts, &name, Some(&name))
                .then(auth::env_token)
                .flatten()
        });
        if args.token.is_none() {
            args.token = match profiles::selection(&hosts, &name, Some(account))? {
                Some(p) => Some(profiles::token(&name, &p)?),
                None if config
                    .user
                    .as_deref()
                    .is_some_and(|u| u.eq_ignore_ascii_case(account)) =>
                {
                    auth::load_token(&name)?
                }
                _ => None,
            };
        }
    }
    if args.token.is_none() && !args.with_token {
        return Err(anyhow!(
            "provide a host-scoped environment PAT or --with-token"
        ));
    }
    let token = auth_login::read_token(&args)?;
    let user = verify(&name, &config, &token, account).await?;
    profiles::store(&name, &user, &token, config)?;
    println!("Saved verified PAT profile [redacted]; legacy credentials retained");
    Ok(())
}
