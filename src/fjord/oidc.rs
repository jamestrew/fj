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
use std::net::TcpListener;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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

/// How long a single accepted connection gets to send its request line before
/// the CLI gives up on it and goes back to accepting. Connections are handled
/// one at a time, so a socket that connects and then stalls (a browser
/// preconnect, a port scanner) would otherwise hold the loopback open ahead of
/// the real redirect forever; this caps that to a bounded delay.
const REQUEST_LINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve the OIDC client id: `FJ_OIDC_CLIENT_ID` if set and non-empty, else
/// [`DEFAULT_CLIENT_ID`].
pub fn client_id() -> String {
    std::env::var("FJ_OIDC_CLIENT_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

/// The redirect URI `--manual` sign-in uses. Manual mode binds no loopback
/// listener, so it can't discover a free port the way [`LoopbackServer::bind`]
/// does; it pins the first (canonical) registered callback instead. This must
/// stay a redirect URI registered on the `fj-cli` client because Paragon
/// validates by exact match, and it must be the exact string sent in both the
/// authorize request and the token exchange.
pub fn default_redirect_uri() -> String {
    format!("http://127.0.0.1:{}/callback", CALLBACK_PORTS[0])
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
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading auth-status body (HTTP {status})"))?;
    super::ensure_json_platform_response("auth-status endpoint", status, Some(&headers), &text)?;
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

    /// Wait until the browser hits the redirect, then return the authorization
    /// `code`. Verifies the returned `state` matches `expected_state` (CSRF)
    /// and surfaces a provider `error=` verbatim.
    ///
    /// Async all the way down, deliberately. `cli::run` races every command
    /// against SIGINT, and only a cancellable future lets Ctrl+C actually end
    /// the process: a `spawn_blocking` accept loop keeps running after the
    /// select drops it, and dropping the tokio runtime waits for blocking
    /// tasks, so the CLI would print `error: interrupted` and then sit there
    /// until [`CALLBACK_TIMEOUT`] elapsed.
    pub async fn wait_for_code(self, expected_state: String) -> Result<String> {
        self.listener
            .set_nonblocking(true)
            .context("configuring loopback listener")?;
        let listener = tokio::net::TcpListener::from_std(self.listener)
            .context("registering the loopback listener with the runtime")?;
        tokio::time::timeout(CALLBACK_TIMEOUT, accept_code(&listener, &expected_state))
            .await
            .unwrap_or_else(|_| {
                Err(anyhow!(
                    "timed out waiting for the browser sign-in to complete"
                ))
            })
    }
}

/// Accept loopback connections until one carries a `/callback` request with a
/// `code` (or `error`). The overall deadline lives in the caller's
/// [`tokio::time::timeout`], so this loop is free to await indefinitely.
async fn accept_code(listener: &tokio::net::TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accepting loopback connection")?;
        match handle_connection(stream, expected_state).await? {
            Some(code) => return Ok(code),
            // A stray hit (favicon, empty path, a browser preconnect that never
            // sends a request): keep waiting for the real redirect rather than
            // aborting the whole sign-in.
            None => continue,
        }
    }
}

/// Read one request line, extract the callback outcome, and write a small HTML
/// page back to the browser. Returns `Ok(Some(code))` on success, `Ok(None)`
/// for an unrelated request to ignore, and `Err` for a provider error or a
/// state mismatch (which must abort the sign-in).
async fn handle_connection(
    stream: tokio::net::TcpStream,
    expected_state: &str,
) -> Result<Option<String>> {
    let mut reader = tokio::io::BufReader::new(stream);
    let mut request_line = String::new();
    // Bounded: a socket that connects and then says nothing (browsers open
    // speculative preconnects) must not starve the real redirect behind it.
    match tokio::time::timeout(REQUEST_LINE_TIMEOUT, reader.read_line(&mut request_line)).await {
        Ok(res) => {
            res.context("reading loopback request")?;
        }
        Err(_) => return Ok(None),
    }
    let stream = reader.into_inner();
    // "GET /callback?code=…&state=… HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    match parse_callback(target) {
        CallbackOutcome::Ignore => {
            write_page(stream, CallbackPage::NotFound).await;
            Ok(None)
        }
        CallbackOutcome::Error(msg) => {
            write_page(stream, CallbackPage::Failed).await;
            Err(anyhow!("sign-in was denied by the provider: {msg}"))
        }
        CallbackOutcome::Code { code, state } => {
            if state != expected_state {
                write_page(stream, CallbackPage::StateMismatch).await;
                return Err(anyhow!(
                    "OAuth state mismatch on the callback; aborting to avoid a possible CSRF"
                ));
            }
            write_page(stream, CallbackPage::Success).await;
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

/// A `code` (and optional CSRF `state`) parsed from a `--manual` paste.
#[derive(Debug, PartialEq, Eq)]
pub struct PastedCode {
    pub code: String,
    /// The `state` when the paste carried one (a full URL or query). `None` for
    /// a bare code, which the caller then cannot CSRF-check.
    pub state: Option<String>,
}

impl PastedCode {
    /// CSRF guard for the manual flow: when the paste carried a `state`, require
    /// it to equal the value this sign-in attempt generated. A bare code (no
    /// state) passes — it can't be checked here, but the PKCE verifier held only
    /// by this process still binds the exchange, and the code is single-use and
    /// short-lived.
    pub fn check_state(&self, expected: &str) -> Result<()> {
        match self.state.as_deref() {
            Some(state) if state != expected => Err(anyhow!(
                "the pasted sign-in `state` doesn't match this attempt; start over with \
                 `fj auth login --fjord --manual` (a stale paste, or a possible CSRF)"
            )),
            _ => Ok(()),
        }
    }
}

/// Parse what the user pastes back in `--manual` sign-in. Accepts three shapes:
/// the whole redirected callback URL
/// (`http://127.0.0.1:47017/callback?code=…&state=…`, even though that page
/// failed to load on the other device), just its query string
/// (`code=…&state=…`), or a bare `code`. Returns the `code` plus any `state`
/// for the caller to compare against the value it generated. A provider
/// `error=…`, an empty paste, or a query with no `code` is an error.
///
/// Pure (no I/O) so the URL-or-bare-code parsing is unit-tested directly.
pub fn parse_pasted_callback(input: &str) -> Result<PastedCode> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "nothing pasted; copy the redirected URL (or the code value) and press Enter"
        ));
    }

    // A query is present if there's a `?`, or a bare `k=v` pair we recognize.
    // A value with none of those is treated as a bare code.
    let query_start = trimmed.find('?');
    let looks_like_query = query_start.is_some()
        || trimmed.contains("code=")
        || trimmed.contains("error=")
        || trimmed.contains("state=");
    if !looks_like_query {
        if trimmed.contains("://") {
            return Err(anyhow!(
                "that URL has no `?code=…` in it; copy the FULL redirected address, or just the code value"
            ));
        }
        // Bare code: nothing to CSRF-check against.
        return Ok(PastedCode {
            code: trimmed.to_string(),
            state: None,
        });
    }

    // Take everything after the first `?` (a full URL / path), or the whole
    // string when it's a bare `code=…&state=…` query. Drop any `#fragment`.
    let query = match query_start {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    };
    let query = query.split('#').next().unwrap_or(query);

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Err(anyhow!(
            "the provider returned an error instead of a code: {err}"
        ));
    }
    match code {
        Some(code) if !code.is_empty() => Ok(PastedCode { code, state }),
        _ => Err(anyhow!(
            "couldn't find a `code` in what you pasted; copy the whole redirected URL \
             (…/callback?code=…&state=…) or just the code value"
        )),
    }
}

