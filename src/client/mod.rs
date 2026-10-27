pub mod capability;
pub mod error;
#[cfg(test)]
mod integration_tests;
pub mod pagination;
pub mod proxy;
mod resolve;

use resolve::build_base_url;
pub use resolve::{proxy_credential_for_host, resolve, web_base_url, ResolvedHost};

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

static DEBUG: AtomicBool = AtomicBool::new(false);

/// Toggle request logging at runtime. Set by `--debug` / `FJ_DEBUG`.
pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

pub(crate) fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

use anyhow::{anyhow, Context, Result};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, LOCATION, PROXY_AUTHORIZATION,
    USER_AGENT,
};
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::config::hosts::AuthKind;
use crate::fjord;

pub use capability::{Capability, Flavor, ServerInfo, Support};
pub use error::ApiError;
pub use pagination::Page;
pub use proxy::{PatTransport, ProxyCredential, ProxyMode};

/// Which HTTP `Authorization` shape a `Client` uses on outgoing requests.
/// `Pat` is the historical Forgejo PAT scheme (`Authorization: token <pat>`).
/// `FjordBearer` is the OAuth-style bearer fjord-platform mints from
/// `/api/v1/sign-in` and the gateway then translates per-instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    Pat,
    FjordBearer,
}

/// A test seam for the silent-refresh path: a closure that stands in for the
/// real `fjord::oidc` refresh so `Client`'s 401-refresh-retry wiring can be
/// exercised against a mock server without touching the keychain-backed token
/// store. Returns `Ok(Some(fresh_bearer))` to simulate a successful refresh,
/// `Ok(None)` for "no usable refresh token" (surface the 401).
#[cfg(test)]
type RefreshHook = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>>
        + Send
        + Sync,
>;

/// Wrap a synchronous closure as a [`RefreshHook`] for tests, so a test can
/// describe the refresh outcome (`Ok(Some(fresh))` / `Ok(None)`) and count
/// invocations without writing the boxed-future boilerplate by hand.
#[cfg(test)]
pub(crate) fn sync_refresh_hook<F>(f: F) -> RefreshHook
where
    F: Fn() -> Result<Option<String>> + Send + Sync + 'static,
{
    let f = Arc::new(f);
    Arc::new(move || {
        let f = Arc::clone(&f);
        Box::pin(async move { f() })
    })
}

/// Convenience handle for talking to a single Forgejo host (or, in
/// Fjord-Account mode, to a per-instance gateway endpoint that fronts one).
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    #[allow(dead_code)]
    host: String,
    base: Url,
    /// The outgoing bearer/PAT. Behind a lock (shared across clones) so the
    /// silent-refresh path can swap in a freshly minted Fjord access token on a
    /// 401 and have every later request, and every clone, pick it up.
    token: Arc<RwLock<Option<String>>>,
    auth_scheme: AuthScheme,
    /// The fjord-platform origin for a Fjord-Account host, used to drive the
    /// silent OIDC refresh on a 401. `None` for PAT hosts (their tokens are not
    /// refreshable) and for Fjord hosts we lack platform config for.
    platform_url: Option<String>,
    /// Credential for a reverse proxy in front of this host, if the user
    /// configured one. Never sent until the proxy asks for it.
    proxy: Option<Arc<ProxyCredential>>,
    /// Which header the proxy asked for (a `ProxyMode` as `u8`). Shared across
    /// clones so one challenge teaches every later request in the process.
    proxy_mode: Arc<AtomicU8>,
    /// Plain client used for the one credential-free hop when the strict
    /// redirect policy (active only with a proxy credential) stops a
    /// cross-origin redirect.
    redirect_http: Option<reqwest::Client>,
    /// Flavor and version learned by the lazy probe, once per process.
    server: Arc<RwLock<Option<ServerInfo>>>,
    /// What hosts.toml recorded at login or refresh. Used only when the live
    /// probe cannot identify the server.
    server_seed: Option<ServerInfo>,
    #[cfg(test)]
    refresh_hook: Option<RefreshHook>,
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str()
            .zip(b.host_str())
            .is_some_and(|(x, y)| x.eq_ignore_ascii_case(y))
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Build the HTTP client. `strict_redirects` is set when a proxy credential is
/// configured: reqwest strips `Authorization` on a cross-origin redirect but
/// knows nothing about `X-Forge-Authorization`, so with a proxy credential we
/// stop at the origin boundary and make that hop ourselves with no credential.
fn build_http(strict_redirects: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(default_user_agent())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60));
    if strict_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
            let stays = attempt
                .previous()
                .last()
                .is_some_and(|prev| same_origin(prev, attempt.url()));
            if !stays {
                attempt.stop()
            } else if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }));
    }
    builder.build().context("building HTTP client")
}

