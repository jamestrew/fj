//! `fj auth login` — PAT and Fjord Account sign-in flows.

use anyhow::{anyhow, Context, Result};
use dialoguer::Password;

use crate::api;
use crate::auth as token_store;
use crate::cli::auth::LoginArgs;
use crate::cli::auth_probe::{build_probe_client, probe_version, warn_on_old_forgejo};
use crate::config::hosts::{Host, Hosts};
use crate::output;

pub(crate) async fn login(args: LoginArgs) -> Result<()> {
    if args.fjord {
        return login_fjord(args).await;
    }
    let hostname = match args.host.clone() {
        Some(h) => h,
        None => prompt_hostname()?,
    };
    let hostname = hostname.trim().to_string();
    if hostname.is_empty() {
        return Err(anyhow!("hostname is required"));
    }

    let token = read_token(&args)?;
    if token.trim().is_empty() {
        return Err(anyhow!("token is required"));
    }

    let git_protocol = args.git_protocol.clone();

    // Verify before persisting: never store a token we can't actually use.
    let provisional = Host {
        user: None,
        git_protocol: git_protocol.clone(),
        api_base_path: None,
        auth_kind: crate::config::hosts::AuthKind::Pat,
        fjord_account: None,
    };
    let probe_client = build_probe_client(&hostname, &provisional, &token)?;
    let me = api::user::current(&probe_client)
        .await
        .context("verifying token against /api/v1/user")?;

    // One-shot version probe. Surfaces obvious version mismatches before
    // they manifest as cryptic 404s on later commands. Failure here is
    // non-fatal: some Forgejo-compatible servers may not expose /version
    // identically.
    let version_string = probe_version(&probe_client).await.unwrap_or_default();
    warn_on_old_forgejo(&version_string);

    // Persist host + token.
    let mut hosts = Hosts::load()?;
    let host = Host {
        user: Some(me.login.clone()),
        git_protocol,
        api_base_path: None,
        auth_kind: crate::config::hosts::AuthKind::Pat,
        fjord_account: None,
    };
    hosts.upsert(&hostname, host);
    hosts.save()?;
    token_store::store_token(&hostname, &token)?;

    println!(
        "{} Logged in to {} as {}{}",
        output::bold("✓"),
        output::bold(&hostname),
        output::bold(&me.login),
        if version_string.is_empty() {
            String::new()
        } else {
            format!(" ({})", output::dim(&version_string))
        }
    );
    Ok(())
}

/// Fjord Account sign-in: email + password (+ TOTP if Paragon asks). The
/// resulting bearer is stored under the platform URL's keychain slot so it
/// doesn't collide with any PAT for the same DNS host (which can't happen
/// in practice, but the keychain key is namespaced anyway).
async fn login_fjord(args: LoginArgs) -> Result<()> {
    use crate::config::hosts::{AuthKind, FjordAccount};
    use crate::fjord::{
        normalize_platform_url, sign_in, SignInRequest, SignInResponse, DEFAULT_PLATFORM_URL,
    };

    let platform_url = match args.platform_url.as_deref() {
        Some(p) => normalize_platform_url(p)?,
        None => DEFAULT_PLATFORM_URL.to_string(),
    };

    let email = prompt_email()?;
    let password = Password::new()
        .with_prompt(format!("Password for {email}"))
        .interact()
        .context("reading password")?;

    // Up to one TOTP / recovery round-trip on top of the password call.
    // We re-submit password + code together; the server chains
    // /auth/2fa/totp/login internally so we don't need to track the
    // challenge token client-side.
    let first = sign_in(
        &platform_url,
        &SignInRequest {
            email: &email,
            password: &password,
            totp_code: None,
            recovery_code: None,
        },
    )
    .await?;
    let success = match first {
        SignInResponse::Success { .. } => first,
        SignInResponse::TotpRequired { message, .. } => {
            if let Some(m) = message.as_deref() {
                if !m.is_empty() {
                    eprintln!("{m}");
                }
            }
            let (totp, recovery) = match prompt_totp_or_recovery()? {
                TotpInput::Totp(c) => (Some(c), None),
                TotpInput::Recovery(c) => (None, Some(c)),
            };
            let second = sign_in(
                &platform_url,
                &SignInRequest {
                    email: &email,
                    password: &password,
                    totp_code: totp.as_deref(),
                    recovery_code: recovery.as_deref(),
                },
            )
            .await?;
            settle_outcome(second)?
        }
        other => settle_outcome(other)?,
    };

    let (access_token, user_email) = match success {
        SignInResponse::Success {
            access_token, user, ..
        } => (access_token, user.email.unwrap_or_else(|| email.clone())),
        _ => unreachable!("settle_outcome only returns Success or errors"),
    };

    // Persist: host entry keyed by platform URL, auth_kind=fjord_account,
    // bearer under the namespaced keychain slot.
    let mut hosts = Hosts::load()?;
    let host_entry = Host {
        user: Some(user_email.clone()),
        git_protocol: args.git_protocol.clone(),
        api_base_path: None,
        auth_kind: AuthKind::FjordAccount,
        fjord_account: Some(FjordAccount {
            platform_url: platform_url.clone(),
            default_instance_id: None,
            default_instance_display_name: None,
            default_instance_slug: None,
        }),
    };
    hosts.upsert(&platform_url, host_entry);
    hosts.save()?;
    token_store::store_fjord_bearer(&platform_url, &access_token)?;

    println!(
        "{} Signed in to {} as {}",
        output::bold("✓"),
        output::bold(&platform_url),
        output::bold(&user_email),
    );
    println!(
        "  {}",
        output::dim("Next: `fj instances list` to see your Forgejo instances, then `fj instances use <id>`.")
    );
    Ok(())
}