/// The branded page the loopback listener serves back to the browser after the
/// redirect. Manual (`--manual`) sign-in never reaches here: it binds no
/// listener, so the browser (on another device) sees its own can't-connect page
/// and the user pastes the code instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackPage {
    /// Code captured and state verified: sign-in is done.
    Success,
    /// The provider returned `error=…` (e.g. the user denied access).
    Failed,
    /// The callback's `state` didn't match: aborted as a possible CSRF.
    StateMismatch,
    /// A stray request (favicon, `/`) that isn't the sign-in redirect.
    NotFound,
}

impl CallbackPage {
    /// The HTTP status line to serve the page under.
    fn http_status(self) -> &'static str {
        match self {
            CallbackPage::Success => "200 OK",
            CallbackPage::Failed | CallbackPage::StateMismatch => "400 Bad Request",
            CallbackPage::NotFound => "404 Not Found",
        }
    }
}

/// The Fjord brand mark (`fjord-brand/assets/fjord-mark.svg`), inlined so the
/// loopback page is fully self-contained: it is served with no network, so no
/// external asset URL would load. `class`/`viewBox` let CSS size it; the fixed
/// fills are the brand's own colors and read on both light and dark surfaces.
const FJORD_MARK_SVG: &str = r##"<svg class="mark" viewBox="0 0 1802 1788" fill="none" role="img" aria-label="Fjord" aria-hidden="true"><rect x="217" y="1075" width="1091" height="713" fill="#83C9D1"/><path d="M1198.08 311.303L957.528 1150.87L1221.66 1226.34L1499.95 679.202L1688.61 245.267C1738.93 229.546 1810.25 70.7518 1787.39 33.0183C1764.52 -4.71514 1712.01 -1.57069 1688.61 4.71822C1586.42 58.174 1365.05 173.576 1297.13 207.536C1229.21 241.496 1202.8 290.864 1198.08 311.303Z" fill="#4A7F91"/><path d="M1150.64 1570.66C1046.87 1664.99 1018.8 1731.4 990.5 1788C1150.87 1788 1509.19 1787.66 1622.5 1788C1698.25 1788.23 1801.54 1703.44 1801.54 1609.11V179.234C1801.54 136.784 1805.98 51.8838 1787.39 51.8838C1768.8 51.8838 1553.37 285 1532.69 316.018C1510.03 350 1452.78 589.586 1419.77 716.936C1386.75 844.287 1235.82 1113.14 1212.23 1179.17C1200.7 1211.46 982.946 1157.2 958.482 1151.11L957.529 1150.87C956.292 1152.19 1086.87 1199.27 1127.27 1216.9C1167.66 1234.54 1287.42 1273.51 1287.42 1372.56C1287.42 1471.61 1212.78 1514.17 1150.64 1570.66Z" fill="#205066"/><path d="M80.225 0C38.7182 0 18.9081 34.589 14.1914 51.8835L561.327 1165.02C561.327 1165.02 696.283 1098.58 842.5 1075C785.9 886.333 613.21 314.465 580.194 268.851C569.393 253.93 570.76 249.984 551.893 231.118C533.027 212.251 121.732 0 80.225 0Z" fill="#4A7F91"/><path d="M0.0446917 122.634C0.0446917 81.1272 9.47806 58.1727 14.1947 51.8838C14.1947 51.8838 263.088 293.77 287.759 334.885C312.43 376 532.083 1125.4 566.044 1174.45C608.494 1235.77 876.755 1264.07 872.628 1330.11C868.501 1396.14 629.659 1437.8 471.71 1504.62C349.082 1556.51 250.026 1702.72 217.009 1787.62C37.7591 1787.62 -1.52754 1641.41 0.0446917 1565.94V122.634Z" fill="#205066"/></svg>"##;

