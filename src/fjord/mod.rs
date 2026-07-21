//! Fjord Account credentials sign-in + per-user forge instance listing.
//!
//! These calls go to fjord-platform (the SvelteKit gateway), NOT to a Forgejo
//! instance. They mint and consume the bearer fj stores in the keychain for
//! Fjord-Account-mode hosts. Once a bearer is in hand, all subsequent Forgejo
//! API calls flow through `/api/v1/forge-gateway/:instance-id/api/v1/...`
//! against the user's selected instance, with the bearer attached. See
//! `client::mod` for that side.
//!
//! The endpoints implemented here are documented in
//! `fjord-platform/docs/PHASE_6_DESIGN.md` and live in
//! `fjord-platform/src/routes/api/v1/`.

use anyhow::{anyhow, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use url::Url;

pub mod oidc;

/// Default fjord-platform deployment users sign in against.
pub const DEFAULT_PLATFORM_URL: &str = "https://forgejo-site.pages.dev";

/// Marker error: a Fjord-Account call got HTTP 401. Returned bare (never
/// wrapped with `.context()`) so callers can `downcast_ref` it and decide
/// whether to attempt a silent OIDC refresh before giving up. Its `Display`
/// is the user-facing message for the terminal case.
#[derive(Debug, thiserror::Error)]
#[error("Fjord Account session expired or revoked (HTTP 401). Re-run `fj auth login --fjord`.")]
pub struct SessionExpired;

fn user_agent() -> &'static str {
    concat!("fj/", env!("CARGO_PKG_VERSION"))
}

/// Normalize a user-supplied platform URL: enforce scheme + strip trailing
/// slash. Stored canonically so the keychain key is stable.
pub fn normalize_platform_url(raw: &str) -> Result<String> {
    let parsed = Url::parse(raw).with_context(|| format!("invalid --platform-url '{raw}'"))?;
    match parsed.scheme() {
        "https" | "http" => {}
        other => {
            return Err(anyhow!("platform URL must be http or https, got '{other}'"));
        }
    }
    if parsed.host_str().is_none() {
        return Err(anyhow!("platform URL '{raw}' has no host"));
    }
    let s = parsed.as_str().trim_end_matches('/').to_string();
    Ok(s)
}

/// Build the bearer client used for `/api/v1/sign-in` (no bearer yet) and
/// for the authenticated endpoints (`me/forge-instances` etc.).
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .context("building HTTP client for Fjord platform")
}

#[derive(Debug, Clone, Serialize)]
pub struct SignInRequest<'a> {
    pub email: &'a str,
    pub password: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totp_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_code: Option<&'a str>,
}