enum TotpInput {
    Totp(String),
    Recovery(String),
}

fn prompt_totp_or_recovery() -> Result<TotpInput> {
    use std::io::{stderr, stdin, Write};
    eprint!("TOTP code (or `r` to use a recovery code): ");
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading TOTP code")?;
    let trimmed = buf.trim();
    if trimmed.eq_ignore_ascii_case("r") {
        let r = Password::new()
            .with_prompt("Recovery code")
            .interact()
            .context("reading recovery code")?;
        Ok(TotpInput::Recovery(r))
    } else if trimmed.is_empty() {
        Err(anyhow!("TOTP code is required"))
    } else {
        Ok(TotpInput::Totp(trimmed.to_string()))
    }
}

fn prompt_email() -> Result<String> {
    use std::io::{stderr, stdin, Write};
    eprint!("Fjord Account email: ");
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading email")?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("email is required"));
    }
    Ok(trimmed.to_string())
}

/// Map a non-Success outcome to an `Err` with a user-readable explanation.
/// Success passes through verbatim so the caller can keep unwrapping fields.
fn settle_outcome(o: crate::fjord::SignInResponse) -> Result<crate::fjord::SignInResponse> {
    use crate::fjord::SignInResponse as R;
    match o {
        R::Success { .. } => Ok(o),
        R::TotpRequired { .. } => Err(anyhow!(
            "Fjord Account still needs a TOTP code; re-run `fj auth login --fjord`."
        )),
        R::WebauthnRequired { .. } => Err(anyhow!(
            "this account uses a security key; sign-in via key is not yet supported in fj. \
             Use TOTP or a recovery code."
        )),
        R::MfaSetupRequired { message, .. } => Err(anyhow!(
            "MFA enrollment is required before this account can sign in via fj. \
             Enroll a TOTP factor through the web UI first. ({})",
            message.unwrap_or_default()
        )),
        R::Locked {
            retry_after_seconds,
            message,
        } => Err(anyhow!(
            "Fjord Account is rate-limited (retry in {retry_after_seconds}s): {message}"
        )),
        R::InvalidCredentials { message } => Err(anyhow!("sign-in failed: {message}")),
        R::Misconfigured { message } => Err(anyhow!("fjord-platform is misconfigured: {message}")),
        R::UpstreamError { message, .. } => {
            Err(anyhow!("fjord-platform upstream error: {message}"))
        }
        R::StrategyMismatch { message } => Err(anyhow!(
            "this Fjord deployment signs in with single sign-on, which fj can't drive yet.\n\
             Sign in to your Forgejo directly with a personal access token instead:\n  \
             fj auth login --host <your-forgejo-host> --token <token>\n\
             Create the token in your Forgejo settings under Applications. ({message})"
        )),
    }
}

fn read_token(args: &LoginArgs) -> Result<String> {
    if let Some(t) = &args.token {
        // `--token` also binds FJ_TOKEN. If the value didn't come from the
        // environment it was passed on the command line, where it lands in
        // `ps` output and shell history. Nudge toward the safe inputs.
        let from_env = std::env::var("FJ_TOKEN").ok().as_deref() == Some(t.as_str());
        if !from_env {
            eprintln!(
                "warning: passing --token on the command line exposes it in shell history and `ps`; prefer `--with-token` (stdin) or the interactive prompt"
            );
        }
        return Ok(t.clone());
    }
    if args.with_token {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(buf.trim().to_string());
    }
    let t = Password::new()
        .with_prompt("Paste a Forgejo access token")
        .interact()
        .context("reading token")?;
    Ok(t)
}

fn prompt_hostname() -> Result<String> {
    eprint!("Hostname (e.g. rasterhub.com): ");
    use std::io::{stderr, stdin, Write};
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading hostname")?;
    Ok(buf)
}
