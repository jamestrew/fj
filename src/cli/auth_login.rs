//! `fj auth login` — PAT and Fjord Account sign-in flows.

use anyhow::{anyhow, Context, Result};
use dialoguer::{Password, Select};

use crate::api;
use crate::auth as token_store;
use crate::cli::auth::LoginArgs;
use crate::cli::auth_probe::{build_probe_client, probe_version, warn_on_old_forgejo};
use crate::config::hosts::{Host, Hosts};
use crate::output;

/// Which sign-in flow `fj auth login` runs. `Prompt` means "ask the user"
/// (the mobile first-run screen): we only reach it interactively when nothing
/// on the command line already disambiguates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginMode {
    Fjord,
    Pat,
    Prompt,
}

/// Decide the login flow from the flags alone, deferring to a menu only when
/// the invocation is ambiguous AND we have a TTY to drive it. Explicit intent
/// (`--fjord`, or any PAT input like `--host` / `--token` / `--with-token` /
/// `FJ_TOKEN`) skips the menu so scripts and the documented non-interactive
/// paths behave exactly as before. Kept pure so the branching is obvious.
fn resolve_login_mode(args: &LoginArgs, stdin_is_tty: bool) -> LoginMode {
    if args.fjord {
        return LoginMode::Fjord;
    }
    // `--token` also binds FJ_TOKEN, so an env-provided token lands here too.
    let pat_input_given = args.host.is_some() || args.token.is_some() || args.with_token;
    if pat_input_given {
        return LoginMode::Pat;
    }
    if stdin_is_tty {
        LoginMode::Prompt
    } else {
        // No flags and no TTY: keep the historical default rather than block
        // on a menu we can't render.
        LoginMode::Pat
    }
}

/// The mobile-style first screen: pick Fjord Account or your own Forgejo.
fn prompt_login_mode() -> Result<LoginMode> {
    let items = [
        "Fjord Account (email + password)",
        "Your own Forgejo server (access token)",
    ];
    let choice = Select::new()
        .with_prompt("How do you want to sign in?")
        .items(&items)
        .default(0)
        .interact()
        .context("reading sign-in method")?;
    Ok(if choice == 0 {
        LoginMode::Fjord
    } else {
        LoginMode::Pat
    })
}

pub(crate) async fn login(args: LoginArgs) -> Result<()> {
    use std::io::IsTerminal;
    let mode = match resolve_login_mode(&args, std::io::stdin().is_terminal()) {
        LoginMode::Prompt => {
            // The interactive first-run screen: greet with the brand mark
            // above the sign-in menu. Gated on an interactive stdout so a
            // redirected stream never collects the art.
            if std::io::stdout().is_terminal() {
                print!("{}", output::logo::banner());
            }
            prompt_login_mode()?
        }
        concrete => concrete,
    };
    match mode {
        LoginMode::Fjord => login_fjord(args).await,
        LoginMode::Pat => login_pat(args).await,
        LoginMode::Prompt => unreachable!("Prompt is resolved to a concrete mode above"),
    }
}

async fn login_pat(args: LoginArgs) -> Result<()> {
    let host_input = match args.host.clone() {
        Some(h) => h,
        None => prompt_hostname()?,
    };
    let (hostname, web_url) = normalize_pat_host(&host_input)?;
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
        web_url: web_url.clone(),
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
        web_url,
        api_base_path: None,
        auth_kind: crate::config::hosts::AuthKind::Pat,
        fjord_account: None,
    };
    hosts.upsert(&hostname, host);
    hosts.save()?;
    let storage = token_store::store_token(&hostname, &token)?;
    print_file_store_notice(&storage);

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
        web_url: None,
        api_base_path: None,
        auth_kind: AuthKind::FjordAccount,
        fjord_account: Some(FjordAccount {
            platform_url: platform_url.clone(),
            default_instance_id: None,
            default_instance_display_name: None,
            default_instance_slug: None,
            default_instance_public_url: None,
        }),
    };
    hosts.upsert(&platform_url, host_entry);
    hosts.save()?;
    let storage = token_store::store_fjord_bearer(&platform_url, &access_token)?;
    print_file_store_notice(&storage);

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

fn print_file_store_notice(storage: &token_store::TokenStoreLocation) {
    if let token_store::TokenStoreLocation::File(path) = storage {
        println!(
            "{} Keychain unavailable; stored token in {}",
            output::bold("!"),
            path.display()
        );
    }
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
    eprint!("Hostname or URL (e.g. rasterhub.com or http://localhost:3000): ");
    use std::io::{stderr, stdin, Write};
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading hostname")?;
    Ok(buf)
}

fn normalize_pat_host(raw: &str) -> Result<(String, Option<String>)> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(anyhow!("hostname is required"));
    }

    let parse_candidate = if trimmed.contains("://") {
        Some(trimmed.to_string())
    } else if trimmed.contains('/') {
        Some(format!("https://{trimmed}"))
    } else {
        None
    };

    let Some(candidate) = parse_candidate else {
        // Bare host (no scheme, no path), e.g. `rasterhub.com` or
        // `localhost:3000`. Validate it through the same keystone check
        // `setup-git` uses, so a malformed or '-'-leading host can never be
        // stored as a host key and later interpolated into the credential
        // helper. (The URL branches below already go through `url::Url`.)
        super::auth_setup_git::validate_hostname(trimmed)?;
        return Ok((trimmed.to_string(), None));
    };

    let parsed =
        url::Url::parse(&candidate).with_context(|| format!("invalid Forgejo host URL '{raw}'"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(anyhow!(
                "Forgejo host URL must be http or https, got '{other}'"
            ))
        }
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("Forgejo host URL '{raw}' has no host"))?;
    let mut key = host.to_string();
    if let Some(port) = parsed.port() {
        key.push(':');
        key.push_str(&port.to_string());
    }
    let path = parsed.path().trim_end_matches('/');
    if !path.is_empty() && path != "/" {
        key.push_str(path);
    }
    let mut web = parsed;
    web.set_query(None);
    web.set_fragment(None);
    let normalized = web.as_str().trim_end_matches('/').to_string();
    Ok((key, Some(normalized)))
}
