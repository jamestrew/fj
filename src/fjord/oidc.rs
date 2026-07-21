//! Fjord Account sign-in under SSO/OIDC.
//!
//! On the production fjord-platform deployment `GET /api/auth/status` reports
//! `{"strategy":"oidc"}`: the email + password `/api/v1/sign-in` path (see
//! [`super::sign_in`]) is unavailable and returns `StrategyMismatch`. This
//! module drives the standards-compliant alternative a CLI can run headlessly
//! on the user's own machine: the OAuth 2.0 Authorization Code grant with PKCE
//! (RFC 7636) over a loopback redirect (RFC 8252 §7.3).
//!
//! Paragon (the identity provider behind fjord-platform) advertises
//! `authorization_code` + `refresh_token` grants and `S256` PKCE in its
//! discovery document, and exposes NO device-authorization endpoint, so
//! loopback + PKCE is the supported shape (not device code). fjord-platform
//! re-exposes Paragon's `/oauth/*` surface under its own origin
//! (`{platform}/oauth/authorize`, `{platform}/oauth/token`), the same front
//! door the Fjord iOS app signs in through as a public PKCE client. `fj` is
//! the `fj-cli` public client: no client secret, identity proven by PKCE.
//!
//! The `access_token` the token endpoint returns is a Paragon
//! `SignedSessionToken` in the exact base64-JSON envelope
//! `/api/v1/me/forge-instances` already accepts as a bearer, so once stored it
//! flows through [`super::list_instances`] and the forge gateway unchanged. The
//! `refresh_token` enables silent renewal via the `refresh_token` grant.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use crate::auth as token_store;

/// Default OIDC client id for the CLI. A public (PKCE, no-secret) client
/// registered in Paragon with the loopback redirect URIs in [`CALLBACK_PORTS`].
/// Overridable with `FJ_OIDC_CLIENT_ID` for staging deployments that register
/// the CLI under a different id.
pub const DEFAULT_CLIENT_ID: &str = "fj-cli";

/// Scopes requested at `/oauth/authorize`. `offline_access` asks Paragon for a
/// refresh token so the short-lived access token can be renewed without another
/// browser round-trip.
const SCOPES: &str = "openid email profile offline_access";

/// Loopback ports the CLI tries in order for the redirect listener. Every one
/// of `http://127.0.0.1:<port>/callback` must be registered as a redirect URI
/// on the `fj-cli` client, because Paragon validates redirect URIs by exact
/// match (it does not apply the RFC 8252 §7.3 loopback-port exemption). The CLI
/// binds the first free port and uses that exact URI in both the authorize
/// request and the token exchange.
const CALLBACK_PORTS: &[u16] = &[47017, 47018, 47019, 47020];

