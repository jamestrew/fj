//! `fj auth login` — PAT and Fjord Account sign-in flows.

use anyhow::{anyhow, Context, Result};
use dialoguer::{Password, Select};

use crate::api;
use crate::auth as token_store;
use crate::cli::auth::LoginArgs;
use crate::cli::auth_probe::{build_probe_client, probe_version, warn_on_old_forgejo};
use crate::config::hosts::{AuthKind, FjordAccount, Host, Hosts};
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
#[cfg_attr(coverage_nightly, coverage(off))]
fn resolve_login_mode(args: &LoginArgs, stdin_is_tty: bool) -> LoginMode {
    if args.fjord {
        return LoginMode::Fjord;
    }
    let pat_input_given = args.host.is_some()
        || args.token.is_some()
        || args.with_token
        || token_store::env_token().is_some();
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
#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
        auth_kind: AuthKind::Pat,
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
        auth_kind: AuthKind::Pat,
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

/// Fjord Account sign-in. The deployment's `GET /api/auth/status` decides the
/// shape: an `oidc` deployment (production) runs the browser-based
/// Authorization Code + PKCE loopback flow; a `credentials` deployment runs the
/// email + password flow. Either way the resulting bearer is stored under the
/// platform URL's keychain slot.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn login_fjord(args: LoginArgs) -> Result<()> {
    use crate::fjord::oidc::{self, AuthStrategy};
    use crate::fjord::{normalize_platform_url, DEFAULT_PLATFORM_URL};

    let platform_url = match args.platform_url.as_deref() {
        Some(p) => normalize_platform_url(p)?,
        None => DEFAULT_PLATFORM_URL.to_string(),
    };

    // Pick the sign-in shape from the deployment. A probe failure is
    // non-fatal: fall back to the credentials flow, which still surfaces a
    // clear `StrategyMismatch` error if the deployment is actually OIDC.
    match oidc::probe_strategy(&platform_url).await {
        Ok(AuthStrategy::Oidc) => return login_fjord_oidc(args, platform_url).await,
        Ok(AuthStrategy::Mock) => {
            eprintln!(
                "note: this deployment reports no live Fjord Account sign-in (dev/pending mode); \
                 attempting credentials sign-in."
            );
        }
        Ok(AuthStrategy::Unknown(_)) => {
            eprintln!(
                "note: this deployment reports an unrecognized sign-in strategy; \
                 attempting credentials sign-in."
            );
        }
        Ok(AuthStrategy::Credentials) => {}
        Err(e) => {
            eprintln!("note: could not probe {platform_url} sign-in strategy ({e}); attempting credentials sign-in.");
        }
    }
    login_fjord_credentials(args, platform_url).await
}

/// Fjord Account sign-in via SSO/OIDC. Three ways to obtain the session, all
/// sharing the token persistence in [`persist_oidc_session`] so the result is
/// identical whichever ran:
/// - default (same-device): bind a loopback listener, open the browser, and
///   capture the Authorization Code + PKCE redirect on `127.0.0.1` (zero
///   copy-paste);
/// - `--device`: the OAuth 2.0 Device Authorization Grant (RFC 8628); show a
///   short code + link + QR, poll for approval from any device. The durable
///   cross-device path; auto-selected on a headless / SSH session (see
///   [`choose_oidc_capture`]);
/// - `--manual`: the interim paste path (superseded by `--device` for the
///   cross-device case); print the authorize URL and read back the redirected
///   URL (or bare code) the user pastes.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn login_fjord_oidc(args: LoginArgs, platform_url: String) -> Result<()> {
    use crate::fjord::oidc;

    match choose_oidc_capture(args.device, args.manual, env_prefers_device_flow()) {
        OidcCapture::Device { auto } => return login_device_flow(&args, platform_url, auto).await,
        OidcCapture::Manual => {
            let client = oidc::client_id();
            let pkce = oidc::Pkce::generate()?;
            let (code, redirect_uri) = acquire_code_manual(&platform_url, &client, &pkce)?;
            let tokens =
                oidc::exchange_code(&platform_url, &client, &code, &redirect_uri, &pkce.verifier)
                    .await?;
            persist_oidc_session(&args, platform_url, tokens)
        }
        OidcCapture::Loopback => {
            let client = oidc::client_id();
            let pkce = oidc::Pkce::generate()?;
            let (code, redirect_uri) = acquire_code_loopback(&platform_url, &client, &pkce).await?;
            let tokens =
                oidc::exchange_code(&platform_url, &client, &code, &redirect_uri, &pkce.verifier)
                    .await?;
            persist_oidc_session(&args, platform_url, tokens)
        }
    }
}

