//! Auth commands for PAT profiles; other auth flows retain the legacy path.
use crate::cli::{
    auth::{AuthCmd, AuthSub, LoginArgs},
    auth_login,
};
use crate::{
    auth,
    client::account::Account,
    config::{
        hosts::{Host, Hosts},
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
            let mut config = hosts
                .hosts
                .get(&args.host)
                .cloned()
                .ok_or_else(|| anyhow!("host not configured"))?;
            config.selected_profile = Some(profile.clone());
            verify(
                &args.host,
                &config,
                &profiles::token(&args.host, &profile)?,
                &profile.user,
            )
            .await?;
            profiles::switch(&args.host, &profile, &config)?;
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
                AuthSub::SetupGit(a) => a.host.as_deref(),
                _ => None,
            };
            let maintenance = matches!(
                command,
                AuthSub::Logout(_) | AuthSub::Token(_) | AuthSub::Refresh(_) | AuthSub::Session(_)
            );
            let guarded = maintenance || matches!(command, AuthSub::SetupGit(_));
            let explicit = local.or(global);
            let target = if !guarded
                || matches!(command, AuthSub::Logout(_))
                    && explicit.is_none()
                    && hosts.hosts.len() != 1
            {
                None
            } else {
                Some(hosts.resolve_host(explicit)?)
            };
            if matches!(command, AuthSub::SetupGit(_))
                && target.is_some_and(|h| profiles::has_profiles(&hosts, h))
            {
                return Err(anyhow!("profiled hosts cannot use the fj git helper; remove it with git config --global --unset-all credential.https://HOST.helper and configure a standard Git credential helper"));
            }
            if selected.is_some()
                || maintenance && target.is_some_and(|h| profiles::has_profiles(&hosts, h))
            {
                return Err(anyhow!("profiled credentials require named login; existing fj git helpers must be removed with git config --global --unset-all credential.https://HOST.helper and replaced with a standard Git credential helper"));
            }
            crate::cli::auth::run(AuthCmd { command }, global).await
        }
    }
}
async fn login(mut args: LoginArgs, account: &str) -> Result<()> {
    if args.fjord {
        let platform = crate::fjord::normalize_platform_url(
            args.platform_url
                .as_deref()
                .unwrap_or(crate::fjord::DEFAULT_PLATFORM_URL),
        )?;
        if !matches!(
            crate::fjord::oidc::probe_strategy(&platform)
                .await
                .map_err(|_| anyhow!("could not probe profile sign-in strategy"))?,
            crate::fjord::oidc::AuthStrategy::Oidc
        ) {
            return Err(anyhow!("named Fjord login requires an OIDC deployment"));
        }
        return auth_login::login_fjord_oidc(args, platform, Some(account))
            .await
            .map_err(|_| anyhow!("named Fjord sign-in failed; retry login"));
    }
    let input = args
        .host
        .as_deref()
        .ok_or_else(|| anyhow!("profile login requires --host"))?;
    let (name, url) = auth_login::normalize_pat_host(input)?;
    let hosts = Hosts::load()?;
    let name = crate::config::metadata::existing_key(&hosts, &name)?.unwrap_or(name);
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

#[path = "profile_verify.rs"]
mod verification;
use verification::verify;
