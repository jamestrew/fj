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

pub mod lock;
pub mod oidc;

/// Default fjord-platform deployment users sign in against.
pub const DEFAULT_PLATFORM_URL: &str = "https://fjord.sh";

/// Marker error: a Fjord-Account call got HTTP 401. Returned bare (never
/// wrapped with `.context()`) so callers can `downcast_ref` it and decide
/// whether to attempt a silent OIDC refresh before giving up. Its `Display`
/// is the user-facing message for the terminal case.
#[derive(Debug, thiserror::Error)]
#[error("Fjord Account session expired or revoked (HTTP 401). Re-run `fj auth login --fjord`.")]
pub struct SessionExpired;

/// Marker error: the Fjord Account sign-in can no longer be renewed. The OP
/// answered the token exchange with `invalid_grant`, which means the refresh
/// token was spent, replayed, revoked, or aged out. No retry recovers it.
///
/// Returned bare so callers can `downcast_ref` it. Callers are free to add
/// their own `.context()`: `main::error_headline` pulls this message back out
/// of the chain, because the causes under the outermost one are printed only
/// with `--debug` and this is the message the user has to act on.
#[derive(Debug, thiserror::Error)]
#[error(
    "this Fjord Account sign-in can no longer be renewed (the refresh token was spent, revoked, \
     or has expired). Run `fj auth login --fjord` to sign in again."
)]
pub struct UnrenewableGrant;

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
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading sign-in response body (HTTP {status})"))?;
    ensure_json_platform_response("sign-in endpoint", status, Some(&headers), &text)?;
    // 4xx / 5xx without a parseable body becomes an explicit error.
    serde_json::from_str::<SignInResponse>(&text).with_context(|| {
        format!(
            "decoding sign-in response (HTTP {status}); body was: {}",
            preview(&text)
        )
    })
}