/// Which OIDC code-capture strategy `fj auth login --fjord` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OidcCapture {
    /// Device Authorization Grant. `auto` is true when it was chosen for the
    /// user (headless / SSH) rather than requested with `--device`, so the CLI
    /// can explain why and how to override.
    Device { auto: bool },
    /// The `--manual` paste path.
    Manual,
    /// The default same-device loopback capture.
    Loopback,
}

/// Pick the OIDC capture strategy from the flags and the environment. Explicit
/// flags always win: `--device` and `--manual` conflict at the CLI layer, so at
/// most one is set. With neither, a headless / SSH session (no local browser to
/// open, no loopback the user can reach) auto-selects device flow; a normal
/// desktop keeps loopback. Pure so the precedence is unit-tested.
fn choose_oidc_capture(device_flag: bool, manual_flag: bool, headless: bool) -> OidcCapture {
    if device_flag {
        OidcCapture::Device { auto: false }
    } else if manual_flag {
        OidcCapture::Manual
    } else if headless {
        OidcCapture::Device { auto: true }
    } else {
        OidcCapture::Loopback
    }
}

/// True when the environment can't drive the same-device loopback flow: a
/// remote shell (`$SSH_CONNECTION` / `$SSH_TTY`), or a Unix session with no
/// display server. macOS and Windows always have a windowing system, so they
/// never auto-fall-back. `FJ_FORCE_LOOPBACK` opts out (for tests / odd setups
/// where a browser really is reachable).
#[cfg_attr(coverage_nightly, coverage(off))]
fn env_prefers_device_flow() -> bool {
    if std::env::var_os("FJ_FORCE_LOOPBACK").is_some() {
        return false;
    }
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return true;
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if !has_display {
            return true;
        }
    }
    false
}

/// Device Authorization Grant sign-in (RFC 8628): request a device+user code,
/// show the user where to approve it (code + link + QR), and poll until the
/// session lands. `auto` distinguishes an auto-selected run (headless / SSH)
/// from an explicit `--device` so the intro line can explain the choice.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn login_device_flow(args: &LoginArgs, platform_url: String, auto: bool) -> Result<()> {
    use crate::fjord::oidc;
    use std::time::{Duration, Instant};

    let client = oidc::client_id();
    let auth = oidc::request_device_authorization(&platform_url, &client, None)
        .await
        .context("starting device-flow sign-in")?;

    render_device_prompt(&platform_url, &auth, auto);

    let deadline = Instant::now() + Duration::from_secs(auth.expires_in);
    let interval = Duration::from_secs(auth.interval.max(1));
    let tokens = oidc::poll_for_device_token(
        &platform_url,
        &client,
        &auth.device_code,
        interval,
        deadline,
    )
    .await?;
    persist_oidc_session(args, platform_url, tokens)
}