/// How long to wait for the browser redirect before giving up. Matches
/// Paragon's authorization-code TTL (5 minutes): a code minted right at the
/// deadline would already be dead, so there is no point waiting longer.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// Resolve the OIDC client id: `FJ_OIDC_CLIENT_ID` if set and non-empty, else
/// [`DEFAULT_CLIENT_ID`].
pub fn client_id() -> String {
    std::env::var("FJ_OIDC_CLIENT_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

/// Sign-in shape a deployment offers, from `GET /api/auth/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStrategy {
    /// Email + password via `/api/v1/sign-in` (the existing flow).
    Credentials,
    /// Authorization Code + PKCE over `/oauth/*` (this module).
    Oidc,
    /// Dev / pending deployment: no live sign-in. Callers fall back to PAT.
    Mock,
    /// A value this build doesn't recognize. Carried verbatim so the caller
    /// can report it rather than silently mis-routing.
    Unknown(&'static str),
}

#[derive(Debug, Deserialize)]
struct AuthStatusBody {
    #[serde(default)]
    strategy: Option<String>,
}

/// `GET {platform}/api/auth/status`. Always answered 200 by the platform; a
/// transport error or unparseable body is surfaced so the caller can decide
/// whether to fall back rather than guess a strategy.
pub async fn probe_strategy(platform_url: &str) -> Result<AuthStrategy> {
    let url = format!("{platform_url}/api/auth/status");
    let http = super::http_client()?;
    let res = http
        .get(&url)
        .header(USER_AGENT, super::user_agent())
        .header(ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading auth-status body (HTTP {status})"))?;
    let body: AuthStatusBody = serde_json::from_str(&text).with_context(|| {
        format!(
            "decoding auth-status (HTTP {status}); body was: {}",
            super::preview(&text)
        )
    })?;
    Ok(match body.strategy.as_deref() {
        Some("credentials") => AuthStrategy::Credentials,
        Some("oidc") => AuthStrategy::Oidc,
        Some("mock") => AuthStrategy::Mock,
        // An absent strategy means the deployment predates the field or is in a
        // mode with no live sign-in; treat it like `mock` (fall back to PAT).
        None => AuthStrategy::Mock,
        Some(_) => AuthStrategy::Unknown("unrecognized"),
    })
}

/// PKCE material for one sign-in attempt: the verifier (kept secret, sent only
/// at token exchange), its S256 challenge (sent in the authorize redirect), and
/// the CSRF `state` + OIDC `nonce`.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
    pub nonce: String,
}

impl Pkce {
    /// Mint fresh PKCE + state + nonce from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let verifier = random_token()?;
        let challenge = pkce_challenge(&verifier);
        Ok(Self {
            verifier,
            challenge,
            state: random_token()?,
            nonce: random_token()?,
        })
    }
}

/// 32 bytes of OS randomness, base64url-no-pad (43 chars). Used for the PKCE
/// verifier (RFC 7636 §4.1 allows 43-128 chars from the unreserved set) and for
/// `state` / `nonce`.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow!("OS randomness unavailable: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// S256 code challenge: base64url-no-pad(SHA-256(verifier)) (RFC 7636 §4.2).
fn pkce_challenge(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// Build the `{platform}/oauth/authorize` URL the user's browser opens.
pub fn build_authorize_url(
    platform_url: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
) -> String {
    // `url` for correct percent-encoding of every query value (the redirect URI
    // carries a `:` and `/`, the scopes carry spaces).
    let mut url = url::Url::parse(platform_url)
        .expect("platform_url is normalized/validated before reaching here");
    url.set_path("/oauth/authorize");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", &pkce.state)
        .append_pair("nonce", &pkce.nonce)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

/// A bound loopback listener plus the exact redirect URI it corresponds to.
pub struct LoopbackServer {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl LoopbackServer {
    /// Bind the first free port in [`CALLBACK_PORTS`] on `127.0.0.1`.
    pub fn bind() -> Result<Self> {
        let mut last_err = None;
        for &port in CALLBACK_PORTS {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => {
                    return Ok(Self {
                        listener,
                        redirect_uri: format!("http://127.0.0.1:{port}/callback"),
                    });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(anyhow!(
            "could not bind any loopback callback port ({:?}); is another `fj auth login` running? ({})",
            CALLBACK_PORTS,
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no ports configured".into())
        ))
    }

    /// Block (on a blocking thread) until the browser hits the redirect, then
    /// return the authorization `code`. Verifies the returned `state` matches
    /// `expected_state` (CSRF) and surfaces a provider `error=` verbatim.
    pub async fn wait_for_code(self, expected_state: String) -> Result<String> {
        let listener = self.listener;
        let deadline = Instant::now() + CALLBACK_TIMEOUT;
        tokio::task::spawn_blocking(move || accept_code(&listener, &expected_state, deadline))
            .await
            .context("loopback callback task panicked")?
    }
}

/// Poll the loopback listener until a `/callback` request carrying `code` (or
/// `error`) arrives, or the deadline passes.
fn accept_code(listener: &TcpListener, expected_state: &str, deadline: Instant) -> Result<String> {
    listener
        .set_nonblocking(true)
        .context("configuring loopback listener")?;
    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for the browser sign-in to complete"
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => match handle_connection(stream, expected_state)? {
                Some(code) => return Ok(code),
                // A stray hit (favicon, empty path): keep waiting for the real
                // redirect rather than aborting the whole sign-in.
                None => continue,
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(anyhow!("accepting loopback connection: {e}")),
        }
    }
}

/// Read one request line, extract the callback outcome, and write a small HTML
/// page back to the browser. Returns `Ok(Some(code))` on success, `Ok(None)`
/// for an unrelated request to ignore, and `Err` for a provider error or a
/// state mismatch (which must abort the sign-in).
fn handle_connection(stream: std::net::TcpStream, expected_state: &str) -> Result<Option<String>> {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("reading loopback request")?;
    // "GET /callback?code=…&state=… HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    match parse_callback(target) {
        CallbackOutcome::Ignore => {
            write_response(stream, "404 Not Found", "Not found.");
            Ok(None)
        }
        CallbackOutcome::Error(msg) => {
            write_response(
                stream,
                "400 Bad Request",
                "Sign-in failed. You can close this window and return to your terminal.",
            );
            Err(anyhow!("sign-in was denied by the provider: {msg}"))
        }
        CallbackOutcome::Code { code, state } => {
            if state != expected_state {
                write_response(
                    stream,
                    "400 Bad Request",
                    "Sign-in state mismatch. Close this window and try again.",
                );
                return Err(anyhow!(
                    "OAuth state mismatch on the callback; aborting to avoid a possible CSRF"
                ));
            }
            write_response(
                stream,
                "200 OK",
                "Signed in to fj. You can close this window and return to your terminal.",
            );
            Ok(Some(code))
        }
    }
}

/// Outcome of parsing the loopback request target's query string.
#[derive(Debug, PartialEq, Eq)]
enum CallbackOutcome {
    Code { code: String, state: String },
    Error(String),
    Ignore,
}

/// Pure parse of the request target (`/callback?code=…&state=…`). Anything
/// without a `code` or `error` (favicon, a bare `/`) is [`CallbackOutcome::Ignore`].
fn parse_callback(target: &str) -> CallbackOutcome {
    // Give the relative target a dummy base so `url` can parse the query.
    let Ok(parsed) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackOutcome::Ignore;
    };
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(err) = error {
        return CallbackOutcome::Error(err);
    }
    match (code, state) {
        (Some(code), Some(state)) => CallbackOutcome::Code { code, state },
        // A `code` with no `state` can't be CSRF-checked; refuse it.
        _ => CallbackOutcome::Ignore,
    }
}

fn write_response(mut stream: std::net::TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>fj</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;text-align:center\">\
         <h1>fj</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A successful `/oauth/token` response (both the code and refresh grants).
/// Only `access_token` and `refresh_token` are load-bearing for the CLI: the
/// access token becomes the Fjord bearer and the refresh token enables silent
/// renewal. `id_token` / `expires_in` are part of the wire shape but unused
/// (renewal is reactive on a 401, not clock-driven), so serde simply ignores
/// them.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// Exchange the authorization `code` for tokens at `{platform}/oauth/token`.
/// Public client: `client_id` in the body, no secret (Paragon's token endpoint
/// short-circuits secret validation for non-confidential clients).
pub async fn exchange_code(
    platform_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    post_token(platform_url, &form).await
}

/// Exchange a refresh token for a fresh access (and rotated refresh) token.
/// Paragon rotates on every use and detects replay, so the caller MUST persist
/// the returned `refresh_token` and discard the one it sent.
pub async fn exchange_refresh(
    platform_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    post_token(platform_url, &form).await
}

async fn post_token(platform_url: &str, form: &[(&str, &str)]) -> Result<TokenResponse> {
    let url = format!("{platform_url}/oauth/token");
    let http = super::http_client()?;
    let res = http
        .post(&url)
        .header(USER_AGENT, super::user_agent())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(form)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading token response (HTTP {status})"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "token endpoint returned HTTP {status}: {}",
            super::preview(&text)
        ));
    }
    serde_json::from_str::<TokenResponse>(&text)
        .with_context(|| format!("decoding token response: {}", super::preview(&text)))
}

/// A refresh token persisted together with the identity of the account that
/// obtained it. Binding the identity is what keeps a refresh usable only for
/// that account: without it, a refresh token left behind by a previously
/// signed-in account on the same platform URL could be replayed on a 401 to
/// silently restore that account (fj#219).
#[derive(Debug, Serialize, Deserialize)]
struct StoredRefresh {
    identity: String,
    refresh_token: String,
}

/// Parse the identity-bound refresh record stored for `platform_url`. A missing
/// or unparseable value (including a bare legacy token) reads as `None`.
fn stored_refresh(platform_url: &str) -> Result<Option<StoredRefresh>> {
    Ok(token_store::load_fjord_refresh(platform_url)?
        .and_then(|raw| serde_json::from_str::<StoredRefresh>(&raw).ok()))
}

/// Pure identity check: yield the stored refresh token only when it was issued
/// to the same account the `current_bearer` identifies.
fn select_valid_refresh(stored: Option<StoredRefresh>, current_bearer: &str) -> Option<String> {
    let record = stored?;
    match bearer_identity(current_bearer) {
        Some(id) if id == record.identity => Some(record.refresh_token),
        _ => None,
    }
}

/// Load the stored refresh token for `platform_url`, but only if it belongs to
/// the account identified by `current_bearer`. A token issued to a different
/// account reads as `None` so callers fall back to interactive sign-in rather
/// than resurrecting a stale session (fj#219).
pub fn valid_refresh_token(platform_url: &str, current_bearer: &str) -> Result<Option<String>> {
    Ok(select_valid_refresh(
        stored_refresh(platform_url)?,
        current_bearer,
    ))
}

/// Persist the tokens from a sign-in / refresh: the access token in the Fjord
/// bearer slot (what every downstream call reads) and, when present, the
/// refresh token bound to the signing-in account's identity. Returns where the
/// access token landed so the caller can warn on a file-store fallback.
pub fn store_tokens(
    platform_url: &str,
    tokens: &TokenResponse,
) -> Result<token_store::TokenStoreLocation> {
    let location = token_store::store_fjord_bearer(platform_url, &tokens.access_token)?;
    let identity = bearer_identity(&tokens.access_token);
    match (tokens.refresh_token.as_deref(), identity.as_deref()) {
        // Persist the new refresh token bound to the account that just proved
        // it, overwriting any token held for this platform URL.
        (Some(rt), Some(id)) => {
            let record = serde_json::to_string(&StoredRefresh {
                identity: id.to_string(),
                refresh_token: rt.to_string(),
            })
            .context("encoding the refresh token record")?;
            token_store::store_fjord_refresh(platform_url, &record)?;
        }
        // Refresh-less issuance (or an access token we can't attribute to an
        // identity): keep an already-stored token only if it belongs to THIS
        // account, so a rotation that returns no new token doesn't strand the
        // session; drop one issued to a different account so a later 401 can't
        // silently restore it (fj#219).
        _ => {
            let keep = matches!(
                (stored_refresh(platform_url)?, identity.as_deref()),
                (Some(record), Some(id)) if record.identity == id
            );
            if !keep {
                token_store::delete_fjord_refresh(platform_url)?;
            }
        }
    }
    Ok(location)
}

/// Silent refresh: load the stored refresh token for the currently signed-in
/// account, exchange it, persist the new tokens, and return the fresh access
/// token. Errors clearly when no usable refresh token is stored (a
/// credentials-flow or PAT host, an OIDC host signed in before refresh support,
/// or a token that belongs to a different account) so the caller can tell the
/// user to sign in again.
pub async fn refresh_session(platform_url: &str) -> Result<String> {
    let bearer = token_store::load_fjord_bearer(platform_url)?.ok_or_else(|| {
        anyhow!("no Fjord Account session stored for {platform_url}; run `fj auth login --fjord` to sign in again")
    })?;
    let refresh_token = valid_refresh_token(platform_url, &bearer)?.ok_or_else(|| {
        anyhow!(
            "no refresh token stored for {platform_url}; run `fj auth login --fjord` to sign in again"
        )
    })?;
    let tokens = exchange_refresh(platform_url, &client_id(), &refresh_token)
        .await
        .context("refreshing the Fjord Account session")?;
    store_tokens(platform_url, &tokens)?;
    Ok(tokens.access_token)
}

/// Decode the non-cryptographic `payload` object from a stored bearer. The
/// access token is `base64(STANDARD, {kid, payload:{…}, signature})`; Paragon
/// re-verifies the signature on every call, so decoding it here is display- and
/// identity-only, never a trust decision. Returns `None` on any mismatch.
fn bearer_payload(access_token: &str) -> Option<serde_json::Value> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(access_token.trim())
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("payload").cloned()
}

