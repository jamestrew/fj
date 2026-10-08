//! A forge behind a reverse proxy that demands its own HTTP Basic credential.
//!
//! Two secrets are in play and they must never be confused: the proxy
//! credential (username + password, checked by the proxy) and the forge
//! personal access token (checked by Forgejo/Gitea). Everything here is pure;
//! the request funnel in `client::mod` owns the I/O.
//!
//! # The header conflict
//!
//! A proxy that answers `407` reads `Proxy-Authorization`, so both credentials
//! travel in their own header and nothing conflicts.
//!
//! A reverse proxy such as nginx `auth_basic` answers `401` with
//! `WWW-Authenticate: Basic` and reads `Authorization`. The forge also reads
//! `Authorization` (`token <pat>`), and one request has one `Authorization`.
//! The proxy credential has to win that header or the request never reaches
//! the forge, so the token needs another carrier. There are two, and each has
//! a real cost (see [`PatTransport`]). Putting the token in the Basic password
//! would work with a stock proxy only if the proxy password WERE the token,
//! which is exactly the confusion this feature exists to prevent, so fj does
//! not offer that.

use std::fmt;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;

/// The header fj carries the forge token in when `Authorization` is taken by
/// the proxy credential. The proxy must copy it to the upstream
/// `Authorization` header.
pub const FORWARDED_AUTH_HEADER: &str = "x-forge-authorization";

/// How the forge token travels once a `401 Basic` proxy has claimed the
/// `Authorization` header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatTransport {
    /// `X-Forge-Authorization: token <pat>`. The token stays out of URLs and
    /// logs, but the proxy needs one line of configuration to forward it as
    /// the upstream `Authorization` header. The default.
    #[default]
    Header,
    /// `?token=<pat>` on the URL. Works with an unmodified proxy, but only
    /// while the forge still accepts query tokens (`DISABLE_QUERY_AUTH_TOKEN`
    /// off), and the token lands in the proxy's and the forge's access logs.
    /// Opt-in only.
    Query,
}

impl PatTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            PatTransport::Header => "header",
            PatTransport::Query => "query",
        }
    }

    pub fn is_default(&self) -> bool {
        matches!(self, PatTransport::Header)
    }
}

/// The proxy credential for one host. `Debug` never prints the password.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyCredential {
    pub username: String,
    pub password: String,
    pub pat_transport: PatTransport,
}

impl fmt::Debug for ProxyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyCredential")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("pat_transport", &self.pat_transport)
            .finish()
    }
}

impl ProxyCredential {
    /// `Basic base64(user:password)`, marked sensitive so reqwest's own debug
    /// formatting hides it.
    pub fn basic_header(&self) -> Option<HeaderValue> {
        let raw = format!("{}:{}", self.username, self.password);
        let mut value = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(raw))).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

/// Which header the proxy asked for. Learned from the first challenge and
/// then sent preemptively for the rest of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// No challenge seen: send the request exactly as an unproxied host.
    Direct,
    /// `407`: the credential goes in `Proxy-Authorization` and the forge token
    /// keeps `Authorization`.
    ProxyAuthorization,
    /// `401` + `WWW-Authenticate: Basic`: the credential takes
    /// `Authorization` and the forge token moves to its [`PatTransport`].
    Basic,
}

impl ProxyMode {
    pub fn to_u8(self) -> u8 {
        match self {
            ProxyMode::Direct => 0,
            ProxyMode::ProxyAuthorization => 1,
            ProxyMode::Basic => 2,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ProxyMode::ProxyAuthorization,
            2 => ProxyMode::Basic,
            _ => ProxyMode::Direct,
        }
    }
}

fn offers_basic(headers: &HeaderMap, name: reqwest::header::HeaderName) -> bool {
    headers.get_all(name).iter().any(|v| {
        v.to_str().is_ok_and(|s| {
            s.split(',').any(|part| {
                let scheme = part.split_whitespace().next().unwrap_or("");
                scheme.eq_ignore_ascii_case("basic")
            })
        })
    })
}

/// Classify a response as a proxy challenge.
///
/// `407` is a proxy by definition. `401` counts only with a Basic
/// `WWW-Authenticate` challenge: Forgejo's own API 401 carries no
/// `WWW-Authenticate` header (checked against 16.0.4), so the header is what
/// separates "the proxy wants its credential" from "the forge rejected the
/// token".
pub fn challenge(status: StatusCode, headers: &HeaderMap) -> Option<ProxyMode> {
    match status {
        StatusCode::PROXY_AUTHENTICATION_REQUIRED => {
            // A 407 offering only another scheme (Negotiate, Digest) is still
            // a proxy challenge. fj can only answer Basic, and the error path
            // names the proxy either way.
            Some(ProxyMode::ProxyAuthorization)
        }
        StatusCode::UNAUTHORIZED if offers_basic(headers, WWW_AUTHENTICATE) => {
            Some(ProxyMode::Basic)
        }
        _ => None,
    }
}