/// Print the branded device-flow instructions: the verification URL, the code
/// to enter, and a scannable QR of the complete link. Mirrors the `--manual`
/// copy style (numbered, dimmed guidance, brand arrow).
#[cfg_attr(coverage_nightly, coverage(off))]
fn render_device_prompt(
    platform_url: &str,
    auth: &crate::fjord::oidc::DeviceAuthorization,
    auto: bool,
) {
    use crate::fjord::oidc;

    println!(
        "{} Sign in to {} from any device {}",
        output::bold("→"),
        output::bold(platform_url),
        output::dim("(device flow)")
    );
    if auto {
        println!(
            "  {}",
            output::dim(
                "(no local browser detected; using the cross-device flow. Pass --device to force it, or set FJ_FORCE_LOOPBACK=1 for the loopback flow.)"
            )
        );
    }
    println!();
    println!(
        "  {} {}",
        output::dim("To sign in, open"),
        output::bold(&auth.verification_uri)
    );
    println!(
        "  {} {}",
        output::dim("on any device (phone, iPad, laptop) and enter code:"),
        output::bold(&auth.user_code)
    );
    println!();

    let link = auth.best_verification_link();
    if auth.verification_uri_complete.is_some() {
        println!(
            "  {}",
            output::dim("Or open this link directly (it fills in the code for you):")
        );
        println!("  {link}");
        println!();
    }

    match oidc::render_qr(link) {
        Some(qr) => {
            println!("{}", output::dim("  Scan to open on a phone or iPad:"));
            // The QR block is already spaced; indent each line to match the copy.
            for line in qr.lines() {
                println!("  {line}");
            }
        }
        None => {
            // Rendering failed (data too large): the URL above is the fallback.
        }
    }

    let minutes = auth.expires_in / 60;
    println!(
        "  {}",
        output::dim(&format!(
            "Waiting for you to approve the sign-in… (the code expires in about {minutes} min)"
        ))
    );
}
/// Build the `fjord_account` block a sign-in persists.
///
/// `hosts.upsert` replaces the whole host entry, so anything not carried over
/// here is lost. The instance selection is the one thing a sign-in has no
/// business resetting: re-login is the documented recovery from an expired
/// session, and dropping it means the very next command fails with "no default
/// Fjord instance selected". Kept pure so both sign-in flows share one answer.
fn fjord_account_for(platform_url: &str, auth_flow: &str, previous: Option<&Host>) -> FjordAccount {
    // An instance id is only meaningful to the platform that issued it, so a
    // previous entry under a different platform URL carries nothing forward.
    let kept = previous
        .filter(|host| matches!(host.auth_kind, AuthKind::FjordAccount))
        .and_then(|host| host.fjord_account.as_ref())
        .filter(|account| account.platform_url == platform_url);

    FjordAccount {
        platform_url: platform_url.to_string(),
        default_instance_id: kept.and_then(|a| a.default_instance_id.clone()),
        default_instance_display_name: kept.and_then(|a| a.default_instance_display_name.clone()),
        default_instance_slug: kept.and_then(|a| a.default_instance_slug.clone()),
        default_instance_public_url: kept.and_then(|a| a.default_instance_public_url.clone()),
        auth_flow: Some(auth_flow.to_string()),
    }
}

/// Default (same-device) code capture: bind the loopback listener, open the
/// browser to the authorize URL, and wait for the redirect. Returns the
/// captured `code` and the exact `redirect_uri` the listener bound (needed
/// verbatim at token exchange).
#[cfg_attr(coverage_nightly, coverage(off))]
async fn acquire_code_loopback(
    platform_url: &str,
    client: &str,
    pkce: &crate::fjord::oidc::Pkce,
) -> Result<(String, String)> {
    use crate::cli::web;
    use crate::fjord::oidc;

    let server = oidc::LoopbackServer::bind()?;
    let redirect_uri = server.redirect_uri.clone();
    let authorize_url = oidc::build_authorize_url(platform_url, client, &redirect_uri, pkce);

    println!(
        "{} Opening your browser to sign in to {}",
        output::bold("→"),
        output::bold(platform_url)
    );
    println!(
        "  {}",
        output::dim("If it doesn't open, copy this URL into your browser:")
    );
    println!("  {authorize_url}");
    // A failed opener (headless box, no `open`/`xdg-open`) isn't fatal: the URL
    // is printed above, so the user can paste it and the loopback still catches
    // the redirect. (If the browser is on another device entirely, use
    // `--manual`.)
    if let Err(e) = web::open(&authorize_url) {
        eprintln!(
            "  {}",
            output::dim(&format!("(couldn't launch a browser automatically: {e})"))
        );
    }
    println!("  {}", output::dim("Waiting for the sign-in to complete…"));

    let code = server.wait_for_code(pkce.state.clone()).await?;
    Ok((code, redirect_uri))
}

/// Manual (`--manual`) code capture for a browser on a different device. Binds
/// no listener: prints the authorize URL, then reads the redirected URL (or
/// bare code) the user pastes back and validates the returned `state` (when the
/// paste carried one) against the value we generated. Returns the `code` and
/// the fixed registered `redirect_uri` — which must match what the token
/// exchange sends.
#[cfg_attr(coverage_nightly, coverage(off))]
fn acquire_code_manual(
    platform_url: &str,
    client: &str,
    pkce: &crate::fjord::oidc::Pkce,
) -> Result<(String, String)> {
    use crate::fjord::oidc;

    let redirect_uri = oidc::default_redirect_uri();
    let authorize_url = oidc::build_authorize_url(platform_url, client, &redirect_uri, pkce);

    println!(
        "{} Sign in to {} from any device {}",
        output::bold("→"),
        output::bold(platform_url),
        output::dim("(manual mode)")
    );
    println!();
    println!(
        "  {} {}",
        output::bold("1."),
        output::dim("Open this URL in a browser on any device (phone, iPad, laptop):")
    );
    println!("     {authorize_url}");
    println!();
    println!(
        "  {} {}",
        output::bold("2."),
        output::dim("Sign in. Your browser will try to reach a 127.0.0.1 address and show")
    );
    println!(
        "     {}",
        output::dim("a \"can't connect\" / \"page not found\" error — that is expected.")
    );
    println!();
    println!(
        "  {} {}",
        output::bold("3."),
        output::dim("Copy the full address from the browser's address bar — it looks like")
    );
    println!(
        "     {}",
        output::dim("http://127.0.0.1:47017/callback?code=…&state=… — and paste it below.")
    );
    println!();

    let pasted = prompt_line("Paste the redirected URL (or just the code): ")?;
    let parsed = oidc::parse_pasted_callback(&pasted)?;
    parsed.check_state(&pkce.state)?;

    Ok((parsed.code, redirect_uri))
}

