pub mod error;
#[cfg(test)]
mod integration_tests;
pub mod pagination;
mod resolve;

use resolve::build_base_url;
pub use resolve::{resolve, web_base_url, ResolvedHost};

use std::sync::atomic::{AtomicBool, Ordering};
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
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::config::hosts::AuthKind;
use crate::fjord;

pub use error::ApiError;
pub use pagination::Page;

/// Which HTTP `Authorization` shape a `Client` uses on outgoing requests.
/// `Pat` is the historical Forgejo PAT scheme (`Authorization: token <pat>`).
/// `FjordBearer` is the OAuth-style bearer fjord-platform mints from
/// `/api/v1/sign-in` and the gateway then translates per-instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    Pat,
    FjordBearer,
}

/// Convenience handle for talking to a single Forgejo host (or, in
/// Fjord-Account mode, to a per-instance gateway endpoint that fronts one).
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    #[allow(dead_code)]
    host: String,
    base: Url,
    token: String,
    auth_scheme: AuthScheme,
}

impl Client {
    pub fn new(resolved: ResolvedHost) -> Result<Self> {
        let (base, auth_scheme) = match resolved.host.auth_kind {
            AuthKind::Pat => (
                build_base_url(&resolved.name, &resolved.host)?,
                AuthScheme::Pat,
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
                )
            }
        };
        let http = reqwest::Client::builder()
            .user_agent(default_user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            host: resolved.name,
            base,
            token: resolved.token,
            auth_scheme,
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
        Self::for_base_url_with_scheme(base_url, token, AuthScheme::Pat)
    }

    /// Like `for_base_url` but lets tests pick the auth scheme.
    #[cfg(test)]
    pub fn for_base_url_with_scheme(
        base_url: Url,
        token: String,
        auth_scheme: AuthScheme,
    ) -> Result<Self> {
        let host = base_url.host_str().map(str::to_string).unwrap_or_default();
        let http = reqwest::Client::builder()
            .user_agent(default_user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            host,
            base: base_url,
            token,
            auth_scheme,
        })
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

    /// Raw token. Use only when constructing custom request headers.
    #[allow(dead_code)]
    pub fn token(&self) -> &str {
        &self.token
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
    pub fn auth_header_value(&self) -> Result<HeaderValue> {
        let raw = match self.auth_scheme {
            AuthScheme::Pat => format!("token {}", self.token),
            AuthScheme::FjordBearer => format!("Bearer {}", self.token),
        };
        HeaderValue::from_str(&raw).map_err(|_| {
            anyhow!(
                "stored credential for host '{}' contains characters that can't appear in an HTTP header. Re-run `fj auth login` with a clean token.",
                self.host
            )
        })
    }

    /// Whether `url` is the same origin (scheme + host + port) as the
    /// configured API base. The auth token is only ever attached to requests
    /// that pass this check, so an absolute URL handed to `fj api` (or any
    /// redirected target) can't exfiltrate the bearer token to another host
    /// or downgrade it onto cleartext. Real bases are always `https` (see
    /// `build_base_url`), so requiring the scheme to match the base also
    /// rejects `fj api http://<same-host>/…`.
    fn is_trusted_url(&self, url: &Url) -> bool {
        url.scheme() == self.base.scheme()
            && url
                .host_str()
                .zip(self.base.host_str())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            && url.port_or_known_default() == self.base.port_or_known_default()
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.auth_header_value()?);
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
        let mut headers = self.auth_headers()?;
        if !self.is_trusted_url(&url) {
            // Absolute URL pointing off-host or not over TLS (only reachable
            // via `fj api <url>`): never leak the token there.
            headers.remove(AUTHORIZATION);
            eprintln!(
                "warning: not sending your {} token to {} (only same-host HTTPS requests are authenticated)",
                self.host,
                url.host_str().unwrap_or("?")
            );
        }
        for (k, v) in extra.iter() {
            headers.insert(k.clone(), v.clone());
        }

        if debug_enabled() {
            log_request(&method, &url, query, body);
        }

        let retries = if is_idempotent(&method) { 3 } else { 1 };

        // Build the request once. `Request::try_clone` (used per retry below)
        // succeeds for any request whose body is `None`, `Bytes`, or `Text`.
        // We only ever set a JSON body, which lives in memory as bytes, so the
        // clone is always successful.
        let mut builder = self
            .http
            .request(method.clone(), url.clone())
            .headers(headers)
            .query(query);
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
                            eprintln!("← 429 {} (rate-limited, sleeping {:?})", res.url(), wait);
                        }
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    if status.is_server_error() && attempt + 1 < retries {
                        if debug_enabled() {
                            eprintln!(
                                "← {status} {} (retrying after backoff, attempt {}/{})",
                                res.url(),
                                attempt + 1,
                                retries
                            );
                        }
                        backoff(attempt).await;
                        continue;
                    }
                    if debug_enabled() {
                        eprintln!("← {status} {}", res.url());
                    }
                    return Ok(res);
                }
                Err(e) => {
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
        let items: Vec<T> = decode_list(&text)?;
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
            let mut items: Vec<T> = decode_list(&text)?;
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
fn decode_list<T: DeserializeOwned>(text: &str) -> Result<Vec<T>> {
    let t = text.trim();
    if t.is_empty() || t == "null" {
        Ok(Vec::new())
    } else {
        serde_json::from_str(t).context("decoding JSON list response")
    }
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
    let q = if query.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("?{}", pairs.join("&"))
    };
    eprintln!("→ {method} {url}{q}");
    if let Some(b) = body {
        if is_sensitive_path(url.path()) {
            eprintln!("  body: [redacted: sensitive endpoint]");
        } else if let Ok(text) = serde_json::to_string(b) {
            eprintln!("  body: {}", truncate_preview(&text));
        }
    }
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
    ["secret", "variable", "token", "password", "/keys"]
        .iter()
        .any(|needle| p.contains(needle))
}

/// Upper bound on a single response body we'll buffer into memory. A
/// misbehaving or hostile server otherwise gets to drive us to OOM, since the
/// JSON/text helpers read the whole body. 256 MiB is far above any real
/// Forgejo API payload.
const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

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

async fn ensure_success(res: Response) -> Result<Response> {
    let status = res.status();
    check_body_size(&res)?;
    if status.is_success() {
        return Ok(res);
    }
    let url = res.url().clone();
    let text = res.text().await.unwrap_or_default();
    let message = parse_error_message(&text).unwrap_or_else(|| text.clone());
    let err = ApiError {
        status,
        url: url.to_string(),
        message,
        body: text,
    };
    if status == StatusCode::UNAUTHORIZED {
        Err(anyhow::Error::new(err)
            .context("authentication failed (HTTP 401). Token may be invalid or revoked"))
    } else {
        Err(anyhow::Error::new(err))
    }
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