impl Client {
    pub fn new(resolved: ResolvedHost) -> Result<Self> {
        let (base, auth_scheme, platform_url) = match resolved.host.auth_kind {
            AuthKind::Pat => (
                build_base_url(&resolved.name, &resolved.host)?,
                AuthScheme::Pat,
                None,
            ),
            AuthKind::FjordAccount => {
                let fa = resolved.host.fjord_account.as_ref().ok_or_else(|| {
                    anyhow!(
                        "host '{}' is Fjord-Account but missing fjord_account config",
                        resolved.name
                    )
                })?;
                let instance_id = fa.default_instance_id.as_deref().ok_or_else(|| {
                    anyhow!(
                        "no default Fjord instance selected. \
                         Run `fj instances list` and then `fj instances use <id>`."
                    )
                })?;
                (
                    fjord::gateway_base_url(&fa.platform_url, instance_id)?,
                    AuthScheme::FjordBearer,
                    // Remember where to refresh: the platform origin owns the
                    // OIDC token endpoint and the stored refresh token.
                    Some(fa.platform_url.clone()),
                )
            }
        };
        // A proxy credential only ever applies to a direct PAT host.
        let proxy = resolved
            .proxy
            .filter(|_| auth_scheme == AuthScheme::Pat)
            .map(Arc::new);
        let http = build_http(proxy.is_some())?;
        let redirect_http = match proxy {
            Some(_) => Some(build_http(false)?),
            None => None,
        };
        let server_seed = resolved
            .host
            .flavor
            .as_deref()
            .map(Flavor::parse)
            .filter(|flavor| *flavor != Flavor::Unidentified)
            .map(|flavor| {
                ServerInfo::new(
                    flavor,
                    resolved.host.server_version.as_deref().unwrap_or(""),
                )
            });
        Ok(Self {
            http,
            host: resolved.name,
            base,
            token: Arc::new(RwLock::new(resolved.token)),
            auth_scheme,
            platform_url,
            proxy,
            proxy_mode: Arc::new(AtomicU8::new(ProxyMode::Direct.to_u8())),
            redirect_http,
            server: Arc::new(RwLock::new(None)),
            server_seed,
            #[cfg(test)]
            refresh_hook: None,
        })
    }

    /// Construct a client from an explicit host flag, loading config + token.
    pub fn connect(host_flag: Option<&str>) -> Result<Self> {
        Self::new(resolve(host_flag)?)
    }

    /// Construct a client pointing at an arbitrary base URL. Used for tests
    /// against a local mock server. `base_url` must end with `/`. Defaults to
    /// PAT-style auth; use `for_base_url_with_scheme` to override.
    #[cfg(test)]
    pub fn for_base_url(base_url: Url, token: String) -> Result<Self> {
        Self::for_base_url_with_scheme(base_url, Some(token), AuthScheme::Pat)
    }

    #[cfg(test)]
    pub fn anonymous_for_base_url(base_url: Url) -> Result<Self> {
        Self::for_base_url_with_scheme(base_url, None, AuthScheme::Pat)
    }

    /// Like `for_base_url` but lets tests pick the auth scheme.
    #[cfg(test)]
    pub fn for_base_url_with_scheme(
        base_url: Url,
        token: Option<String>,
        auth_scheme: AuthScheme,
    ) -> Result<Self> {
        let host = base_url.host_str().map(str::to_string).unwrap_or_default();
        let http = build_http(false)?;
        Ok(Self {
            http,
            host,
            base: base_url,
            token: Arc::new(RwLock::new(token)),
            auth_scheme,
            platform_url: None,
            proxy: None,
            proxy_mode: Arc::new(AtomicU8::new(ProxyMode::Direct.to_u8())),
            redirect_http: None,
            server: Arc::new(RwLock::new(None)),
            server_seed: None,
            refresh_hook: None,
        })
    }

    /// Attach a reverse-proxy credential to a test client, with the same
    /// strict redirect policy `Client::new` applies.
    #[cfg(test)]
    pub fn with_proxy(mut self, cred: ProxyCredential) -> Result<Self> {
        self.http = build_http(true)?;
        self.redirect_http = Some(build_http(false)?);
        self.proxy = Some(Arc::new(cred));
        Ok(self)
    }

    /// Pretend the flavor probe already ran, so a test can pin the server
    /// without mocking the version routes.
    #[cfg(test)]
    pub fn with_server_info(self, info: ServerInfo) -> Self {
        *self
            .server
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(info);
        self
    }

    /// A `FjordBearer` client whose silent-refresh path is driven by a supplied
    /// `refresh_hook` instead of the real keychain-backed `fjord::oidc` flow, so
    /// the 401-refresh-retry wiring is testable against a mock server.
    #[cfg(test)]
    pub fn for_base_url_fjord_refresh(
        base_url: Url,
        token: String,
        refresh_hook: RefreshHook,
    ) -> Result<Self> {
        let mut client =
            Self::for_base_url_with_scheme(base_url, Some(token), AuthScheme::FjordBearer)?;
        client.refresh_hook = Some(refresh_hook);
        Ok(client)
    }