/// The page shell: one self-contained document with inline CSS (no external
/// fonts/styles/scripts, so nothing hits the network) and light/dark support via
/// `prefers-color-scheme`. Palette mirrors the fjord-platform design tokens
/// (`src/app.css`: warm accent, teal, ink/cream surfaces) so the page reads as
/// the same product as fjord.sh. Placeholders are filled by [`render_callback_page`].
const CALLBACK_PAGE_SHELL: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fjord · fj sign-in</title>
<style>
*{box-sizing:border-box}
:root{--bg:#0d0d10;--card:#16161b;--line:#26262e;--fg:#ededf2;--haze:#9a9aa6;--ok:#79c2cf;--bad:#f87171;--warn:#f2a166}
@media (prefers-color-scheme:light){:root{--bg:#f4f1ea;--card:#ffffff;--line:#e4ded1;--fg:#1d2a31;--haze:#5d6b73;--ok:#2c6d7c;--bad:#b4453b;--warn:#b85c1e}}
html,body{margin:0;height:100%}
body{background:var(--bg);color:var(--fg);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;padding:24px;-webkit-font-smoothing:antialiased}
.card{background:var(--card);border:1px solid var(--line);border-radius:16px;max-width:26rem;width:100%;padding:40px 32px;text-align:center;box-shadow:0 1px 2px rgba(0,0,0,.18),0 14px 40px -16px rgba(0,0,0,.5)}
.mark{width:44px;height:44px;margin:0 auto 22px;display:block}
.icon{width:56px;height:56px;border-radius:50%;display:flex;align-items:center;justify-content:center;margin:0 auto 20px;font-size:27px;font-weight:700;line-height:1;border:2px solid currentColor}
.icon.ok{color:var(--ok)}.icon.bad{color:var(--bad)}.icon.warn{color:var(--warn)}
h1{font-size:1.35rem;margin:0 0 10px;letter-spacing:-.01em}
p{margin:0 auto;max-width:22rem;color:var(--haze);font-size:.98rem;line-height:1.55}
.foot{margin-top:28px;padding-top:18px;border-top:1px solid var(--line);color:var(--haze);font-size:.8rem;letter-spacing:.02em}
.foot b{color:var(--fg);font-weight:600}
</style>
</head>
<body>
<main class="card">
__MARK__
<div class="icon __ACCENT__" aria-hidden="true">__GLYPH__</div>
<h1>__TITLE__</h1>
<p>__MESSAGE__</p>
<div class="foot"><b>Fjord</b> CLI · a Raster &amp; State project</div>
</main>
</body>
</html>"##;

/// Render the branded HTML for one [`CallbackPage`]. Pure (string in, string
/// out) so it is unit-testable without a socket.
fn render_callback_page(page: CallbackPage) -> String {
    let (accent, glyph, title, message) = match page {
        CallbackPage::Success => (
            "ok",
            "&check;",
            "Signed in to Fjord",
            "You can close this window and return to your terminal.",
        ),
        CallbackPage::Failed => (
            "bad",
            "&times;",
            "Sign-in failed",
            "Something went wrong completing sign-in. Return to your terminal and try again.",
        ),
        CallbackPage::StateMismatch => (
            "warn",
            "!",
            "Couldn&rsquo;t verify sign-in",
            "This sign-in couldn&rsquo;t be verified (state mismatch), so it was stopped. Return to your terminal and start over.",
        ),
        CallbackPage::NotFound => (
            "warn",
            "?",
            "Nothing to see here",
            "This is the fj sign-in callback. You can close this window.",
        ),
    };
    CALLBACK_PAGE_SHELL
        .replace("__MARK__", FJORD_MARK_SVG)
        .replace("__ACCENT__", accent)
        .replace("__GLYPH__", glyph)
        .replace("__TITLE__", title)
        .replace("__MESSAGE__", message)
}

/// Write a branded [`CallbackPage`] back to the browser over the loopback
/// socket. Best-effort: a write failure never aborts sign-in (the code is
/// already captured), it just means the browser tab shows its own error.
async fn write_page(mut stream: tokio::net::TcpStream, page: CallbackPage) {
    let body = render_callback_page(page);
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        page.http_status(),
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
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
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading token response (HTTP {status})"))?;
    super::ensure_json_platform_response("token endpoint", status, Some(&headers), &text)?;
    if !status.is_success() {
        return Err(anyhow!(
            "token endpoint returned HTTP {status}: {}",
            super::preview(&text)
        ));
    }
    serde_json::from_str::<TokenResponse>(&text)
        .with_context(|| format!("decoding token response: {}", super::preview(&text)))
}

// ---- OAuth 2.0 Device Authorization Grant (RFC 8628) ----
//
// The durable cross-device sign-in: when the browser is on a DIFFERENT device
// than `fj` (an iPad while `fj` runs on a Mac/server, or fully headless), the
// user approves on that other device and `fj` polls for the result. Supersedes
// the `--manual` paste interim. The Paragon OP side is merged (paragon #863) and
// re-exposed by fjord-platform at `{platform}/oauth/device_authorization` and
// `{platform}/oauth/token`. `fj` is the same `fj-cli` public client (no secret;
// device flow needs no PKCE; the device code itself binds the exchange).

/// The `grant_type` a device-code token poll sends (RFC 8628 §3.4).
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// A successful `POST {platform}/oauth/device_authorization` response
/// (RFC 8628 §3.2). `verification_uri_complete` embeds the `user_code` so a
/// scanned QR / tapped link needs no manual code entry; it is RECOMMENDED, not
/// required, so it's optional here and the CLI falls back to `verification_uri`
/// + the code shown separately.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    /// Minimum seconds between polls. Absent means the RFC 8628 default of 5.
    #[serde(default = "default_device_interval")]
    pub interval: u64,
}

fn default_device_interval() -> u64 {
    5
}

impl DeviceAuthorization {
    /// The URL to render as a QR / tappable link: the complete URI (with the
    /// code baked in) when the OP provided one, else the bare verification URI.
    pub fn best_verification_link(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// `POST {platform}/oauth/device_authorization` (form: `client_id`, optional
/// `scope`). Starts a device-flow sign-in and returns the codes to display.
pub async fn request_device_authorization(
    platform_url: &str,
    client_id: &str,
    scope: Option<&str>,
) -> Result<DeviceAuthorization> {
    let url = format!("{platform_url}/oauth/device_authorization");
    let mut form = vec![("client_id", client_id)];
    if let Some(scope) = scope {
        form.push(("scope", scope));
    }
    let http = super::http_client()?;
    let res = http
        .post(&url)
        .header(USER_AGENT, super::user_agent())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&form)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading device-authorization response (HTTP {status})"))?;
    super::ensure_json_platform_response(
        "device-authorization endpoint",
        status,
        Some(&headers),
        &text,
    )?;
    if !status.is_success() {
        return Err(anyhow!(
            "device-authorization endpoint returned HTTP {status}: {}",
            super::preview(&text)
        ));
    }
    serde_json::from_str::<DeviceAuthorization>(&text).with_context(|| {
        format!(
            "decoding device-authorization response: {}",
            super::preview(&text)
        )
    })
}

/// The outcome of one device-token poll. The pending / slow-down / terminal
/// states map to RFC 8628 §3.5 `error` codes; `Approved` carries the tokens.
#[derive(Debug)]
pub enum DevicePoll {
    /// The user approved: sign-in is complete.
    Approved(Box<TokenResponse>),
    /// `authorization_pending`: keep polling at the current interval.
    Pending,
    /// `slow_down`: the OP wants a longer interval (RFC 8628 §3.5: +5s).
    SlowDown,
    /// `access_denied`: the user declined on the other device.
    Denied,
    /// `expired_token`: the device code aged out before approval.
    Expired,
    /// Any other terminal `error` (e.g. `invalid_grant`: consumed / unknown
    /// code / wrong client). Carries the code verbatim for the message.
    Failed(String),
}

/// The `{error, error_description}` body a device-token poll returns on a 400
/// (RFC 8628 §3.5 / RFC 6749 §5.2).
#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    #[serde(default)]
    error: Option<String>,
}

/// One `POST {platform}/oauth/token` device-code poll. A 200 yields
/// [`DevicePoll::Approved`]; a 4xx carrying a recognized OAuth `error` maps to
/// the matching state. A non-OAuth failure (a proxy 502, an HTML error page)
/// is a hard `Err` so the caller stops rather than hot-looping.
pub async fn poll_device_token(
    platform_url: &str,
    client_id: &str,
    device_code: &str,
) -> Result<DevicePoll> {
    let url = format!("{platform_url}/oauth/token");
    let http = super::http_client()?;
    let res = http
        .post(&url)
        .header(USER_AGENT, super::user_agent())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", DEVICE_CODE_GRANT),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading device-token response (HTTP {status})"))?;
    super::ensure_json_platform_response("device-token endpoint", status, Some(&headers), &text)?;
    if status.is_success() {
        let tokens = serde_json::from_str::<TokenResponse>(&text).with_context(|| {
            format!("decoding device-token response: {}", super::preview(&text))
        })?;
        return Ok(DevicePoll::Approved(Box::new(tokens)));
    }
    match parse_device_error(&text).as_deref() {
        Some("authorization_pending") => Ok(DevicePoll::Pending),
        Some("slow_down") => Ok(DevicePoll::SlowDown),
        Some("access_denied") => Ok(DevicePoll::Denied),
        Some("expired_token") => Ok(DevicePoll::Expired),
        Some(other) => Ok(DevicePoll::Failed(other.to_string())),
        // No recognizable OAuth error in the body: a transport/proxy failure,
        // not a poll state. Surface it so the loop aborts.
        None => Err(anyhow!(
            "device-token endpoint returned HTTP {status}: {}",
            super::preview(&text)
        )),
    }
}