/// Best-effort email extraction from a stored bearer for the sign-in success
/// line: reads `payload.email`.
pub fn bearer_email(access_token: &str) -> Option<String> {
    bearer_payload(access_token)?
        .get("email")
        .and_then(|e| e.as_str())
        .map(str::to_string)
}

/// Stable account identity carried in a Fjord access token: the immutable
/// `user_id` when present, else the `email`. A stored refresh token is bound to
/// this identity so a token left behind by a previously signed-in account on
/// the same platform URL can never be replayed for a different one (fj#219).
pub fn bearer_identity(access_token: &str) -> Option<String> {
    let payload = bearer_payload(access_token)?;
    payload
        .get("user_id")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("email").and_then(|v| v.as_str()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generate_pkce_is_urlsafe_and_distinct() {
        let p = Pkce::generate().unwrap();
        // 32 bytes base64url-no-pad = 43 chars, no padding, url-safe alphabet.
        assert_eq!(p.verifier.len(), 43);
        assert!(!p.verifier.contains('='));
        assert!(p
            .verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(p.verifier, p.state);
        assert_ne!(p.state, p.nonce);
        // Challenge is the S256 of THIS verifier, not the verifier itself.
        assert_eq!(p.challenge, pkce_challenge(&p.verifier));
        assert_ne!(p.challenge, p.verifier);
    }

    #[test]
    fn authorize_url_has_pkce_and_encoded_redirect() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "chal".into(),
            state: "st8".into(),
            nonce: "non".into(),
        };
        let url = build_authorize_url(
            "https://forgejo-site.pages.dev",
            "fj-cli",
            "http://127.0.0.1:47017/callback",
            &pkce,
        );
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.host_str(), Some("forgejo-site.pages.dev"));
        assert_eq!(parsed.path(), "/oauth/authorize");
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "fj-cli");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:47017/callback");
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st8");
        assert_eq!(q["nonce"], "non");
        assert_eq!(q["scope"], SCOPES);
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        assert_eq!(
            parse_callback("/callback?code=abc123&state=xyz"),
            CallbackOutcome::Code {
                code: "abc123".into(),
                state: "xyz".into(),
            }
        );
    }

    #[test]
    fn parse_callback_surfaces_provider_error() {
        assert_eq!(
            parse_callback("/callback?error=access_denied&error_description=nope"),
            CallbackOutcome::Error("access_denied".into())
        );
    }

    #[test]
    fn parse_callback_ignores_favicon_and_stateless_code() {
        assert_eq!(parse_callback("/favicon.ico"), CallbackOutcome::Ignore);
        assert_eq!(parse_callback("/"), CallbackOutcome::Ignore);
        // A code with no state can't be CSRF-checked, so it's ignored, not used.
        assert_eq!(
            parse_callback("/callback?code=abc"),
            CallbackOutcome::Ignore
        );
    }

    #[test]
    fn client_id_prefers_env_override() {
        // Default when unset.
        std::env::remove_var("FJ_OIDC_CLIENT_ID");
        assert_eq!(client_id(), "fj-cli");
        std::env::set_var("FJ_OIDC_CLIENT_ID", "  staging-cli  ");
        assert_eq!(client_id(), "staging-cli");
        std::env::set_var("FJ_OIDC_CLIENT_ID", "   ");
        assert_eq!(client_id(), "fj-cli");
        std::env::remove_var("FJ_OIDC_CLIENT_ID");
    }

    #[test]
    fn bearer_email_reads_payload_email() {
        // Same envelope shape Paragon mints: base64(STANDARD, {kid,payload,signature}).
        let token = base64::engine::general_purpose::STANDARD.encode(
            serde_json::json!({
                "kid": "k1",
                "payload": { "user_id": "u1", "email": "dev@example.com", "expires_at": 1 },
                "signature": "AAA="
            })
            .to_string(),
        );
        assert_eq!(bearer_email(&token).as_deref(), Some("dev@example.com"));
        assert_eq!(bearer_email("not-base64!!!"), None);
    }

    /// Mint a bearer in the same envelope Paragon issues:
    /// `base64(STANDARD, {kid, payload, signature})`.
    fn fake_bearer(user_id: Option<&str>, email: Option<&str>) -> String {
        let mut payload = serde_json::Map::new();
        if let Some(u) = user_id {
            payload.insert("user_id".into(), u.into());
        }
        if let Some(e) = email {
            payload.insert("email".into(), e.into());
        }
        base64::engine::general_purpose::STANDARD.encode(
            serde_json::json!({ "kid": "k1", "payload": payload, "signature": "AAA=" }).to_string(),
        )
    }

    #[test]
    fn bearer_identity_prefers_user_id_then_email() {
        let both = fake_bearer(Some("u-123"), Some("a@example.com"));
        assert_eq!(bearer_identity(&both).as_deref(), Some("u-123"));
        let email_only = fake_bearer(None, Some("a@example.com"));
        assert_eq!(
            bearer_identity(&email_only).as_deref(),
            Some("a@example.com")
        );
        // No attributable field, or an undecodable token, has no identity.
        assert_eq!(bearer_identity(&fake_bearer(None, None)), None);
        assert_eq!(bearer_identity("not-base64!!!"), None);
    }

    #[test]
    fn refresh_token_is_bound_to_the_account_that_obtained_it() {
        let account_a = fake_bearer(Some("u-A"), Some("a@example.com"));
        let account_b = fake_bearer(Some("u-B"), Some("b@example.com"));
        let record = serde_json::to_string(&StoredRefresh {
            identity: bearer_identity(&account_a).unwrap(),
            refresh_token: "refresh-A".into(),
        })
        .unwrap();
        let parsed = || serde_json::from_str::<StoredRefresh>(&record).ok();

        // The account that obtained it can use it...
        assert_eq!(
            select_valid_refresh(parsed(), &account_a).as_deref(),
            Some("refresh-A")
        );
        // ...but a different account on the same platform URL cannot: this is
        // the fj#219 replay the identity binding closes.
        assert_eq!(select_valid_refresh(parsed(), &account_b), None);
        // Nothing stored, or a bearer we can't attribute, yields nothing.
        assert_eq!(select_valid_refresh(None, &account_a), None);
        assert_eq!(select_valid_refresh(parsed(), "garbage-bearer"), None);
        // A bare legacy token (no envelope) is not a valid record, so it reads
        // as absent rather than being used.
        assert!(serde_json::from_str::<StoredRefresh>("bare-legacy-token").is_err());
    }

    #[test]
    fn auth_status_body_parses_strategy() {
        let b: AuthStatusBody =
            serde_json::from_str(r#"{"mode":"live","strategy":"oidc"}"#).unwrap();
        assert_eq!(b.strategy.as_deref(), Some("oidc"));
    }

    // ---- wiremock-backed network tests. These mock the platform's
    // /api/auth/status and /oauth/token endpoints exactly as the issue asks. ----

    mod wire {
        use super::super::*;
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn probe_strategy_maps_each_value() {
            for (wire, expected) in [
                ("oidc", AuthStrategy::Oidc),
                ("credentials", AuthStrategy::Credentials),
                ("mock", AuthStrategy::Mock),
            ] {
                let server = MockServer::start().await;
                Mock::given(method("GET"))
                    .and(path("/api/auth/status"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "mode": "live",
                        "strategy": wire,
                    })))
                    .expect(1)
                    .mount(&server)
                    .await;
                assert_eq!(probe_strategy(&server.uri()).await.unwrap(), expected);
            }
        }

        #[tokio::test]
        async fn probe_strategy_absent_field_is_mock() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/auth/status"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "mode": "mock" })),
                )
                .mount(&server)
                .await;
            assert_eq!(
                probe_strategy(&server.uri()).await.unwrap(),
                AuthStrategy::Mock
            );
        }

        #[tokio::test]
        async fn exchange_code_sends_pkce_and_no_secret_and_parses_tokens() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .and(header("content-type", "application/x-www-form-urlencoded"))
                .and(body_string_contains("grant_type=authorization_code"))
                .and(body_string_contains("client_id=fj-cli"))
                .and(body_string_contains("code=the-code"))
                .and(body_string_contains("code_verifier=the-verifier"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc",
                    "id_token": "idt",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "ref",
                })))
                .expect(1)
                .mount(&server)
                .await;
            let tokens = exchange_code(
                &server.uri(),
                "fj-cli",
                "the-code",
                "http://127.0.0.1:47017/callback",
                "the-verifier",
            )
            .await
            .unwrap();
            assert_eq!(tokens.access_token, "acc");
            assert_eq!(tokens.refresh_token.as_deref(), Some("ref"));

            // A public client must send NO client_secret on the exchange.
            let reqs = server.received_requests().await.unwrap();
            let body = String::from_utf8(reqs[0].body.clone()).unwrap();
            assert!(
                !body.contains("client_secret"),
                "public client must not send a client_secret; body was: {body}"
            );
        }

        #[tokio::test]
        async fn exchange_refresh_sends_refresh_grant_and_parses_rotation() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .and(body_string_contains("grant_type=refresh_token"))
                .and(body_string_contains("refresh_token=old-refresh"))
                .and(body_string_contains("client_id=fj-cli"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc2",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "rotated-refresh",
                })))
                .expect(1)
                .mount(&server)
                .await;
            let tokens = exchange_refresh(&server.uri(), "fj-cli", "old-refresh")
                .await
                .unwrap();
            assert_eq!(tokens.access_token, "acc2");
            assert_eq!(tokens.refresh_token.as_deref(), Some("rotated-refresh"));
        }

        #[tokio::test]
        async fn token_endpoint_error_surfaces_status_and_body() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": "invalid_grant",
                })))
                .mount(&server)
                .await;
            let err = exchange_refresh(&server.uri(), "fj-cli", "dead")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("400"), "want status in error: {err}");
            assert!(err.contains("invalid_grant"), "want body in error: {err}");
        }
    }
}