pub(crate) fn ensure_json_platform_response(
    label: &str,
    status: StatusCode,
    headers: Option<&HeaderMap>,
    text: &str,
) -> Result<()> {
    let content_type = headers
        .and_then(|headers| headers.get(reqwest::header::CONTENT_TYPE))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let trimmed = text.trim_start();
    let html_body = trimmed
        .get(..trimmed.len().min(64))
        .map(|prefix| prefix.to_ascii_lowercase())
        .is_some_and(|prefix| prefix.starts_with("<!doctype html") || prefix.starts_with("<html"));
    let non_json_content_type =
        !content_type.is_empty() && !content_type.to_ascii_lowercase().contains("json");
    if html_body || non_json_content_type {
        let kind = if html_body {
            "an HTML page"
        } else {
            "a non-JSON response"
        };
        return Err(anyhow!(
            "{label} returned {kind} rather than the Fjord platform API (HTTP {status}); \
             check `--platform-url`. Body starts: {}",
            preview(text)
        ));
    }
    Ok(())
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

impl ForgeInstance {
    /// Whether the instance can be routed to right now. A missing state counts
    /// as ready (older deployments do not report one); any other value, such
    /// as `provisioning`, does not. Same rule as the phone apps' `isReady`.
    pub fn is_ready(&self) -> bool {
        self.state
            .as_deref()
            .is_none_or(|state| state.eq_ignore_ascii_case("ready"))
    }
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
    let headers = res.headers().clone();
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
    ensure_json_platform_response("forge-instances endpoint", status, Some(&headers), &text)?;
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

/// The Commons, as `GET /api/v1/me/commons` reports it.
///
/// Mirrors `fjord-android`'s `account/ForgeInstance.kt` (`CommonsInfo`)
/// deliberately, field for field. fj and the phone apps disagreeing about what
/// the Commons is would become someone's bug later, and the app is the older
/// implementation, so it is the reference.
///
/// Note what is NOT here: no token, no credential of any kind. The gateway
/// mints a per-user forge token server-side on each proxied call and attaches
/// it there; nothing minted is ever sent to a client. That is why fj cannot
/// extend the revocation window even in principle, and why nothing from this
/// response is cached to disk.
#[derive(Debug, Clone, Deserialize)]
pub struct CommonsInfo {
    pub url: String,
    pub name: String,
    /// Member count, when the platform reports one.
    #[serde(rename = "memberCount", default)]
    pub member_count: Option<u64>,
    #[serde(rename = "gatewayForgeId", default)]
    pub gateway_forge_id: Option<String>,
    #[serde(default)]
    pub membership: CommonsMembership,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommonsMembership {
    #[serde(default)]
    pub joined: bool,
    /// The account's namespace on the Commons, once it has one.
    #[serde(default)]
    pub namespace: Option<String>,
    /// The Commons organization the membership is held through.
    #[serde(rename = "orgSlug", default)]
    pub org_slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommonsResponse {
    #[serde(default)]
    commons: Option<CommonsInfo>,
}

impl CommonsInfo {
    /// The Commons as a switchable instance, or `None` when it cannot be one.
    ///
    /// The three refusals match the app's `toSwitchableInstance()` exactly: no
    /// gateway id, no public url, or not joined. The joined check is the one
    /// that matters operationally, since offering an instance the account
    /// cannot use produces a 403 from the gateway at the moment the user tries
    /// to do work, which is the worst place to learn it.
    pub fn to_switchable_instance(&self) -> Option<ForgeInstance> {
        let id = self
            .gateway_forge_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())?;
        let public_url = Some(self.url.clone()).filter(|s| !s.trim().is_empty())?;
        if !self.membership.joined {
            return None;
        }
        let display_name = if self.name.trim().is_empty() {
            "Fjord Commons".to_string()
        } else {
            self.name.clone()
        };
        Some(ForgeInstance {
            id: id.to_string(),
            display_name,
            slug: "commons".to_string(),
            region: Some("shared".to_string()),
            plan: Some("commons".to_string()),
            state: Some("ready".to_string()),
            public_url: Some(public_url),
            created_at: None,
        })
    }

    /// The name to show for the Commons, never blank.
    pub fn display_name(&self) -> &str {
        match self.name.trim() {
            "" => "Fjord Commons",
            name => name,
        }
    }

    /// The URL to hand a joined account to in the browser when the Commons is
    /// not gateway-routable, or `None` when no hand-off applies: not joined
    /// (an un-joined Commons is never presented), already switchable (it is
    /// an instance, not a hand-off), or a url that is not a plain web origin.
    ///
    /// The scheme check is not decoration. This string comes from the
    /// platform and ends up as an argument to the OS opener, which will
    /// happily launch `file:` or a custom scheme handler.
    pub fn browser_handoff_url(&self) -> Option<&str> {
        if !self.membership.joined || self.to_switchable_instance().is_some() {
            return None;
        }
        let url = self.url.trim();
        let parsed = Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return None;
        }
        Some(url)
    }
}

/// GET `/api/v1/me/commons`.
///
/// The Commons is intentionally absent from `/me/forge-instances` (it is not an
/// instance the account owns), so this is the only way a client learns it has a
/// gateway id at all. The phone apps have called it since before fj existed;
/// this is fj catching up rather than a new mechanism.
///
/// Absent-endpoint handling is deliberate: a platform older than this endpoint
/// answers 404, and so does a platform with the Commons feature flag off. Both
/// mean "no Commons here", which is today's behaviour, so both map to `Ok(None)`
/// rather than to an error. A user who explicitly asks for Commons gets a clear
/// message from the caller; a user who did not ask sees no change.
pub async fn fetch_commons(platform_url: &str, bearer: &str) -> Result<Option<CommonsInfo>> {
    let url = format!("{platform_url}/api/v1/me/commons");
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
        return Ok(None);
    }
    if status == StatusCode::UNAUTHORIZED {
        // Bare marker, same contract as list_instances: the caller downcasts
        // it, refreshes once, and retries before surfacing anything.
        return Err(SessionExpired.into());
    }
    let headers = res.headers().clone();
    let text = res
        .text()
        .await
        .with_context(|| format!("reading me/commons response (HTTP {status})"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "fjord-platform returned HTTP {status} from {url}: {}",
            preview(&text)
        ));
    }
    ensure_json_platform_response("me/commons endpoint", status, Some(&headers), &text)?;
    let parsed: CommonsResponse =
        serde_json::from_str(&text).context("decoding me/commons JSON")?;
    Ok(parsed.commons)
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

    fn commons(value: serde_json::Value) -> CommonsInfo {
        serde_json::from_value(value).expect("commons descriptor decodes")
    }

    #[test]
    fn commons_descriptor_decodes_every_field() {
        let c = commons(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "memberCount": 42,
            "gatewayForgeId": "box_1",
            "membership": { "joined": true, "namespace": "stephen", "orgSlug": "commons" },
        }));
        assert_eq!(c.url, "https://commons.example");
        assert_eq!(c.name, "The Commons");
        assert_eq!(c.member_count, Some(42));
        assert_eq!(c.gateway_forge_id.as_deref(), Some("box_1"));
        assert!(c.membership.joined);
        assert_eq!(c.membership.namespace.as_deref(), Some("stephen"));
        assert_eq!(c.membership.org_slug.as_deref(), Some("commons"));
    }

    #[test]
    fn commons_descriptor_tolerates_absent_and_null_optionals() {
        let absent = commons(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "membership": { "joined": false },
        }));
        assert_eq!(absent.member_count, None);
        assert_eq!(absent.gateway_forge_id, None);
        assert_eq!(absent.membership.namespace, None);
        assert_eq!(absent.membership.org_slug, None);

        let nulls = commons(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "memberCount": null,
            "gatewayForgeId": null,
            "membership": { "joined": true, "namespace": null, "orgSlug": null },
        }));
        assert_eq!(nulls.member_count, None);
        assert_eq!(nulls.membership.namespace, None);
        assert!(nulls.membership.joined);

        // A descriptor with no membership object at all is not a joined one:
        // membership is never inferred from the descriptor being present.
        let bare = commons(serde_json::json!({ "url": "https://c.example", "name": "C" }));
        assert!(!bare.membership.joined);
        assert!(bare.to_switchable_instance().is_none());
        assert!(bare.browser_handoff_url().is_none());
    }

    #[test]
    fn joined_routable_commons_is_an_instance_not_a_handoff() {
        let c = commons(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "gatewayForgeId": "box_1",
            "membership": { "joined": true },
        }));
        let instance = c.to_switchable_instance().expect("switchable");
        assert_eq!(instance.id, "box_1");
        assert_eq!(instance.state.as_deref(), Some("ready"));
        assert!(c.browser_handoff_url().is_none());
    }

    #[test]
    fn joined_commons_without_a_gateway_id_is_a_browser_handoff() {
        for gateway in [serde_json::Value::Null, serde_json::json!("  ")] {
            let c = commons(serde_json::json!({
                "url": "https://commons.example",
                "name": "The Commons",
                "gatewayForgeId": gateway,
                "membership": { "joined": true },
            }));
            assert!(c.to_switchable_instance().is_none());
            assert_eq!(c.browser_handoff_url(), Some("https://commons.example"));
        }
    }

    #[test]
    fn unjoined_commons_is_neither_an_instance_nor_a_handoff() {
        let c = commons(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "gatewayForgeId": "box_1",
            "membership": { "joined": false },
        }));
        assert!(c.to_switchable_instance().is_none());
        assert!(c.browser_handoff_url().is_none());
    }

    #[test]
    fn handoff_refuses_a_url_that_is_not_a_web_origin() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "", "not a url"] {
            let c = commons(serde_json::json!({
                "url": url,
                "name": "The Commons",
                "membership": { "joined": true },
            }));
            assert!(c.browser_handoff_url().is_none(), "{url}");
        }
    }

    #[test]
    fn instance_readiness_treats_a_missing_state_as_ready() {
        let with = |state: serde_json::Value| -> ForgeInstance {
            serde_json::from_value(serde_json::json!({
                "id": "i", "displayName": "I", "slug": "i", "state": state,
            }))
            .unwrap()
        };
        assert!(with(serde_json::Value::Null).is_ready());
        assert!(with(serde_json::json!("ready")).is_ready());
        assert!(with(serde_json::json!("READY")).is_ready());
        assert!(!with(serde_json::json!("provisioning")).is_ready());
        assert!(!with(serde_json::json!("suspended")).is_ready());
    }

    #[test]
    fn commons_display_name_falls_back_when_blank() {
        let c = commons(serde_json::json!({ "url": "https://c.example", "name": " " }));
        assert_eq!(c.display_name(), "Fjord Commons");
    }
}
