pub mod error;
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

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("token {}", self.token))
                .expect("token contains invalid header chars"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(default_user_agent_static()),
        );
        headers
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
    pub async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&serde_json::Value>,
        extra: &HeaderMap,
    ) -> Result<Response> {
        let url = self.url(path)?;
        let mut headers = self.auth_headers();
        for (k, v) in extra.iter() {
            headers.insert(k.clone(), v.clone());
        }
        let mut req = self
            .http
            .request(method.clone(), url.clone())
            .headers(headers)
            .query(query);
        if let Some(body) = body {
            req = req.json(body);
        }
        if debug_enabled() {
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
        let res = req.send().await.context("sending HTTP request")?;
        if debug_enabled() {
            eprintln!("← {} {}", res.status(), res.url());
        }
        Ok(res)
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
        let items: Vec<T> = res.json().await.context("decoding JSON list response")?;
        Ok(Page::from_headers(items, &headers))
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