    #[allow(dead_code)]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[allow(dead_code)]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Underlying `reqwest::Client` for code paths that need to bypass our
    /// JSON-shaped helpers (e.g. multipart uploads for release assets).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// A snapshot of the current bearer/PAT. Cloned out from behind the lock so
    /// callers never hold the guard across an await.
    fn current_token(&self) -> Option<String> {
        self.token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Swap in a freshly minted bearer after a silent refresh. Visible to every
    /// later request on this client and all its clones (shared `Arc`).
    fn set_token(&self, token: String) {
        *self
            .token
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(token);
    }

    /// Which `Authorization` shape this client emits. Used by call sites
    /// (e.g. release asset upload) that bypass `request_with_headers` and
    /// need to build their own header set.
    #[allow(dead_code)]
    pub fn auth_scheme(&self) -> AuthScheme {
        self.auth_scheme
    }

    /// Build the `Authorization` header value matching the client's auth
    /// scheme: `token <pat>` for PAT-mode hosts, `Bearer <jwt>` for
    /// Fjord-Account-mode hosts. Returns an `HeaderValue` so the caller
    /// can plug it into any `HeaderMap` directly.
    pub fn auth_header_value(&self) -> Result<Option<HeaderValue>> {
        let Some(token) = self.current_token() else {
            return Ok(None);
        };
        let raw = match self.auth_scheme {
            AuthScheme::Pat => format!("token {token}"),
            AuthScheme::FjordBearer => format!("Bearer {token}"),
        };
        HeaderValue::from_str(&raw).map(Some).map_err(|_| {
            anyhow!(
                "stored credential for host '{}' contains characters that can't appear in an HTTP header. Re-run `fj auth login` with a clean token.",
                self.host
            )
        })
    }

    /// The same client with no forge token attached. For the few unversioned
    /// web routes fj still reads on servers without an API equivalent: those
    /// routes authenticate with a browser session cookie, a token does nothing
    /// there, so none is sent. A configured proxy credential is kept, because
    /// it belongs to the proxy and not to the forge.
    pub fn without_credentials(&self) -> Client {
        let mut anonymous = self.clone();
        anonymous.token = Arc::new(RwLock::new(None));
        anonymous
    }

    /// Flavor and version of the server, probed at most once per process (two
    /// GETs at worst). When the live probe cannot identify the server, the
    /// value recorded in hosts.toml at login or refresh is used instead.
    pub async fn server_info(&self) -> ServerInfo {
        let cached = self
            .server
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(info) = cached {
            return info;
        }
        let probed = capability::probe(self).await;
        let info = match (&probed.flavor, &self.server_seed) {
            (Flavor::Unidentified, Some(seed)) => seed.clone(),
            _ => probed,
        };
        *self
            .server
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(info.clone());
        info
    }

    fn proxy_mode(&self) -> ProxyMode {
        ProxyMode::from_u8(self.proxy_mode.load(Ordering::Relaxed))
    }

    /// Headers for the multipart upload call sites that build their own
    /// request (`release upload`, issue attachments). Carries the forge token
    /// and, once a proxy challenge has been seen in this process, the proxy
    /// credential in the header the proxy asked for.
    pub fn upload_auth_headers(&self, what: &str) -> Result<HeaderMap> {
        let auth = self
            .auth_header_value()?
            .ok_or_else(|| anyhow!("{what} requires an authenticated host"))?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);
        if let Some(cred) = self.proxy.as_deref() {
            if cred.pat_transport == PatTransport::Query && self.proxy_mode() == ProxyMode::Basic {
                return Err(anyhow!(
                    "{what} is not supported through a reverse proxy with the `query` token \
                     transport: the token would have to ride in the upload URL. Switch to the \
                     header transport with `fj auth proxy --host {} --pat-transport header`.",
                    self.host
                ));
            }
            self.apply_proxy(&mut headers, cred, self.proxy_mode());
        }
        Ok(headers)
    }

    /// Rewrite `headers` for the proxy mode in force. Returns the forge token
    /// to append as a query pair when the `query` transport is selected.
    fn apply_proxy(
        &self,
        headers: &mut HeaderMap,
        cred: &ProxyCredential,
        mode: ProxyMode,
    ) -> Option<String> {
        if mode == ProxyMode::Direct || !proxy::transport_is_safe(&self.base) {
            return None;
        }
        let basic = cred.basic_header()?;
        match mode {
            ProxyMode::Direct => None,
            ProxyMode::ProxyAuthorization => {
                headers.insert(PROXY_AUTHORIZATION, basic);
                None
            }
            ProxyMode::Basic => {
                // The proxy owns `Authorization`, so the forge token moves.
                let pat = headers.remove(AUTHORIZATION);
                headers.insert(AUTHORIZATION, basic);
                match cred.pat_transport {
                    PatTransport::Header => {
                        if let Some(mut pat) = pat {
                            pat.set_sensitive(true);
                            headers
                                .insert(HeaderName::from_static(proxy::FORWARDED_AUTH_HEADER), pat);
                        }
                        None
                    }
                    PatTransport::Query => self.current_token(),
                }
            }
        }
    }