/// Whether a proxy credential may be sent to `base` at all. Basic is base64,
/// not encryption, so it only ever goes over TLS or to this machine.
pub fn transport_is_safe(base: &Url) -> bool {
    if base.scheme() == "https" {
        return true;
    }
    match base.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The proxy asked for a credential and none is configured for this host.
pub fn missing_credential_error(host: &str, status: StatusCode) -> anyhow::Error {
    anyhow::anyhow!(
        "a reverse proxy in front of {host} requires its own HTTP Basic credential (HTTP {}). \
         This is the proxy's login, not your forge token. Set it with \
         `fj auth proxy --host {host} --username <user>` (or FJ_PROXY_USER and \
         FJ_PROXY_PASSWORD in CI). See docs/troubleshooting.md.",
        status.as_u16()
    )
}

/// The proxy challenged again after fj sent the configured credential.
pub fn rejected_credential_error(host: &str, status: StatusCode) -> anyhow::Error {
    anyhow::anyhow!(
        "the reverse proxy in front of {host} rejected the proxy credential (HTTP {}): the \
         proxy username or password is wrong or has expired. Your forge token was not the \
         problem. Update it with `fj auth proxy --host {host} --username <user>`.",
        status.as_u16()
    )
}

/// The base URL is cleartext and not loopback, so fj refused to send Basic.
pub fn unsafe_transport_error(host: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "a reverse proxy in front of {host} asked for an HTTP Basic credential over cleartext \
         HTTP. fj only sends the proxy credential over HTTPS or to a loopback address."
    )
}

/// The proxy let the request through and the forge then answered 401.
pub fn forge_rejected_error(host: &str, transport: PatTransport) -> anyhow::Error {
    match transport {
        PatTransport::Header => anyhow::anyhow!(
            "the reverse proxy in front of {host} accepted the proxy credential, but the forge \
             then rejected the request (HTTP 401). Either the forge token is invalid or \
             revoked, or the proxy is not forwarding the `X-Forge-Authorization` header as \
             `Authorization` (nginx: `proxy_set_header Authorization \
             $http_x_forge_authorization;`). If you cannot change the proxy, see \
             `fj auth proxy --help` for the query transport and its cost."
        ),
        PatTransport::Query => anyhow::anyhow!(
            "the reverse proxy in front of {host} accepted the proxy credential, but the forge \
             then rejected the request (HTTP 401). Either the forge token is invalid or \
             revoked, or the forge has query tokens disabled (`DISABLE_QUERY_AUTH_TOKEN`), in \
             which case use the default header transport: `fj auth proxy --host {host} \
             --pat-transport header`."
        ),
    }
}

const SECRET_QUERY_KEYS: [&str; 2] = ["token", "access_token"];

/// Whether a query key carries a credential.
pub fn is_secret_query_key(key: &str) -> bool {
    SECRET_QUERY_KEYS
        .iter()
        .any(|k| key.eq_ignore_ascii_case(k))
}