/// Persist a completed OIDC sign-in: store the access + refresh tokens, upsert
/// the Fjord-Account host entry, and print the success line. Shared by the
/// loopback and manual paths.
#[cfg_attr(coverage_nightly, coverage(off))]
fn persist_oidc_session(
    args: &LoginArgs,
    platform_url: String,
    tokens: crate::fjord::oidc::TokenResponse,
) -> Result<()> {
    use crate::fjord::oidc;

    if tokens.refresh_token.is_none() {
        eprintln!(
            "{}",
            output::dim(
                "note: the provider returned no refresh token; you'll re-run `fj auth login --fjord` when the session expires."
            )
        );
    }
    let user_email = oidc::bearer_email(&tokens.access_token);

    let mut hosts = Hosts::load()?;
    let host_entry = Host {
        user: user_email.clone(),
        git_protocol: args.git_protocol.clone(),
        web_url: None,
        api_base_path: None,
        auth_kind: AuthKind::FjordAccount,
        fjord_account: Some(fjord_account_for(
            &platform_url,
            "oidc",
            hosts.hosts.get(&platform_url),
        )),
    };
    hosts.upsert(&platform_url, host_entry);
    hosts.save()?;
    let storage = oidc::store_tokens(&platform_url, &tokens)?;
    print_file_store_notice(&storage);

    println!(
        "{} Signed in to {}{}",
        output::bold("✓"),
        output::bold(&platform_url),
        match user_email.as_deref() {
            Some(e) => format!(" as {}", output::bold(e)),
            None => String::new(),
        }
    );
    println!(
        "  {}",
        output::dim("Next: `fj instances list` to see your Forgejo instances, then `fj instances use <id>`.")
    );
    Ok(())
}