/// Pure extraction of the OAuth `error` code from a token-poll body.
fn parse_device_error(body: &str) -> Option<String> {
    serde_json::from_str::<OAuthErrorBody>(body)
        .ok()
        .and_then(|b| b.error)
}

/// Drive the device-token poll loop until the user approves, denies, or the
/// code expires. Waits `interval` between polls (RFC 8628 §3.5), grows it by 5s
/// on each `slow_down`, and gives up at `deadline`. Kept parameterized over
/// `interval`/`deadline` (rather than reading them off the authorization here)
/// so tests can drive it fast.
pub async fn poll_for_device_token(
    platform_url: &str,
    client_id: &str,
    device_code: &str,
    interval: Duration,
    deadline: Instant,
) -> Result<TokenResponse> {
    let mut interval = interval;
    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "the sign-in code expired before it was approved; run `fj auth login --fjord --device` again"
            ));
        }
        tokio::time::sleep(interval).await;
        match poll_device_token(platform_url, client_id, device_code).await? {
            DevicePoll::Approved(tokens) => return Ok(*tokens),
            DevicePoll::Pending => {}
            DevicePoll::SlowDown => interval += Duration::from_secs(5),
            DevicePoll::Denied => {
                return Err(anyhow!(
                    "the sign-in was denied on the other device; run `fj auth login --fjord --device` to try again"
                ))
            }
            DevicePoll::Expired => {
                return Err(anyhow!(
                    "the sign-in code expired before it was approved; run `fj auth login --fjord --device` again"
                ))
            }
            DevicePoll::Failed(err) => {
                return Err(anyhow!("the sign-in could not be completed ({err}); run `fj auth login --fjord --device` again"))
            }
        }
    }
}

