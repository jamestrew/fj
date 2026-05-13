pub mod error;
#[cfg(test)]
mod integration_tests;
pub mod pagination;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static DEBUG: AtomicBool = AtomicBool::new(false);

/// Toggle request logging at runtime. Set by `--debug` / `FJ_DEBUG`.
pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

use anyhow::{anyhow, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::auth;
use crate::config::hosts::{api_base_path, Host, Hosts};

pub use error::ApiError;
pub use pagination::Page;

/// Convenience handle for talking to a single Forgejo host.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    #[allow(dead_code)]
    host: String,
    base: Url,
    token: String,
}

pub struct ResolvedHost {
    pub name: String,
    pub host: Host,
    pub token: String,
}

/// Look up the host config and token for the host the user is targeting.
pub fn resolve(host_flag: Option<&str>) -> Result<ResolvedHost> {
    let hosts = Hosts::load()?;
    let name = hosts.resolve_host(host_flag)?.to_string();
    let host_cfg = hosts
        .hosts
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow!("host '{name}' not configured"))?;
    let token = auth::load_token(&name)?.ok_or_else(|| {
        anyhow!("no token stored for host '{name}'. Run `fj auth login --host {name}`.")
    })?;
    Ok(ResolvedHost {
        name,
        host: host_cfg,
        token,
    })
}

impl Client {
    pub fn new(resolved: ResolvedHost) -> Result<Self> {
        let base = build_base_url(&resolved.name, &resolved.host)?;
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
        })
    }

    /// Construct a client from an explicit host flag, loading config + token.
    pub fn connect(host_flag: Option<&str>) -> Result<Self> {
        Self::new(resolve(host_flag)?)
    }

    /// Construct a client pointing at an arbitrary base URL. Used for tests
    /// against a local mock server. `base_url` must end with `/`.
    #[allow(dead_code)]
    pub fn for_base_url(base_url: Url, token: String) -> Result<Self> {
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
    pub fn token(&self) -> &str {
        &self.token
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_str(&format!("token {}", self.token)).map_err(|_| {
            anyhow!(
                "stored token for host '{}' contains characters that can't appear in an HTTP header. Re-run `fj auth login --host {}` with a clean token.",
                self.host,
                self.host
            )
        })?;
        headers.insert(AUTHORIZATION, value);
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
        let items: Vec<T> = if text.trim().is_empty() || text.trim() == "null" {
            Vec::new()
        } else {
            serde_json::from_str(&text).context("decoding JSON list response")?
        };
        Ok(Page::from_headers(items, &headers))
    }

    /// GET that follows `Link: rel=next` until either `total_limit` items have
    /// been collected or there are no more pages. Use this when the caller's
    /// requested limit exceeds Forgejo's per-page cap (50).
    pub async fn get_all<T: DeserializeOwned>(
        &self,
        path: &str,
        base_query: &[(String, String)],
        total_limit: usize,
    ) -> Result<Vec<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut current_path: String = path.to_string();
        let mut current_query: Vec<(String, String)> = base_query.to_vec();
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
            let mut items: Vec<T> = res.json().await.context("decoding JSON list response")?;
            if items.is_empty() {
                break;
            }
            let need = total_limit.saturating_sub(out.len());
            if items.len() > need {
                items.truncate(need);
            }
            out.append(&mut items);
            if out.len() >= total_limit {
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
        Ok(out)
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
        if let Ok(text) = serde_json::to_string(b) {
            let preview = if text.len() > 200 {
                format!("{}…", &text[..200])
            } else {
                text
            };
            eprintln!("  body: {preview}");
        }
    }
}

fn build_base_url(hostname: &str, host: &Host) -> Result<Url> {
    let scheme = "https";
    let mut url = Url::parse(&format!("{scheme}://{hostname}"))
        .with_context(|| format!("constructing URL for {hostname}"))?;
    let path = api_base_path(host).trim_end_matches('/');
    url.set_path(&format!("{path}/"));
    Ok(url)
}

async fn ensure_success(res: Response) -> Result<Response> {
    let status = res.status();
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
