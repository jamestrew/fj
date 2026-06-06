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

/// Construct the `/api/v1`-rooted base URL for a PAT host. Older configs only
/// have a host key, which defaults to HTTPS. Newer configs can carry an
/// explicit web URL so local HTTP, ports, and path-mounted instances survive
/// login and refresh.
pub(crate) fn build_base_url(hostname: &str, host: &Host) -> Result<Url> {
    let base = host
        .web_url
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://{hostname}"));
    let mut url = Url::parse(&base).with_context(|| format!("constructing URL for {base}"))?;
    let mut path = url.path().trim_end_matches('/').to_string();
    let api = api_base_path(host).trim_matches('/');
    if !api.is_empty() {
        path.push('/');
        path.push_str(api);
    }
    path.push('/');
    url.set_path(&path);
    Ok(url)
}

/// Return the browser origin/base for a resolved client host. For PAT hosts,
/// this is the configured web URL or the legacy `https://<host key>` default.
/// Fjord-Account hosts resolve to the selected instance's public URL when the
/// instances command has cached it; otherwise browser-only shortcuts cannot be
/// built without querying platform metadata.
pub fn web_base_url(name: &str, host: &Host) -> Result<Option<Url>> {
    match host.auth_kind {
        AuthKind::Pat => {
            let base = host
                .web_url
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://{name}"));
            let mut url =
                Url::parse(&base).with_context(|| format!("constructing web URL for {base}"))?;
            let path = web_base_path(url.path());
            url.set_path(&path);
            Ok(Some(url))
        }
        AuthKind::FjordAccount => host
            .fjord_account
            .as_ref()
            .and_then(|fa| fa.default_instance_public_url.as_deref())
            .map(|base| {
                let mut url =
                    Url::parse(base).with_context(|| format!("constructing web URL for {base}"))?;
                let path = web_base_path(url.path());
                url.set_path(&path);
                Ok(url)
            })
            .transpose(),
    }
}

fn web_base_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        format!("{trimmed}/")
    }
}

#[cfg(test)]
mod tests {
    use super::{build_base_url, web_base_url};
    use crate::config::hosts::{AuthKind, FjordAccount, Host};

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

    #[test]
    fn build_base_url_honors_web_url_scheme_port_and_path() {
        let mut host = host_with_api_base(None);
        host.web_url = Some("http://localhost:3000/forgejo".into());
        let url = build_base_url("localhost:3000/forgejo", &host).unwrap();
        assert_eq!(url.as_str(), "http://localhost:3000/forgejo/api/v1/");
    }

    #[test]
    fn web_base_url_keeps_mount_path_joinable() {
        let mut host = host_with_api_base(None);
        host.web_url = Some("http://localhost:3000/forgejo".into());
        let url = web_base_url("localhost:3000/forgejo", &host)
            .unwrap()
            .unwrap();
        assert_eq!(
            url.join("owner/repo/pulls").unwrap().as_str(),
            "http://localhost:3000/forgejo/owner/repo/pulls"
        );
    }

    #[test]
    fn web_base_url_uses_cached_fjord_instance_url() {
        let host = Host {
            auth_kind: AuthKind::FjordAccount,
            fjord_account: Some(FjordAccount {
                default_instance_public_url: Some("https://forge.example/base".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let url = web_base_url("https://platform.example", &host)
            .unwrap()
            .unwrap();
        assert_eq!(
            url.join("owner/repo/issues").unwrap().as_str(),
            "https://forge.example/base/owner/repo/issues"
        );
    }
}