    /// Whether `url` is the same origin (scheme + host + port) as the
    /// configured API base. The auth token is only ever attached to requests
    /// that pass this check, so an absolute URL handed to `fj api` (or any
    /// redirected target) can't exfiltrate the bearer token to another host
    /// or downgrade it onto cleartext. Real bases are always `https` (see
    /// `build_base_url`), so requiring the scheme to match the base also
    /// rejects `fj api http://<same-host>/…`.
    fn is_trusted_url(&self, url: &Url) -> bool {
        same_origin(url, &self.base)
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(auth) = self.auth_header_value()? {
            headers.insert(AUTHORIZATION, auth);
        }
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(default_user_agent_static()),
        );
        Ok(headers)
    }

    /// Build an absolute URL for an API path. Accepts any of: a full URL
    /// (`https://…`), an absolute path that already includes the API base
    /// (`/api/v1/user`), or a path relative to the API base (`user`, `/user`).
    /// All forms anchor at `<host>/api/v1/` unless an explicit URL is given.
    pub fn url(&self, path: &str) -> Result<Url> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Ok(Url::parse(path)?);
        }
        let trimmed = path
            .strip_prefix("/api/v1/")
            .or_else(|| path.strip_prefix("api/v1/"))
            .or_else(|| path.strip_prefix('/'))
            .unwrap_or(path);
        Ok(self.base.join(trimmed)?)
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
    ) -> Result<Response> {
        self.request_with_headers(method, path, query, body, &HeaderMap::new())
            .await
    }

    /// Like `request` but merges `extra` headers in (they override defaults).
    ///
    /// The request is constructed once and cloned per retry attempt via
    /// `reqwest::Request::try_clone`, which avoids re-allocating headers and
    /// re-parsing the URL on each attempt.
    pub async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
        extra: &HeaderMap,
    ) -> Result<Response> {
        let url = self.url(path)?;
        let trusted = self.is_trusted_url(&url);
        if !trusted {
            // Absolute URL pointing off-host or not over TLS (only reachable
            // via `fj api <url>`): never leak the token there.
            eprintln!(
                "warning: not sending your {} token to {} (only same-host HTTPS requests are authenticated)",
                self.host,
                url.host_str().unwrap_or("?")
            );
        }

        if debug_enabled() {
            log_request(&method, &url, query, body);
        }

        // A reverse proxy only ever fronts a direct PAT host, and its
        // credential is only ever sent same-origin.
        let proxy_capable = trusted && self.auth_scheme == AuthScheme::Pat;
        let mut mode = if proxy_capable && self.proxy.is_some() {
            self.proxy_mode()
        } else {
            ProxyMode::Direct
        };
        let mut res = self
            .send_once(&method, &url, query, body, extra, trusted, mode)
            .await?;

        if proxy_capable {
            if let Some(wanted) = proxy::challenge(res.status(), res.headers()) {
                let status = res.status();
                if self.proxy.is_none() {
                    return Err(proxy::missing_credential_error(&self.host, status));
                }
                if !proxy::transport_is_safe(&self.base) {
                    return Err(proxy::unsafe_transport_error(&self.host));
                }
                if wanted == mode {
                    // We already sent the credential in the header it asked for.
                    return Err(proxy::rejected_credential_error(&self.host, status));
                }
                // The proxy answered this request itself, so the forge never
                // saw it. Replaying it once with the credential cannot apply
                // anything twice, which is why this is safe even for the POST
                // and PATCH requests the transient-failure retry never repeats.
                self.proxy_mode.store(wanted.to_u8(), Ordering::Relaxed);
                mode = wanted;
                if debug_enabled() {
                    eprintln!(
                        "← {status} {}: proxy challenge, retrying once with the proxy credential",
                        proxy::redact_url(&url)
                    );
                }
                res = self
                    .send_once(&method, &url, query, body, extra, trusted, mode)
                    .await?;
                if proxy::challenge(res.status(), res.headers()).is_some() {
                    return Err(proxy::rejected_credential_error(&self.host, res.status()));
                }
            }
            if mode == ProxyMode::Basic && res.status() == StatusCode::UNAUTHORIZED {
                let transport = self
                    .proxy
                    .as_deref()
                    .map(|cred| cred.pat_transport)
                    .unwrap_or_default();
                return Err(proxy::forge_rejected_error(&self.host, transport));
            }
        }

        // Transparent Fjord-Account session refresh. On a 401 for a host we
        // signed the request to with a Fjord bearer, silently renew the access
        // token via the stored refresh token and retry the request exactly
        // ONCE. PAT hosts, off-host requests, and the no-refresh-token case fall
        // through with the original 401 (the user re-runs `fj auth login`).
        // Bounded to a single refresh+retry, so a persistently-401ing endpoint
        // can never loop.
        if trusted
            && self.auth_scheme == AuthScheme::FjordBearer
            && res.status() == StatusCode::UNAUTHORIZED
        {
            if let Some(fresh) = self.refresh_bearer().await? {
                self.set_token(fresh);
                if debug_enabled() {
                    eprintln!(
                        "← 401 {}: refreshed Fjord session, retrying once",
                        proxy::redact_url(&url)
                    );
                }
                return self
                    .send_once(&method, &url, query, body, extra, trusted, mode)
                    .await;
            }
        }
        Ok(res)
    }

    /// Send one request with the current bearer, applying the transient-failure
    /// retry policy (5xx / 429 / transport). Does NOT handle 401 refresh: that
    /// lives one level up in [`request_with_headers`] so it happens at most once
    /// per logical request. The `Authorization` header is (re)built from the
    /// current token on every call, so a post-refresh retry uses the new token.
    #[allow(clippy::too_many_arguments)]
    async fn send_once(
        &self,
        method: &Method,
        url: &Url,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
        extra: &HeaderMap,
        trusted: bool,
        mode: ProxyMode,
    ) -> Result<Response> {
        let mut headers = self.auth_headers()?;
        let mut query_token: Option<String> = None;
        if !trusted {
            headers.remove(AUTHORIZATION);
        } else if let Some(cred) = self.proxy.as_deref() {
            query_token = self.apply_proxy(&mut headers, cred, mode);
        }
        for (k, v) in extra.iter() {
            headers.insert(k.clone(), v.clone());
        }

        let retries = if is_idempotent(method) { 3 } else { 1 };

        // Build the request once. `Request::try_clone` (used per retry below)
        // succeeds for any request whose body is `None`, `Bytes`, or `Text`.
        // We only ever set a JSON body, which lives in memory as bytes, so the
        // clone is always successful.
        let mut builder = self
            .http
            .request(method.clone(), url.clone())
            .headers(headers)
            .query(query);
        if let Some(token) = &query_token {
            // Opt-in `query` transport only. Every place this URL could be
            // printed goes through `proxy::redact_url`.
            builder = builder.query(&[("token", token.as_str())]);
        }
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let prepared = builder.build().context("building request")?;

        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..retries {
            let req = match prepared.try_clone() {
                Some(c) => c,
                None => {
                    return Err(anyhow!(
                        "internal: request body could not be cloned for retry"
                    ));
                }
            };
            match self.http.execute(req).await {
                Ok(res) => {
                    let status = res.status();
                    // 429 (rate limit): always honor Retry-After when retryable.
                    if status == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < retries {
                        let wait = parse_retry_after(&res).unwrap_or_else(|| {
                            std::time::Duration::from_millis(200 * (1u64 << attempt))
                        });
                        let wait = wait.min(std::time::Duration::from_secs(30));
                        if debug_enabled() {
                            eprintln!(
                                "← 429 {} (rate-limited, sleeping {:?})",
                                proxy::redact_url(res.url()),
                                wait
                            );
                        }
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    if status.is_server_error() && attempt + 1 < retries {
                        if debug_enabled() {
                            eprintln!(
                                "← {status} {} (retrying after backoff, attempt {}/{})",
                                proxy::redact_url(res.url()),
                                attempt + 1,
                                retries
                            );
                        }
                        backoff(attempt).await;
                        continue;
                    }
                    if debug_enabled() {
                        eprintln!("← {status} {}", proxy::redact_url(res.url()));
                    }
                    return self.follow_cross_origin(method, res).await;
                }
                Err(e) => {
                    // reqwest prints the request URL in its error. With the
                    // query transport that URL carries the token, so drop it.
                    let e = if query_token.is_some() {
                        e.without_url()
                    } else {
                        e
                    };
                    if attempt + 1 < retries {
                        if debug_enabled() {
                            eprintln!(
                                "transport error: {e}; retrying ({}/{})",
                                attempt + 1,
                                retries
                            );
                        }
                        backoff(attempt).await;
                        last_err = Some(anyhow::Error::new(e));
                        continue;
                    }
                    return Err(anyhow::Error::new(e).context("sending HTTP request"));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("retries exhausted")))
    }

    /// With a proxy credential configured the HTTP client refuses to follow a
    /// redirect to another origin, because `X-Forge-Authorization` would ride
    /// along. Make that one hop here with no credential at all, which is what
    /// reqwest's default policy amounts to for `Authorization` anyway.
    async fn follow_cross_origin(&self, method: &Method, res: Response) -> Result<Response> {
        let Some(plain) = &self.redirect_http else {
            return Ok(res);
        };
        if !res.status().is_redirection() || !matches!(method.as_str(), "GET" | "HEAD") {
            return Ok(res);
        }
        let Some(target) = res
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|loc| res.url().join(loc).ok())
        else {
            return Ok(res);
        };
        if same_origin(&target, res.url()) {
            return Ok(res);
        }
        if debug_enabled() {
            eprintln!(
                "→ {method} {} (cross-origin redirect, no credentials)",
                proxy::redact_url(&target)
            );
        }
        plain
            .request(method.clone(), target)
            .send()
            .await
            .map_err(|e| anyhow::Error::new(e.without_url()).context("following redirect"))
    }

    /// Attempt a silent session refresh for a Fjord-Account host. Returns
    /// `Ok(Some(fresh_bearer))` when a stored refresh token belonging to the
    /// current account is exchanged for a new access token, and `Ok(None)` when
    /// there is no usable refresh token (a PAT host, no stored token, or one
    /// bound to a different account) so the caller surfaces the original 401.
    /// Reuses [`fjord::oidc::valid_refresh_token`] for the identity-bound gate
    /// and [`fjord::oidc::refresh_session`] for the exchange-and-persist.
    async fn refresh_bearer(&self) -> Result<Option<String>> {
        // Test seam: drive the wiring from a mock instead of the keychain.
        #[cfg(test)]
        if let Some(hook) = &self.refresh_hook {
            return hook().await;
        }
        let Some(platform_url) = self.platform_url.as_deref() else {
            return Ok(None);
        };
        let Some(current) = self.current_token() else {
            return Ok(None);
        };
        if fjord::oidc::valid_refresh_token(platform_url, &current)?.is_none() {
            return Ok(None);
        }
        let fresh = fjord::oidc::refresh_session(platform_url)
            .await
            .context("refreshing the expired Fjord Account session")?;
        Ok(Some(fresh))
    }

    /// Issue a request and decode a JSON body, mapping non-2xx to a typed
    /// `ApiError`.
    pub async fn json<T, B>(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&B>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let body_value = match body {
            Some(b) => Some(serde_json::to_value(b).context("serializing request body")?),
            None => None,
        };
        let res = self
            .request(method, path, query, body_value.as_ref())
            .await?;
        ensure_success(res)
            .await?
            .json()
            .await
            .context("decoding JSON response")
    }

    /// Issue a request that returns no body we need to decode (merge, ready,
    /// update-branch, …), mapping non-2xx to a typed [`ApiError`]. Unlike
    /// `reqwest::Response::error_for_status`, this surfaces the parsed Forgejo
    /// `message` (e.g. why a merge is not allowed) instead of a bare status.
    pub async fn send<B>(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&B>,
    ) -> Result<Response>
    where
        B: Serialize + ?Sized,
    {
        let body_value = match body {
            Some(b) => Some(serde_json::to_value(b).context("serializing request body")?),
            None => None,
        };
        let res = self
            .request(method, path, query, body_value.as_ref())
            .await?;
        ensure_success(res).await
    }

    /// GET that returns a single page along with pagination metadata.
    pub async fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<Page<T>> {
        let res = self.request(Method::GET, path, query, None).await?;
        let res = ensure_success(res).await?;
        let headers = res.headers().clone();
        let text = res.text().await.context("reading list response body")?;
        // Forgejo returns `null` for some empty list endpoints. Normalize.
        let items: Vec<T> = decode_list(&text, path, query)?;
        Ok(Page::from_headers(items, &headers))
    }

    /// GET that follows `Link: rel=next` until either `total_limit` items have
    /// been collected or there are no more pages. Use this when the caller's
    /// requested limit exceeds Forgejo's per-page cap (50).
    #[cfg(test)]
    pub async fn get_all<T: DeserializeOwned>(
        &self,
        path: &str,
        base_query: &[(String, String)],
        total_limit: usize,
    ) -> Result<Vec<T>> {
        Ok(self
            .get_all_page(path, base_query, Some(total_limit))
            .await?
            .items)
    }

    /// GET that follows `Link: rel=next` and returns the collected items along
    /// with pagination metadata from the first page, including `X-Total-Count`.
    pub async fn get_all_page<T: DeserializeOwned>(
        &self,
        path: &str,
        base_query: &[(String, String)],
        total_limit: Option<usize>,
    ) -> Result<Page<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut current_path: String = path.to_string();
        let mut current_query: Vec<(String, String)> = base_query.to_vec();
        let mut first_page: Option<Page<()>> = None;
        // Make sure per-page is set to the API's max so we don't make extra
        // round-trips.
        if !current_query.iter().any(|(k, _)| k == "limit") {
            current_query.push(("limit".into(), "50".into()));
        } else {
            for (k, v) in current_query.iter_mut() {
                if k == "limit" {
                    *v = "50".to_string();
                }
            }
        }

        loop {
            let res = self
                .request(Method::GET, &current_path, &current_query, None)
                .await?;
            let res = ensure_success(res).await?;
            let headers = res.headers().clone();
            let text = res.text().await.context("reading list response body")?;
            if first_page.is_none() {
                first_page = Some(Page::from_headers(Vec::new(), &headers));
            }
            // Some Forgejo list endpoints answer an empty collection with a
            // `null` (or empty) body rather than `[]`. Normalize it exactly as
            // `get_page` does, so a large-`--limit` walk doesn't error where a
            // single page would have succeeded.
            let mut items: Vec<T> = decode_list(&text, &current_path, &current_query)?;
            if items.is_empty() {
                break;
            }
            if let Some(total_limit) = total_limit {
                let need = total_limit.saturating_sub(out.len());
                if items.len() > need {
                    items.truncate(need);
                }
            }
            out.append(&mut items);
            if total_limit.is_some_and(|limit| out.len() >= limit) {
                break;
            }
            // Find Link: rel=next.
            let Some(next) = headers
                .get(reqwest::header::LINK)
                .and_then(|l| l.to_str().ok())
                .map(pagination::parse_link_header)
                .and_then(|links| {
                    links
                        .into_iter()
                        .find_map(|(url, rel)| (rel == "next").then_some(url))
                })
            else {
                break;
            };
            let parsed = Url::parse(&next)?;
            current_path = parsed.path().to_string();
            current_query = parsed
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
        }
        let meta = first_page.unwrap_or_else(|| Page::single(Vec::new()));
        Ok(Page {
            items: out,
            next: meta.next,
            prev: meta.prev,
            last: meta.last,
            first: meta.first,
            total: meta.total,
        })
    }
}