/// A URL safe to print: userinfo removed and any `token` / `access_token`
/// query value replaced. Applied to every URL fj writes to `--debug` output or
/// into an error, whatever the transport.
pub fn redact_url(url: &Url) -> String {
    let mut out = url.clone();
    let _ = out.set_username("");
    let _ = out.set_password(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                if is_secret_query_key(&k) {
                    (k.into_owned(), "[redacted]".to_string())
                } else {
                    (k.into_owned(), v.into_owned())
                }
            })
            .collect();
        out.query_pairs_mut().clear().extend_pairs(pairs);
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred() -> ProxyCredential {
        ProxyCredential {
            username: "ops".into(),
            password: "hunter2-proxy".into(),
            pat_transport: PatTransport::Header,
        }
    }

    fn headers(name: reqwest::header::HeaderName, value: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, HeaderValue::from_static(value));
        h
    }

    #[test]
    fn basic_header_encodes_user_and_password_and_is_sensitive() {
        let v = cred().basic_header().unwrap();
        assert!(v.is_sensitive());
        let raw = v.to_str().unwrap();
        let decoded = STANDARD
            .decode(raw.strip_prefix("Basic ").unwrap())
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "ops:hunter2-proxy");
        assert!(!format!("{v:?}").contains("hunter2"));
    }

    #[test]
    fn debug_never_prints_the_password() {
        let text = format!("{:?}", cred());
        assert!(text.contains("ops"));
        assert!(!text.contains("hunter2-proxy"), "{text}");
    }

    #[test]
    fn challenge_407_is_proxy_authorization() {
        assert_eq!(
            challenge(StatusCode::PROXY_AUTHENTICATION_REQUIRED, &HeaderMap::new()),
            Some(ProxyMode::ProxyAuthorization)
        );
    }

    #[test]
    fn challenge_401_needs_a_basic_www_authenticate() {
        let basic = headers(WWW_AUTHENTICATE, "Basic realm=\"Restricted\"");
        assert_eq!(
            challenge(StatusCode::UNAUTHORIZED, &basic),
            Some(ProxyMode::Basic)
        );
        let lower = headers(WWW_AUTHENTICATE, "basic");
        assert_eq!(
            challenge(StatusCode::UNAUTHORIZED, &lower),
            Some(ProxyMode::Basic)
        );
        let multi = headers(WWW_AUTHENTICATE, "Negotiate, Basic realm=\"x\"");
        assert_eq!(
            challenge(StatusCode::UNAUTHORIZED, &multi),
            Some(ProxyMode::Basic)
        );
    }

    #[test]
    fn forge_401_without_basic_challenge_is_not_a_proxy() {
        // Forgejo's own "token is required" 401 has no WWW-Authenticate.
        assert_eq!(challenge(StatusCode::UNAUTHORIZED, &HeaderMap::new()), None);
        let bearer = headers(WWW_AUTHENTICATE, "Bearer realm=\"x\"");
        assert_eq!(challenge(StatusCode::UNAUTHORIZED, &bearer), None);
        let basic = headers(WWW_AUTHENTICATE, "Basic realm=\"x\"");
        assert_eq!(challenge(StatusCode::FORBIDDEN, &basic), None);
        assert_eq!(challenge(StatusCode::OK, &basic), None);
    }

    #[test]
    fn proxy_mode_round_trips() {
        for m in [
            ProxyMode::Direct,
            ProxyMode::ProxyAuthorization,
            ProxyMode::Basic,
        ] {
            assert_eq!(ProxyMode::from_u8(m.to_u8()), m);
        }
    }

    #[test]
    fn basic_only_travels_over_tls_or_loopback() {
        for ok in [
            "https://forge.example/api/v1/",
            "http://localhost:3000/api/v1/",
            "http://127.0.0.1:8080/api/v1/",
            "http://[::1]:8080/api/v1/",
        ] {
            assert!(transport_is_safe(&Url::parse(ok).unwrap()), "{ok}");
        }
        for bad in ["http://forge.example/api/v1/", "http://10.0.0.5/api/v1/"] {
            assert!(!transport_is_safe(&Url::parse(bad).unwrap()), "{bad}");
        }
    }

    #[test]
    fn errors_name_the_proxy_and_carry_no_secret() {
        let c = cred();
        let all = [
            missing_credential_error("forge.example", StatusCode::UNAUTHORIZED).to_string(),
            rejected_credential_error("forge.example", StatusCode::UNAUTHORIZED).to_string(),
            unsafe_transport_error("forge.example").to_string(),
            forge_rejected_error("forge.example", PatTransport::Header).to_string(),
            forge_rejected_error("forge.example", PatTransport::Query).to_string(),
        ];
        for msg in &all {
            assert!(msg.contains("proxy"), "{msg}");
            assert!(msg.contains("forge.example"), "{msg}");
            assert!(!msg.contains(&c.password), "{msg}");
        }
        assert!(all[0].contains("not your forge token"));
        assert!(all[1].contains("wrong or has expired"));
        assert!(all[1].contains("forge token was not the problem"));
    }

    #[test]
    fn redact_url_hides_query_tokens_and_userinfo() {
        let url = Url::parse(
            "https://u:p@forge.example/api/v1/user?page=2&token=sekrit-pat&access_token=other",
        )
        .unwrap();
        let out = redact_url(&url);
        assert!(!out.contains("sekrit-pat"), "{out}");
        assert!(!out.contains("other"), "{out}");
        assert!(!out.contains("u:p@"), "{out}");
        assert!(out.contains("page=2"), "{out}");
        assert!(out.contains("token=%5Bredacted%5D"), "{out}");
    }

    #[test]
    fn redact_url_leaves_plain_urls_alone() {
        let url = Url::parse("https://forge.example/api/v1/repos/o/r/issues?state=open").unwrap();
        assert_eq!(redact_url(&url), url.as_str());
        let bare = Url::parse("https://forge.example/api/v1/user").unwrap();
        assert_eq!(redact_url(&bare), bare.as_str());
    }

    #[test]
    fn pat_transport_defaults_to_header() {
        assert_eq!(PatTransport::default(), PatTransport::Header);
        assert!(PatTransport::Header.is_default());
        assert_eq!(PatTransport::Query.as_str(), "query");
    }
}
