//! Host resolution: turn the user's `--host` flag (or the configured default)
//! into the host config, its stored token, and the API base URL. This is the
//! "where do I connect, and as whom" step, kept separate from the request
//! funnel in `client::mod`.

use anyhow::{anyhow, Context, Result};
use url::Url;

use super::proxy::ProxyCredential;
use crate::auth;
use crate::config::hosts::{api_base_path, AuthKind, Host, Hosts};

pub struct ResolvedHost {
    pub name: String,
    pub host: Host,
    pub token: Option<String>,
    /// Credential for a reverse proxy in front of the host, when one is
    /// configured. Separate from `token` by construction.
    pub proxy: Option<ProxyCredential>,
}

/// Assemble the proxy credential for `name` from the environment and the
/// stored config. Each half resolves independently: the environment wins, then
/// hosts.toml (username) and the token store (password). `None` unless both
/// halves are present, so a half-configured credential is never sent.
pub fn proxy_credential_for_host(
    name: &str,
    host: &Host,
    allow_generic_env: bool,
) -> Result<Option<ProxyCredential>> {
    if host.auth_kind != AuthKind::Pat {
        return Ok(None);
    }
    let username = auth::env_proxy_user(name, allow_generic_env)
        .or_else(|| host.proxy.as_ref().map(|p| p.username.clone()))
        .filter(|u| !u.is_empty());
    let Some(username) = username else {
        return Ok(None);
    };
    let password = match auth::env_proxy_password(name, allow_generic_env) {
        Some(p) => Some(p),
        None if host.proxy.is_some() => auth::load_proxy_password(name)?,
        None => None,
    };
    Ok(password.map(|password| ProxyCredential {
        username,
        password,
        pat_transport: host
            .proxy
            .as_ref()
            .map(|p| p.pat_transport)
            .unwrap_or_default(),
    }))
}

/// Look up the host config and token for the host the user is targeting.
pub fn resolve(host_flag: Option<&str>) -> Result<ResolvedHost> {
    resolve_with(&Hosts::load()?, host_flag)
}

/// [`resolve`] against a configuration the caller already loaded. A command
/// that routes, connects and renders from one `Hosts` value cannot see a
/// second terminal's `auth switch` half way through (CLS-1264).
pub fn resolve_with(hosts: &Hosts, host_flag: Option<&str>) -> Result<ResolvedHost> {
    let stored = host_flag
        .map(|n| crate::config::metadata::existing_key(hosts, n))
        .transpose()?
        .flatten();
    let host_flag = stored.as_deref().or(host_flag);
    let (name, mut host_cfg) = if let Some(name) = host_flag {
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
    let allow_generic_env = crate::config::profiles::generic_env(hosts, &name, host_flag);
    let token = super::account::resolve_token(hosts, &name, &mut host_cfg, allow_generic_env)?;
    let proxy = proxy_credential_for_host(&name, &host_cfg, allow_generic_env)?;
    Ok(ResolvedHost {
        name,
        host: host_cfg,
        token,
        proxy,
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
    use super::{build_base_url, resolve, resolve_with, web_base_url};
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

    /// A configuration with two accounts on one host and `default` selected.
    fn write_accounts_config(config_home: &std::path::Path, default: &str, protocol: &str) {
        let dir = config_home.join("fj");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hosts.toml"),
            format!(
                r#"current = "forge.example"

[hosts."forge.example"]
git_protocol = "{protocol}"
web_url = "https://forge.example"

[accounts."forge.example"]
default_account = "{default}"

[accounts."forge.example".profiles.alice]
user = "alice"
generation = "g1"

[accounts."forge.example".profiles.bob]
user = "bob"
generation = "g2"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn resolve_with_uses_the_snapshot_not_the_files_on_disk() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_accounts_config(dir.path(), "alice", "ssh");
        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
        let _token = EnvRestore::set(
            crate::auth::host_env_token_var("forge.example"),
            "env-token",
        );

        let snapshot = crate::config::hosts::Hosts::load().unwrap();
        // Another terminal switches account and protocol after the snapshot.
        write_accounts_config(dir.path(), "bob", "https");

        let seen = resolve_with(&snapshot, None).unwrap();
        assert_eq!(seen.name, "forge.example");
        assert_eq!(
            seen.host.selected_account.as_ref().map(|a| a.as_str()),
            Some("alice"),
            "the snapshot's account, not the switched one"
        );
        assert_eq!(seen.host.git_protocol, "ssh");

        let fresh = resolve(None).unwrap();
        assert_eq!(
            fresh.host.selected_account.as_ref().map(|a| a.as_str()),
            Some("bob"),
            "a new command sees the switch"
        );
    }

    #[test]
    fn a_connected_client_keeps_the_configuration_it_connected_with() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_accounts_config(dir.path(), "alice", "ssh");
        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
        let _token = EnvRestore::set(
            crate::auth::host_env_token_var("forge.example"),
            "env-token",
        );

        let snapshot = crate::config::hosts::Hosts::load().unwrap();
        let client = crate::client::Client::connect_with(&snapshot, None).unwrap();
        write_accounts_config(dir.path(), "bob", "https");

        assert_eq!(client.host(), "forge.example");
        assert_eq!(client.config().git_protocol, "ssh");
        assert_eq!(
            client
                .config()
                .selected_account
                .as_ref()
                .map(|a| a.as_str()),
            Some("alice")
        );
        let web = web_base_url(client.host(), client.config())
            .unwrap()
            .unwrap();
        assert_eq!(web.as_str(), "https://forge.example/");
    }

    #[test]
    fn the_inbox_scope_follows_the_connected_client_not_the_disk() {
        use crate::config::muted::{account_scope, client_scope};
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_accounts_config(dir.path(), "alice", "https");
        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
        let _token = EnvRestore::set(
            crate::auth::host_env_token_var("forge.example"),
            "env-token",
        );

        let alice_scope = account_scope(None).unwrap();
        let client = crate::client::Client::connect(None).unwrap();
        // Another terminal switches account after the client connected.
        write_accounts_config(dir.path(), "bob", "https");

        assert_eq!(
            client_scope(&client).unwrap(),
            alice_scope,
            "the connected account's mute list"
        );
        assert_ne!(
            account_scope(None).unwrap(),
            alice_scope,
            "reloading the files after the switch would name the other account"
        );
    }

    #[test]
    fn the_inbox_scope_follows_the_connected_fjord_instance() {
        use crate::config::muted::{client_scope, scope_key};
        let instance = |id: &str| Host {
            auth_kind: AuthKind::FjordAccount,
            fjord_account: Some(FjordAccount {
                platform_url: "https://p.example".into(),
                default_instance_id: Some(id.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let client = |host: Host| {
            crate::client::Client::for_base_url(
                "http://h.example/api/v1/".parse().unwrap(),
                "t".into(),
            )
            .unwrap()
            .with_config(host)
        };
        let one = client_scope(&client(instance("i1"))).unwrap();
        let two = client_scope(&client(instance("i2"))).unwrap();
        assert_ne!(one, two, "each instance has its own mute list");
        assert_eq!(one, scope_key("h.example", Some(&instance("i1"))).unwrap());
    }

    #[test]
    fn default_host_can_use_generic_env_token_without_reading_token_store() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_hosts_config(dir.path(), "rasterhub.com");

        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
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

        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
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

        let _config = EnvRestore::set("FJ_CONFIG_DIR", dir.path().join("fj"));
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
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

        let _config = EnvRestore::set("FJ_CONFIG_DIR", &not_a_dir);
        let _no_keychain = EnvRestore::set("FJ_NO_KEYCHAIN", "1");
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