/// Render `data` as a QR code of Unicode half-blocks for the terminal, so a
/// phone/iPad camera can jump straight to the verification link. Returns `None`
/// when the data won't fit in a QR symbol (the caller then shows just the URL),
/// so QR rendering is never load-bearing. Colors follow the `qrcode` crate's
/// documented terminal recipe (dark modules drawn light, light modules dark).
pub fn render_qr(data: &str) -> Option<String> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    Some(
        code.render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
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

    /// Regression (Ctrl+C hang): `wait_for_code` must be cancellable. It used
    /// to drive its accept loop from `spawn_blocking`, which keeps running
    /// after `cli::run`'s SIGINT select drops the future, and dropping a tokio
    /// runtime waits for blocking tasks. `fj auth login` therefore printed
    /// `error: interrupted` and then sat there for the rest of
    /// [`CALLBACK_TIMEOUT`]. Asserting on shutdown latency is what catches
    /// that: awaiting the future looks identical either way.
    #[test]
    fn cancelling_wait_for_code_does_not_stall_runtime_shutdown() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        rt.block_on(async {
            let server = LoopbackServer::bind().expect("bind loopback");
            // Cancel the wait the way the SIGINT select does.
            let cancelled = tokio::time::timeout(
                Duration::from_millis(50),
                server.wait_for_code("state".to_string()),
            )
            .await;
            assert!(cancelled.is_err(), "nothing connected, so it must time out");
        });
        let start = Instant::now();
        rt.shutdown_timeout(Duration::from_secs(2));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "runtime shutdown stalled for {:?} after the wait was cancelled; \
             an accept loop is still running on a blocking thread",
            start.elapsed()
        );
    }

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
    fn default_redirect_uri_is_the_first_registered_callback() {
        assert_eq!(default_redirect_uri(), "http://127.0.0.1:47017/callback");
    }

    #[test]
    fn parse_pasted_full_url_extracts_code_and_state() {
        let got =
            parse_pasted_callback("http://127.0.0.1:47017/callback?code=abc123&state=xyz").unwrap();
        assert_eq!(
            got,
            PastedCode {
                code: "abc123".into(),
                state: Some("xyz".into()),
            }
        );
    }

    #[test]
    fn parse_pasted_tolerates_whitespace_query_only_and_fragment() {
        // Leading/trailing whitespace (a sloppy paste).
        assert_eq!(
            parse_pasted_callback("  http://127.0.0.1:47017/callback?code=a&state=b  ")
                .unwrap()
                .code,
            "a"
        );
        // Just the query, no scheme/host.
        assert_eq!(
            parse_pasted_callback("code=a&state=b").unwrap(),
            PastedCode {
                code: "a".into(),
                state: Some("b".into())
            }
        );
        // A `/callback?…` path, plus a trailing fragment to drop.
        assert_eq!(
            parse_pasted_callback("/callback?code=a&state=b#_=_")
                .unwrap()
                .state,
            Some("b".into())
        );
        // Order-independent extraction.
        assert_eq!(
            parse_pasted_callback("http://127.0.0.1:47017/callback?state=b&code=a")
                .unwrap()
                .code,
            "a"
        );
    }

    #[test]
    fn parse_pasted_bare_code_has_no_state() {
        let got = parse_pasted_callback("just-the-code-value").unwrap();
        assert_eq!(
            got,
            PastedCode {
                code: "just-the-code-value".into(),
                state: None,
            }
        );
    }

    #[test]
    fn parse_pasted_rejects_empty_error_and_codeless() {
        // Empty / whitespace-only paste.
        assert!(parse_pasted_callback("   ").is_err());
        // A provider error instead of a code.
        let err = parse_pasted_callback(
            "http://127.0.0.1:47017/callback?error=access_denied&error_description=no",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("access_denied"), "want provider error: {err}");
        // A query that carries a state but no code.
        assert!(parse_pasted_callback("http://127.0.0.1:47017/callback?state=b").is_err());
        // A URL-looking value with no query at all is a truncated paste, not a
        // bare code.
        assert!(parse_pasted_callback("https://forgejo-site.pages.dev/oauth/authorize").is_err());
    }

    #[test]
    fn pasted_state_is_csrf_checked_but_bare_code_passes() {
        // Matching state passes.
        let with_state = PastedCode {
            code: "c".into(),
            state: Some("s".into()),
        };
        assert!(with_state.check_state("s").is_ok());
        // Mismatched state is rejected (the CSRF guard).
        assert!(with_state.check_state("different").is_err());
        // A bare code (no state) passes: nothing to compare, verifier still binds.
        let bare = PastedCode {
            code: "c".into(),
            state: None,
        };
        assert!(bare.check_state("s").is_ok());
    }

    #[test]
    fn callback_page_is_branded_self_contained_and_theme_aware() {
        for page in [
            CallbackPage::Success,
            CallbackPage::Failed,
            CallbackPage::StateMismatch,
            CallbackPage::NotFound,
        ] {
            let html = render_callback_page(page);
            // Self-contained: no external asset the loopback page couldn't load.
            assert!(
                !html.contains("http://"),
                "no external http asset: {page:?}"
            );
            assert!(
                !html.contains("https://"),
                "no external https asset: {page:?}"
            );
            assert!(!html.contains("<script"), "no scripts: {page:?}");
            // Branded + theme-aware.
            assert!(html.contains("Fjord"), "wordmark present: {page:?}");
            assert!(html.contains("<svg"), "inlined mark present: {page:?}");
            assert!(
                html.contains("prefers-color-scheme"),
                "light/dark aware: {page:?}"
            );
            // No leftover template placeholders.
            assert!(!html.contains("__"), "all placeholders filled: {page:?}");
        }
        assert!(render_callback_page(CallbackPage::Success).contains("Signed in to Fjord"));
        assert_eq!(CallbackPage::Success.http_status(), "200 OK");
        assert_eq!(CallbackPage::NotFound.http_status(), "404 Not Found");
        assert_eq!(CallbackPage::StateMismatch.http_status(), "400 Bad Request");
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
    fn device_authorization_defaults_interval_and_picks_best_link() {
        // Full body: complete link is preferred, interval honored.
        let full: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"WXYZ-ABCD",
                "verification_uri":"https://fjord.sh/device",
                "verification_uri_complete":"https://fjord.sh/device?user_code=WXYZ-ABCD",
                "expires_in":600,"interval":5}"#,
        )
        .unwrap();
        assert_eq!(full.user_code, "WXYZ-ABCD");
        assert_eq!(full.interval, 5);
        assert_eq!(
            full.best_verification_link(),
            "https://fjord.sh/device?user_code=WXYZ-ABCD"
        );

        // No `interval` → RFC 8628 default of 5; no complete link → bare URI.
        let minimal: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"AAAA-BBBB",
                "verification_uri":"https://fjord.sh/device","expires_in":600}"#,
        )
        .unwrap();
        assert_eq!(minimal.interval, 5);
        assert_eq!(minimal.best_verification_link(), "https://fjord.sh/device");
    }

    #[test]
    fn parse_device_error_extracts_code() {
        assert_eq!(
            parse_device_error(r#"{"error":"authorization_pending"}"#).as_deref(),
            Some("authorization_pending")
        );
        assert_eq!(parse_device_error("not json").as_deref(), None);
        assert_eq!(parse_device_error("{}").as_deref(), None);
    }

    #[test]
    fn render_qr_produces_a_block_grid_and_tolerates_huge_input() {
        let qr = render_qr("https://fjord.sh/device?user_code=WXYZ-ABCD")
            .expect("a short URL fits in a QR symbol");
        // Multi-line grid of Unicode half-blocks (the Dense1x2 renderer).
        assert!(qr.lines().count() > 5, "QR should be a multi-row grid");
        assert!(
            qr.contains('█') || qr.contains('▀') || qr.contains('▄') || qr.contains(' '),
            "QR uses half-block glyphs"
        );
        // Oversized data can't be encoded: render_qr degrades to None, never panics.
        assert!(render_qr(&"x".repeat(10_000)).is_none());
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
        async fn manual_flow_parses_paste_checks_state_and_exchanges() {
            // End-to-end at the module level (no stdin/socket): the exact steps
            // `--manual` runs after the user pastes the redirected URL — parse,
            // CSRF-check the state, then exchange the code with the fixed
            // registered redirect URI.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .and(body_string_contains("grant_type=authorization_code"))
                .and(body_string_contains("code=pasted-code"))
                .and(body_string_contains("code_verifier=the-verifier"))
                // The redirect URI in the exchange must be the fixed registered
                // callback (percent-encoded in the form body).
                .and(body_string_contains(
                    "redirect_uri=http%3A%2F%2F127.0.0.1%3A47017%2Fcallback",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc",
                    "refresh_token": "ref",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })))
                .expect(1)
                .mount(&server)
                .await;

            // The user pastes the full redirected URL from another device.
            let redirect_uri = default_redirect_uri();
            let pasted = format!("{redirect_uri}?code=pasted-code&state=the-state");
            let parsed = parse_pasted_callback(&pasted).unwrap();
            assert_eq!(parsed.code, "pasted-code");
            parsed.check_state("the-state").expect("state matches");

            let tokens = exchange_code(
                &server.uri(),
                "fj-cli",
                &parsed.code,
                &redirect_uri,
                "the-verifier",
            )
            .await
            .unwrap();
            assert_eq!(tokens.access_token, "acc");
            assert_eq!(tokens.refresh_token.as_deref(), Some("ref"));
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

        // ---- Device Authorization Grant (RFC 8628) ----

        #[tokio::test]
        async fn device_authorization_posts_client_and_scope_and_parses() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/device_authorization"))
                .and(header("content-type", "application/x-www-form-urlencoded"))
                .and(body_string_contains("client_id=fj-cli"))
                .and(body_string_contains("scope=openid"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "device_code": "the-device-code",
                    "user_code": "WXYZ-ABCD",
                    "verification_uri": "https://fjord.sh/device",
                    "verification_uri_complete": "https://fjord.sh/device?user_code=WXYZ-ABCD",
                    "expires_in": 600,
                    "interval": 5,
                })))
                .expect(1)
                .mount(&server)
                .await;
            let auth = request_device_authorization(&server.uri(), "fj-cli", Some("openid email"))
                .await
                .unwrap();
            assert_eq!(auth.device_code, "the-device-code");
            assert_eq!(auth.user_code, "WXYZ-ABCD");
            assert_eq!(auth.expires_in, 600);
            assert_eq!(auth.interval, 5);
        }

        #[tokio::test]
        async fn device_authorization_html_response_points_at_platform_url() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/device_authorization"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/html")
                        .set_body_string("<!DOCTYPE html><title>Sign in</title>"),
                )
                .expect(1)
                .mount(&server)
                .await;
            let err = request_device_authorization(&server.uri(), "fj-cli", Some("openid"))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("HTML page"), "got: {err}");
            assert!(err.contains("--platform-url"), "got: {err}");
        }

        /// One poll maps each RFC 8628 §3.5 outcome. A public client sends the
        /// device-code grant, the client id, and the device code; no secret.
        #[tokio::test]
        async fn device_token_poll_maps_each_outcome() {
            // 400 error bodies → the matching non-terminal / terminal states.
            for (err, want) in [
                ("authorization_pending", "Pending"),
                ("slow_down", "SlowDown"),
                ("access_denied", "Denied"),
                ("expired_token", "Expired"),
                ("invalid_grant", "Failed"),
            ] {
                let server = MockServer::start().await;
                Mock::given(method("POST"))
                    .and(path("/oauth/token"))
                    .and(body_string_contains(
                        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                    ))
                    .and(body_string_contains("client_id=fj-cli"))
                    .and(body_string_contains("device_code=dc"))
                    .respond_with(
                        ResponseTemplate::new(400)
                            .set_body_json(serde_json::json!({ "error": err })),
                    )
                    .mount(&server)
                    .await;
                let poll = poll_device_token(&server.uri(), "fj-cli", "dc")
                    .await
                    .unwrap();
                let got = match poll {
                    DevicePoll::Pending => "Pending",
                    DevicePoll::SlowDown => "SlowDown",
                    DevicePoll::Denied => "Denied",
                    DevicePoll::Expired => "Expired",
                    DevicePoll::Failed(_) => "Failed",
                    DevicePoll::Approved(_) => "Approved",
                };
                assert_eq!(got, want, "error {err} should map to {want}");

                // No client_secret ever leaves a public client.
                let reqs = server.received_requests().await.unwrap();
                let body = String::from_utf8(reqs[0].body.clone()).unwrap();
                assert!(!body.contains("client_secret"), "no secret: {body}");
            }

            // 200 → Approved with the parsed tokens.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc",
                    "id_token": "idt",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "ref",
                })))
                .mount(&server)
                .await;
            match poll_device_token(&server.uri(), "fj-cli", "dc")
                .await
                .unwrap()
            {
                DevicePoll::Approved(t) => {
                    assert_eq!(t.access_token, "acc");
                    assert_eq!(t.refresh_token.as_deref(), Some("ref"));
                }
                other => panic!("want Approved, got {other:?}"),
            }
        }

        /// A non-OAuth failure (a proxy 502 with an HTML body, no `error`) is a
        /// hard error so the poll loop aborts instead of hot-looping.
        #[tokio::test]
        async fn device_token_poll_surfaces_non_oauth_failure() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"),
                )
                .mount(&server)
                .await;
            let err = poll_device_token(&server.uri(), "fj-cli", "dc")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("502"), "want status in error: {err}");
        }

        /// Happy path through the loop: `authorization_pending` then `200`.
        #[tokio::test]
        async fn device_poll_loop_pending_then_approved() {
            use std::time::{Duration, Instant};
            let server = MockServer::start().await;
            // First poll: pending.
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .and(body_string_contains("grant_type=urn%3Aietf"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "authorization_pending" })),
                )
                .up_to_n_times(1)
                .mount(&server)
                .await;
            // Then: approved.
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc",
                    "refresh_token": "ref",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })))
                .mount(&server)
                .await;
            let tokens = poll_for_device_token(
                &server.uri(),
                "fj-cli",
                "dc",
                Duration::from_millis(10),
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap();
            assert_eq!(tokens.access_token, "acc");
            assert_eq!(tokens.refresh_token.as_deref(), Some("ref"));
        }

        /// `slow_down` is honored (loop keeps going after a back-off) and then
        /// approves; the loop doesn't treat it as terminal.
        #[tokio::test]
        async fn device_poll_loop_honors_slow_down_then_approves() {
            use std::time::{Duration, Instant};
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "slow_down" })),
                )
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "acc2",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })))
                .mount(&server)
                .await;
            let tokens = poll_for_device_token(
                &server.uri(),
                "fj-cli",
                "dc",
                Duration::from_millis(5),
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap();
            assert_eq!(tokens.access_token, "acc2");
        }

        /// `access_denied` aborts the loop with a clear message.
        #[tokio::test]
        async fn device_poll_loop_aborts_on_denied() {
            use std::time::{Duration, Instant};
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "access_denied" })),
                )
                .mount(&server)
                .await;
            let err = poll_for_device_token(
                &server.uri(),
                "fj-cli",
                "dc",
                Duration::from_millis(5),
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(err.contains("denied"), "want denial message: {err}");
        }

        /// `expired_token` from the server aborts with the run-again message.
        #[tokio::test]
        async fn device_poll_loop_aborts_on_expired() {
            use std::time::{Duration, Instant};
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "expired_token" })),
                )
                .mount(&server)
                .await;
            let err = poll_for_device_token(
                &server.uri(),
                "fj-cli",
                "dc",
                Duration::from_millis(5),
                Instant::now() + Duration::from_secs(30),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(err.contains("expired"), "want expiry message: {err}");
        }

        /// The client-side deadline stops a loop the server never resolves,
        /// even while it keeps returning `authorization_pending`.
        #[tokio::test]
        async fn device_poll_loop_stops_at_deadline() {
            use std::time::{Duration, Instant};
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "authorization_pending" })),
                )
                .mount(&server)
                .await;
            let err = poll_for_device_token(
                &server.uri(),
                "fj-cli",
                "dc",
                Duration::from_millis(10),
                Instant::now() + Duration::from_millis(60),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(err.contains("expired"), "want deadline message: {err}");
        }
    }
}
