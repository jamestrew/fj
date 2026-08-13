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
    pub token: Option<String>,
}

/// Look up the host config and token for the host the user is targeting.
pub fn resolve(host_flag: Option<&str>) -> Result<ResolvedHost> {
    let hosts = Hosts::load()?;
    let (name, host_cfg) = if let Some(name) = host_flag {
        let host_cfg = hosts.hosts.get(name).cloned().unwrap_or_else(|| Host {
            git_protocol: "https".into(),
            ..Default::default()
        });
        (name.to_string(), host_cfg)
    } else {
        let name = hosts.resolve_host(None)?.to_string();
        let host_cfg = hosts
            .hosts
            .get(&name)
            .cloned()
            .ok_or_else(|| anyhow!("host '{name}' not configured"))?;
        (name, host_cfg)
    };
    let allow_generic_env = generic_env_allowed_for_resolved_host(&hosts, &name, host_flag);
    let token = match host_cfg.auth_kind {
        AuthKind::Pat => pat_token_for_resolved_host(&name, allow_generic_env)?,
        AuthKind::FjordAccount => {
            let fa = host_cfg.fjord_account.as_ref().ok_or_else(|| {
                anyhow!(
                    "host '{name}' is Fjord-Account but has no fjord_account config; \
                     remove and re-run `fj auth login --fjord`"
                )
            })?;
            Some(auth::load_fjord_bearer(&fa.platform_url)?.ok_or_else(|| {
                anyhow!(
                    "no Fjord Account session stored for '{}'. Run `fj auth login --fjord` again.",
                    fa.platform_url
                )
            })?)
        }
    };
    Ok(ResolvedHost {
        name,
        host: host_cfg,
        token,
    })
}

fn generic_env_allowed_for_resolved_host(
    hosts: &Hosts,
    name: &str,
    host_flag: Option<&str>,
) -> bool {
    if host_flag.is_none() {
        return true;
    }
    matches!(hosts.resolve_host(None), Ok(default) if default == name)
}

fn pat_token_for_resolved_host(name: &str, allow_generic_env: bool) -> Result<Option<String>> {
    if let Some(token) = auth::env_token_for_host(name) {
        return Ok(Some(token));
    }
    if allow_generic_env {
        if let Some(token) = auth::env_token() {
            return Ok(Some(token));
        }
    }
    auth::load_token(name)
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
    use super::{build_base_url, resolve, web_base_url};
    use crate::config::hosts::{AuthKind, FjordAccount, Host};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    struct EnvRestore {
        key: String,
        old: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        fn set(key: impl Into<String>, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let key = key.into();
            let old = std::env::var_os(&key);
            std::env::set_var(&key, value);
            Self { key, old }
        }

        fn remove(key: impl Into<String>) -> Self {
            let key = key.into();
            let old = std::env::var_os(&key);
            std::env::remove_var(&key);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    fn write_hosts_config(config_home: &std::path::Path, current: &str) {
        let dir = config_home.join("fj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hosts.toml"),
            format!(
                r#"current = "{current}"

[hosts."rasterhub.com"]
git_protocol = "https"

[hosts."commons.fjord.host"]
git_protocol = "https"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn default_host_can_use_generic_env_token_without_reading_token_store() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_hosts_config(dir.path(), "rasterhub.com");

        let _config = EnvRestore::set("XDG_CONFIG_HOME", dir.path());
        let _token = EnvRestore::set("FJ_TOKEN", "env-token");

        let resolved = resolve(None).expect("default host may use generic FJ_TOKEN");
        assert_eq!(resolved.name, "rasterhub.com");
        assert_eq!(resolved.token.as_deref(), Some("env-token"));
    }

    #[test]
    fn explicit_host_ignores_generic_env_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_hosts_config(dir.path(), "rasterhub.com");

        let _config = EnvRestore::set("XDG_CONFIG_HOME", dir.path());
        let _token = EnvRestore::set("FJ_TOKEN", "env-token");
        let _host_token = EnvRestore::remove(crate::auth::host_env_token_var("commons.fjord.host"));

        let resolved = resolve(Some("commons.fjord.host"))
            .expect("explicit configured host should still resolve");
        assert_eq!(resolved.name, "commons.fjord.host");
        assert_eq!(resolved.token, None);
    }

    #[test]
    fn explicit_default_host_can_use_generic_env_token_without_reading_token_store() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_hosts_config(dir.path(), "rasterhub.com");

        let _config = EnvRestore::set("XDG_CONFIG_HOME", dir.path());
        let _token = EnvRestore::set("FJ_TOKEN", "env-token");
        let _host_token = EnvRestore::remove(crate::auth::host_env_token_var("rasterhub.com"));

        let resolved =
            resolve(Some("rasterhub.com")).expect("explicit default host may use generic FJ_TOKEN");
        assert_eq!(resolved.name, "rasterhub.com");
        assert_eq!(resolved.token.as_deref(), Some("env-token"));
    }

    #[test]
    fn explicit_host_can_use_host_scoped_env_token_without_reading_token_store() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("config-file");
        std::fs::write(&not_a_dir, "not a directory").unwrap();

        let _config = EnvRestore::set("XDG_CONFIG_HOME", &not_a_dir);
        let _token = EnvRestore::set("FJ_TOKEN", "wrong-host-token");
        let _host_token = EnvRestore::set(
            crate::auth::host_env_token_var("git.example.com"),
            "host-env-token",
        );

        let resolved = resolve(Some("git.example.com"))
            .expect("host-scoped env token should avoid store read");
        assert_eq!(resolved.name, "git.example.com");
        assert_eq!(resolved.token.as_deref(), Some("host-env-token"));
    }
}