/// Sealed-union response from `POST /api/v1/sign-in`. Mirrors the
/// `ApiSignInResponse` shape from fjord-platform; see the definitive
/// source at
/// `fjord-platform/src/routes/api/v1/sign-in/+server.ts`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SignInResponse {
    Success {
        #[serde(rename = "accessToken")]
        access_token: String,
        #[serde(rename = "refreshToken")]
        #[allow(dead_code)]
        refresh_token: String,
        #[serde(rename = "accessTokenExp")]
        #[allow(dead_code)]
        access_token_exp: i64,
        user: SignInUser,
    },
    TotpRequired {
        #[serde(rename = "challengeToken")]
        #[allow(dead_code)]
        challenge_token: String,
        #[serde(default)]
        message: Option<String>,
    },
    WebauthnRequired {
        #[serde(default)]
        #[allow(dead_code)]
        message: Option<String>,
    },
    MfaSetupRequired {
        #[serde(default)]
        message: Option<String>,
    },
    Locked {
        #[serde(rename = "retryAfterSeconds")]
        retry_after_seconds: u64,
        message: String,
    },
    InvalidCredentials {
        message: String,
    },
    Misconfigured {
        message: String,
    },
    UpstreamError {
        #[allow(dead_code)]
        status: i64,
        message: String,
    },
    StrategyMismatch {
        message: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignInUser {
    #[serde(rename = "userId")]
    #[allow(dead_code)]
    pub user_id: String,
    pub email: Option<String>,
    #[allow(dead_code)]
    pub role: String,
}

/// POST `/api/v1/sign-in` with email + password (and optionally a TOTP /
/// recovery code). `platform_url` should already be normalized.
pub async fn sign_in(platform_url: &str, req: &SignInRequest<'_>) -> Result<SignInResponse> {
    let url = format!("{platform_url}/api/v1/sign-in");
    let http = http_client()?;
    let res = http
        .post(&url)
        .header(USER_AGENT, user_agent())
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    // The sign-in endpoint uses its own status codes (200, 401, 429, …)
    // alongside the discriminant in the body. We decode the body either
    // way; the HTTP status is informational.
    let status = res.status();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading sign-in response body (HTTP {status})"))?;
    // 4xx / 5xx without a parseable body becomes an explicit error.
    serde_json::from_str::<SignInResponse>(&text).with_context(|| {
        format!(
            "decoding sign-in response (HTTP {status}); body was: {}",
            preview(&text)
        )
    })
}

fn preview(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() > 240 {
        format!("{}…", &trimmed[..240])
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForgeInstance {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub slug: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(rename = "publicUrl", default)]
    pub public_url: Option<String>,
    #[serde(rename = "createdAt", default)]
    #[allow(dead_code)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ForgeInstancesResponse {
    instances: Vec<ForgeInstance>,
}

/// GET `/api/v1/me/forge-instances`. Paragon returning 404 (no instances
/// provisioned for this user) maps to an empty list rather than an error.
pub async fn list_instances(platform_url: &str, bearer: &str) -> Result<Vec<ForgeInstance>> {
    let url = format!("{platform_url}/api/v1/me/forge-instances");
    let http = http_client()?;
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(user_agent()));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|_| anyhow!("stored Fjord bearer contains invalid header chars"))?,
    );
    let res = http
        .get(&url)
        .headers(headers)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if status == StatusCode::UNAUTHORIZED {
        // Bare marker (no `.context()`) so a caller with a stored refresh token
        // can downcast it, silently refresh, and retry before surfacing this.
        return Err(SessionExpired.into());
    }
    let text = res
        .text()
        .await
        .with_context(|| format!("reading forge-instances response (HTTP {status})"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "fjord-platform returned HTTP {status} from {url}: {}",
            preview(&text)
        ));
    }
    // Paragon returns 200 with an empty list, OR (on a misconfigured
    // adapter) with no "instances" key at all. Tolerate both shapes.
    let parsed: serde_json::Value =
        serde_json::from_str(&text).context("decoding forge-instances JSON")?;
    if parsed.get("instances").is_none() {
        return Ok(Vec::new());
    }
    let typed: ForgeInstancesResponse = serde_json::from_value(parsed)
        .context("decoding forge-instances response into typed shape")?;
    Ok(typed.instances)
}

/// Build the Forgejo-API base URL the `Client` uses when routing through
/// the gateway. Always ends with `/api/v1/` so existing call sites that
/// pass `/api/v1/<path>` strip-and-join cleanly.
pub fn gateway_base_url(platform_url: &str, instance_id: &str) -> Result<Url> {
    let s = format!("{platform_url}/api/v1/forge-gateway/{instance_id}/api/v1/");
    Url::parse(&s).with_context(|| format!("building gateway base url from '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_platform_url("https://forgejo-site.pages.dev/").unwrap(),
            "https://forgejo-site.pages.dev"
        );
        assert_eq!(
            normalize_platform_url("https://forgejo-site.pages.dev").unwrap(),
            "https://forgejo-site.pages.dev"
        );
    }

    #[test]
    fn normalize_rejects_scheme_other_than_http_https() {
        assert!(normalize_platform_url("ftp://example.com").is_err());
        assert!(normalize_platform_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn normalize_rejects_garbage() {
        assert!(normalize_platform_url("not a url").is_err());
        assert!(normalize_platform_url("").is_err());
    }

    #[test]
    fn gateway_base_url_shape() {
        let u = gateway_base_url("https://forgejo-site.pages.dev", "inst_abc").unwrap();
        assert_eq!(
            u.as_str(),
            "https://forgejo-site.pages.dev/api/v1/forge-gateway/inst_abc/api/v1/"
        );
    }

    /// The base URL must end with `/api/v1/` so when `Client::url`
    /// joins `/api/v1/user` against it (after stripping the leading
    /// `/api/v1/`), the result is the gateway path `…/inst/api/v1/user`,
    /// not a double-prefixed `…/inst/api/v1/api/v1/user`.
    #[test]
    fn gateway_base_joins_cleanly_with_forgejo_paths() {
        let base = gateway_base_url("https://p.example", "inst").unwrap();
        let joined = base.join("user").unwrap();
        assert_eq!(
            joined.as_str(),
            "https://p.example/api/v1/forge-gateway/inst/api/v1/user"
        );
    }
}