/// Decode a Forgejo list-response body, normalizing the `null`/empty bodies
/// some endpoints return for an empty collection into an empty `Vec`. Shared
/// by `get_page` and `get_all` so both tolerate the same shapes.
fn decode_list<T: DeserializeOwned>(
    text: &str,
    path: &str,
    query: &[(String, String)],
) -> Result<Vec<T>> {
    let t = text.trim();
    if t.is_empty() || t == "null" {
        Ok(Vec::new())
    } else {
        let mut de = serde_json::Deserializer::from_str(t);
        serde_path_to_error::deserialize(&mut de).map_err(|err| {
            anyhow!(
                "decoding JSON list response from GET {} failed at field {}: {}",
                endpoint_for_error(path, query),
                err.path(),
                err.inner()
            )
        })
    }
}

fn endpoint_for_error(path: &str, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let params = query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{params}")
}

fn is_idempotent(m: &Method) -> bool {
    matches!(m.as_str(), "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE")
}

/// Parse a Retry-After response header. Supports both delta-seconds (the
/// common case) and HTTP-date (which we don't bother parsing — we just bail
/// out and let the caller use exponential backoff instead).
fn parse_retry_after(res: &Response) -> Option<std::time::Duration> {
    let v = res.headers().get(reqwest::header::RETRY_AFTER)?;
    let s = v.to_str().ok()?.trim();
    let secs: u64 = s.parse().ok()?;
    Some(std::time::Duration::from_secs(secs))
}