/// Fjord Account sign-in: email + password (+ TOTP if Paragon asks). The
/// resulting bearer is stored under the platform URL's keychain slot so it
/// doesn't collide with any PAT for the same DNS host (which can't happen
/// in practice, but the keychain key is namespaced anyway).
#[cfg_attr(coverage_nightly, coverage(off))]
async fn login_fjord_credentials(args: LoginArgs, platform_url: String) -> Result<()> {
    use crate::fjord::{sign_in, SignInRequest, SignInResponse};

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
        fjord_account: Some(fjord_account_for(
            &platform_url,
            "credentials",
            hosts.hosts.get(&platform_url),
        )),
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

/// Read one line from stdin after printing `prompt` to stderr. Used by
/// `--manual` sign-in to read the pasted callback URL / code. Not masked: a
/// single-use authorization code isn't a long-lived secret, and the user is
/// pasting a URL they need to see.
#[cfg_attr(coverage_nightly, coverage(off))]
fn prompt_line(prompt: &str) -> Result<String> {
    use std::io::{stderr, stdin, Write};
    eprint!("{prompt}");
    stderr().flush().ok();
    let mut buf = String::new();
    stdin()
        .read_line(&mut buf)
        .context("reading the pasted sign-in callback")?;
    Ok(buf)
}

/// Map a non-Success outcome to an `Err` with a user-readable explanation.
/// Success passes through verbatim so the caller can keep unwrapping fields.
#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn read_token(args: &LoginArgs) -> Result<String> {
    if let Some(t) = &args.token {
        eprintln!(
            "warning: passing --token on the command line exposes it in shell history and `ps`; prefer `--with-token` (stdin) or the interactive prompt"
        );
        return Ok(t.clone());
    }
    if let Some(t) = token_store::env_token() {
        return Ok(t);
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn prompt_hostname() -> Result<String> {
    eprint!("Hostname or URL (e.g. rasterhub.com or http://localhost:3000): ");
    use std::io::{stderr, stdin, Write};
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading hostname")?;
    Ok(buf)
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn login_args() -> LoginArgs {
        LoginArgs {
            host: None,
            token: None,
            with_token: false,
            git_protocol: "https".into(),
            fjord: false,
            manual: false,
            device: false,
            platform_url: None,
        }
    }

    fn signed_in_host(instance: Option<&str>) -> Host {
        Host {
            user: Some("stephen@example.com".into()),
            git_protocol: "https".into(),
            web_url: None,
            api_base_path: None,
            auth_kind: AuthKind::FjordAccount,
            fjord_account: Some(FjordAccount {
                platform_url: "https://fjord.sh".into(),
                default_instance_id: instance.map(str::to_string),
                default_instance_display_name: instance.map(|_| "rasterhub.com".to_string()),
                default_instance_slug: instance.map(|_| "rasterhub-com".to_string()),
                default_instance_public_url: instance.map(|_| "https://rasterhub.com".to_string()),
                auth_flow: Some("oidc".into()),
            }),
        }
    }

    #[test]
    fn signing_in_again_keeps_the_instance_already_selected() {
        let previous = signed_in_host(Some("819eadfe"));

        let account = fjord_account_for("https://fjord.sh", "oidc", Some(&previous));

        // Re-login is the documented recovery from an expired session. Losing
        // the selection here means the next command fails with "no default
        // Fjord instance selected" straight after a successful sign-in.
        assert_eq!(account.default_instance_id.as_deref(), Some("819eadfe"));
        assert_eq!(
            account.default_instance_public_url.as_deref(),
            Some("https://rasterhub.com")
        );
        assert_eq!(account.auth_flow.as_deref(), Some("oidc"));
    }

    #[test]
    fn a_first_sign_in_has_no_instance_to_keep() {
        let account = fjord_account_for("https://fjord.sh", "credentials", None);

        assert_eq!(account.default_instance_id, None);
        assert_eq!(account.platform_url, "https://fjord.sh");
        assert_eq!(account.auth_flow.as_deref(), Some("credentials"));
    }

    #[test]
    fn signing_in_to_a_different_platform_does_not_inherit_its_instance() {
        let previous = signed_in_host(Some("819eadfe"));

        // The host entry is keyed by platform URL, but a stale or hand-edited
        // config can disagree. An instance id is only meaningful to the
        // platform that issued it.
        let account = fjord_account_for("https://other.example", "oidc", Some(&previous));

        assert_eq!(account.default_instance_id, None);
    }

    #[test]
    fn a_pat_host_at_the_same_key_carries_no_instance_to_keep() {
        let previous = Host {
            auth_kind: AuthKind::Pat,
            fjord_account: None,
            ..signed_in_host(Some("819eadfe"))
        };

        let account = fjord_account_for("https://fjord.sh", "oidc", Some(&previous));

        assert_eq!(account.default_instance_id, None);
    }

    struct EnvRestore {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn fjord_mode_wins_over_generic_env_token() {
        let _token = EnvRestore::set("FJ_TOKEN", "env-token");
        let mut args = login_args();
        args.fjord = true;

        assert_eq!(resolve_login_mode(&args, false), LoginMode::Fjord);

        let args = login_args();
        assert_eq!(resolve_login_mode(&args, false), LoginMode::Pat);
    }

    #[test]
    fn choose_oidc_capture_precedence() {
        // Explicit --device always wins (even on a normal desktop), and is not
        // marked auto.
        assert_eq!(
            choose_oidc_capture(true, false, false),
            OidcCapture::Device { auto: false }
        );
        // Explicit --device still wins if the env also looks headless.
        assert_eq!(
            choose_oidc_capture(true, false, true),
            OidcCapture::Device { auto: false }
        );
        // Explicit --manual is honored when device isn't requested.
        assert_eq!(choose_oidc_capture(false, true, false), OidcCapture::Manual);
        assert_eq!(choose_oidc_capture(false, true, true), OidcCapture::Manual);
        // No flags on a headless / SSH session: auto-select device flow.
        assert_eq!(
            choose_oidc_capture(false, false, true),
            OidcCapture::Device { auto: true }
        );
        // No flags on a normal desktop: keep the same-device loopback default.
        assert_eq!(
            choose_oidc_capture(false, false, false),
            OidcCapture::Loopback
        );
    }
}
