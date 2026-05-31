//! Host resolution: turn the user's `--host` flag (or the configured default)
//! into the host config, its stored token, and the API base URL. This is the
//! "where do I connect, and as whom" step, kept separate from the request
//! funnel in `client::mod`.

use anyhow::{anyhow, Context, Result};
use url::Url;

use crate::auth;
use crate::config::hosts::{api_base_path, AuthKind, Host, Hosts};

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
    let token = match host_cfg.auth_kind {
        AuthKind::Pat => auth::load_token(&name)?.ok_or_else(|| {
            anyhow!("no token stored for host '{name}'. Run `fj auth login --host {name}`.")
        })?,
        AuthKind::FjordAccount => {
            let fa = host_cfg.fjord_account.as_ref().ok_or_else(|| {
                anyhow!(
                    "host '{name}' is Fjord-Account but has no fjord_account config; \
                     remove and re-run `fj auth login --fjord`"
                )
            })?;
            auth::load_fjord_bearer(&fa.platform_url)?.ok_or_else(|| {
                anyhow!(
                    "no Fjord Account session stored for '{}'. Run `fj auth login --fjord` again.",
                    fa.platform_url
                )
            })?
        }
    };
    Ok(ResolvedHost {
        name,
        host: host_cfg,
        token,
    })
}

/// Construct the `/api/v1`-rooted base URL for a PAT host. Always https; the
/// API base path defaults to `/api/v1` but can be overridden per host.
pub(crate) fn build_base_url(hostname: &str, host: &Host) -> Result<Url> {
    let scheme = "https";
    let mut url = Url::parse(&format!("{scheme}://{hostname}"))
        .with_context(|| format!("constructing URL for {hostname}"))?;
    let path = api_base_path(host).trim_end_matches('/');
    url.set_path(&format!("{path}/"));
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::build_base_url;
    use crate::config::hosts::Host;

    fn host_with_api_base(api: Option<&str>) -> Host {
        Host {
            api_base_path: api.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn build_base_url_defaults_to_api_v1_with_trailing_slash() {
        let url = build_base_url("git.example.com", &host_with_api_base(None)).unwrap();
        assert_eq!(url.as_str(), "https://git.example.com/api/v1/");
    }

    #[test]
    fn build_base_url_honors_a_custom_api_base() {
        let url = build_base_url("git.example.com", &host_with_api_base(Some("/api/v2"))).unwrap();
        assert_eq!(url.as_str(), "https://git.example.com/api/v2/");
    }

    #[test]
    fn build_base_url_normalizes_a_trailing_slash_in_the_config() {
        let url = build_base_url("git.example.com", &host_with_api_base(Some("/api/v1/"))).unwrap();
        assert_eq!(url.as_str(), "https://git.example.com/api/v1/");
    }
}