async fn backoff(attempt: u32) {
    let base_ms: u64 = 200;
    let delay = base_ms * (1u64 << attempt);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
}

/// Emit a `--debug` log line for an outgoing request. Pulled out of the hot
/// path so the formatting code is gated behind the cold `debug_enabled` check.
#[cold]
fn log_request(
    method: &Method,
    url: &Url,
    query: &[(String, String)],
    body: Option<&serde_json::Value>,
) {
    for line in request_log_lines(method, url, query, body) {
        eprintln!("{line}");
    }
}

/// The lines `--debug` prints for an outgoing request. Pure so the redaction
/// is testable: no header is ever printed (so neither the forge token nor the
/// proxy credential can appear), and a `token` / `access_token` query value is
/// replaced whether it sits in the URL or in the query pairs.
fn request_log_lines(
    method: &Method,
    url: &Url,
    query: &[(String, String)],
    body: Option<&serde_json::Value>,
) -> Vec<String> {
    let q = if query.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = query
            .iter()
            .map(|(k, v)| {
                if proxy::is_secret_query_key(k) {
                    format!("{k}=[redacted]")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        format!("?{}", pairs.join("&"))
    };
    let mut lines = vec![format!("→ {method} {}{q}", proxy::redact_url(url))];
    if let Some(line) = body.and_then(|b| debug_body_preview(url.path(), b)) {
        lines.push(format!("  body: {line}"));
    }
    lines
}

/// What `--debug` may print for a request body: a truncated preview, or a
/// fixed marker when the endpoint carries a credential or an Actions value.
fn debug_body_preview(path: &str, body: &serde_json::Value) -> Option<String> {
    if is_sensitive_path(path) {
        return Some("[redacted: sensitive endpoint]".to_string());
    }
    serde_json::to_string(body)
        .ok()
        .map(|text| truncate_preview(&text))
}

/// Cap a debug body preview at 200 characters, appending an ellipsis when
/// truncated. Counts by `char`, not byte: slicing `&text[..200]` would panic
/// when byte 200 lands inside a multi-byte UTF-8 sequence (e.g. an emoji or
/// accented character in an issue title straddling the cut).
fn truncate_preview(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(200).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Endpoints whose request bodies carry credentials we must never echo to the
/// debug log (`fj secret set`, `variable set`, token/key management, …).
fn is_sensitive_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    // `/admin/users` carries a new account's initial password.
    [
        "secret",
        "variable",
        "token",
        "password",
        "/keys",
        "/admin/users",
    ]
    .iter()
    .any(|needle| p.contains(needle))
}

/// Upper bound on a single response body we'll buffer into memory. A
/// misbehaving or hostile server otherwise gets to drive us to OOM, since the
/// JSON/text helpers read the whole body. 256 MiB is far above any real
/// Forgejo API payload.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn check_body_size(res: &Response) -> Result<()> {
    if let Some(len) = res.content_length() {
        if len > MAX_RESPONSE_BYTES {
            return Err(anyhow!(
                "response body is {len} bytes, over the {MAX_RESPONSE_BYTES}-byte cap; refusing to buffer it"
            ));
        }
    }
    Ok(())
}

pub(crate) async fn ensure_success(res: Response) -> Result<Response> {
    let status = res.status();
    check_body_size(&res)?;
    if status.is_success() {
        return Ok(res);
    }
    let url = res.url().clone();
    let retry_after = (status == StatusCode::TOO_MANY_REQUESTS).then(|| parse_retry_after(&res));
    let text = res.text().await.unwrap_or_default();
    let message = parse_error_message(&text).unwrap_or_else(|| text.clone());
    let err = ApiError {
        status,
        url: proxy::redact_url(&url),
        message,
        body: text,
    };
    let context = friendly_http_error(status, &url, &err.message, retry_after.flatten());
    match context {
        Some(context) => Err(anyhow::Error::new(err).context(context)),
        None => Err(anyhow::Error::new(err)),
    }
}

pub(crate) fn with_not_found_context(err: anyhow::Error, context: String) -> anyhow::Error {
    if err
        .downcast_ref::<ApiError>()
        .is_some_and(|e| e.status == StatusCode::NOT_FOUND)
    {
        err.context(context)
    } else {
        err
    }
}

fn friendly_http_error(
    status: StatusCode,
    url: &Url,
    message: &str,
    retry_after: Option<Duration>,
) -> Option<String> {
    let resource = api_resource_label(url).unwrap_or_else(|| "requested resource".to_string());
    match status {
        StatusCode::UNAUTHORIZED => {
            Some("authentication failed (HTTP 401). Token may be invalid or revoked".to_string())
        }
        StatusCode::FORBIDDEN => Some(format!(
            "forbidden: {resource} (token lacks scope or access)"
        )),
        StatusCode::NOT_FOUND => Some(format!("not found: {resource}")),
        // 409/422 are request problems where the server's own message is the
        // actionable detail ("branch already exists", a validation error), so we
        // surface it in the headline instead of the raw /api/v1 URL. The full
        // ApiError (with the URL) stays as the `caused by:` source for --debug.
        StatusCode::CONFLICT => Some(detail_headline("conflict", &resource, message)),
        StatusCode::UNPROCESSABLE_ENTITY => {
            Some(detail_headline("invalid request", &resource, message))
        }
        StatusCode::TOO_MANY_REQUESTS => match retry_after {
            Some(wait) => Some(format!("rate limited: retry after {} s", wait.as_secs())),
            None => Some("rate limited: retry later".to_string()),
        },
        _ => None,
    }
}

/// Headline for statuses whose server message carries the actionable detail
/// (409 conflict, 422 validation): `"<label>: <resource>: <message>"`, or just
/// `"<label>: <resource>"` when the server sent no message. The message is
/// length-capped so a verbose validation body can't blow up the headline.
fn detail_headline(label: &str, resource: &str, message: &str) -> String {
    let msg = message.trim();
    if msg.is_empty() {
        format!("{label}: {resource}")
    } else {
        format!("{label}: {resource}: {}", truncate_preview(msg))
    }
}

fn api_resource_label(url: &Url) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    if let Some(repos) = segments.iter().position(|segment| *segment == "repos") {
        let owner = segments.get(repos + 1)?;
        let repo = segments.get(repos + 2)?;
        let repo_label = format!("{owner}/{repo}");
        return match segments.get(repos + 3).copied() {
            Some("issues" | "pulls") => match segments.get(repos + 4) {
                Some(number) => Some(format!("{repo_label} #{number}")),
                None => Some(repo_label),
            },
            _ => Some(repo_label),
        };
    }

    let orgs = segments.iter().position(|segment| *segment == "orgs")?;
    let org = segments.get(orgs + 1)?;
    Some(format!("org {org}"))
}

fn parse_error_message(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("error")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
}

fn default_user_agent() -> String {
    default_user_agent_static().to_string()
}

const fn default_user_agent_static() -> &'static str {
    concat!("fj/", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod trust_tests {
    use super::*;

    fn client(base: &str) -> Client {
        Client::for_base_url(Url::parse(base).unwrap(), "tok".into()).unwrap()
    }

    #[test]
    fn debug_body_preview_never_carries_a_secret_or_variable_value() {
        let secret = serde_json::json!({ "data": "hunter2-secret-value" });
        for path in [
            "/api/v1/repos/o/r/actions/secrets/DEPLOY_KEY",
            "/api/v1/orgs/team/actions/secrets/DEPLOY_KEY",
            "/api/v1/repos/o/r/actions/variables/REGION",
            "/api/v1/orgs/team/actions/variables/REGION",
        ] {
            let line = debug_body_preview(path, &secret).expect("a marker line");
            assert_eq!(line, "[redacted: sensitive endpoint]", "{path}");
            assert!(!line.contains("hunter2"), "{path}");
        }
        // An ordinary endpoint still shows its body.
        let line = debug_body_preview(
            "/api/v1/repos/o/r/issues",
            &serde_json::json!({ "title": "t" }),
        )
        .unwrap();
        assert!(line.contains("\"title\""));
    }

    #[test]
    fn truncate_preview_does_not_split_multibyte_chars() {
        // A multi-byte char straddling byte offset 200 must not panic.
        // 198 ASCII bytes then crabs: byte 200 lands mid-emoji.
        let text = format!("{}{}", "a".repeat(198), "🦀🦀🦀");
        let preview = truncate_preview(&text);
        assert!(preview.ends_with('…'), "should be truncated with ellipsis");
        // 200 chars kept + the ellipsis.
        assert_eq!(preview.chars().count(), 201);
    }

    #[test]
    fn truncate_preview_passes_short_text_through() {
        assert_eq!(truncate_preview("hello"), "hello");
        let exactly_200: String = "x".repeat(200);
        assert_eq!(truncate_preview(&exactly_200), exactly_200);
    }

    #[test]
    fn user_creation_body_is_never_debug_logged() {
        // `fj admin user create` posts the initial password here.
        assert!(is_sensitive_path("/api/v1/admin/users"));
        assert!(!is_sensitive_path("/api/v1/admin/cron"));
    }

    #[test]
    fn trusts_same_host_https() {
        let c = client("https://rasterhub.com/api/v1/");
        assert!(c.is_trusted_url(&Url::parse("https://rasterhub.com/api/v1/user").unwrap()));
        assert!(c.is_trusted_url(&Url::parse("https://RasterHub.com/repos/x/y").unwrap()));
    }

    #[test]
    fn rejects_other_host_or_cleartext() {
        let c = client("https://rasterhub.com/api/v1/");
        assert!(!c.is_trusted_url(&Url::parse("https://attacker.example/x").unwrap()));
        assert!(!c.is_trusted_url(&Url::parse("http://rasterhub.com/api/v1/user").unwrap()));
        assert!(!c.is_trusted_url(&Url::parse("https://rasterhub.com:8443/x").unwrap()));
    }
}
