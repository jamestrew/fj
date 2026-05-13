use std::collections::BTreeMap;
use std::fs;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::hosts_path;

/// A single Forgejo host entry. The auth token is intentionally NOT serialized
/// here: tokens live in the OS keychain via the `auth` module.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Host {
    /// Username (login) of the authenticated user on this host.
    pub user: Option<String>,
    /// Protocol used for git operations (`https` or `ssh`).
    #[serde(default = "default_git_protocol")]
    pub git_protocol: String,
    /// Optional API base path. Defaults to `/api/v1` when absent.
    pub api_base_path: Option<String>,
}

fn default_git_protocol() -> String {
    "https".into()
}

/// Top-level shape of `hosts.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hosts {
    /// Hostname currently selected as the default for commands that don't pass `--host`.
    pub current: Option<String>,
    /// Map of hostname -> host config.
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
}

impl Hosts {
    pub fn load() -> Result<Self> {
        let path = hosts_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<()> {
        let path = hosts_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing hosts.toml")?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Resolve which hostname to use given an explicit override (e.g. from `--host`).
    pub fn resolve_host<'a>(&'a self, explicit: Option<&'a str>) -> Result<&'a str> {
        if let Some(h) = explicit {
            if !self.hosts.contains_key(h) {
                return Err(anyhow!(
                    "no auth entry for host '{h}'. Run `fj auth login --host {h}` first."
                ));
            }
            return Ok(h);
        }
        if let Some(current) = self.current.as_deref() {
            if self.hosts.contains_key(current) {
                return Ok(current);
            }
        }
        // Fall back to the only host configured, if there is exactly one.
        if self.hosts.len() == 1 {
            return Ok(self.hosts.keys().next().unwrap());
        }
        Err(anyhow!(
            "no host selected. Run `fj auth login` first, or pass `--host <hostname>`."
        ))
    }

    pub fn upsert(&mut self, hostname: &str, host: Host) {
        self.hosts.insert(hostname.to_string(), host);
        if self.current.is_none() {
            self.current = Some(hostname.to_string());
        }
    }

    pub fn remove(&mut self, hostname: &str) -> Option<Host> {
        let removed = self.hosts.remove(hostname);
        if self.current.as_deref() == Some(hostname) {
            self.current = self.hosts.keys().next().cloned();
        }
        removed
    }

    pub fn set_current(&mut self, hostname: &str) -> Result<()> {
        if !self.hosts.contains_key(hostname) {
            return Err(anyhow!("host '{hostname}' is not configured"));
        }
        self.current = Some(hostname.to_string());
        Ok(())
    }
}

/// Returns the `/api/v1` base path for a host, honoring per-host overrides.
pub fn api_base_path(host: &Host) -> &str {
    host.api_base_path.as_deref().unwrap_or("/api/v1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(user: &str) -> Host {
        Host {
            user: Some(user.into()),
            git_protocol: "https".into(),
            api_base_path: None,
        }
    }

    #[test]
    fn upsert_sets_current_on_first_host() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        assert_eq!(h.current.as_deref(), Some("a.example"));
    }

    #[test]
    fn upsert_does_not_steal_current_from_existing() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.upsert("b.example", host("bob"));
        assert_eq!(h.current.as_deref(), Some("a.example"));
    }

    #[test]
    fn remove_drops_current_when_targeted() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.upsert("b.example", host("bob"));
        h.remove("a.example");
        // current was a.example, falls back to surviving host
        assert_eq!(h.current.as_deref(), Some("b.example"));
    }

    #[test]
    fn remove_returns_none_for_missing() {
        let mut h = Hosts::default();
        assert!(h.remove("nope").is_none());
    }

    #[test]
    fn resolve_explicit_must_exist() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        assert!(h.resolve_host(Some("nope")).is_err());
        assert_eq!(h.resolve_host(Some("a.example")).unwrap(), "a.example");
    }

    #[test]
    fn resolve_uses_current() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.upsert("b.example", host("bob"));
        h.set_current("b.example").unwrap();
        assert_eq!(h.resolve_host(None).unwrap(), "b.example");
    }

    #[test]
    fn resolve_falls_back_to_only_host() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.current = None;
        assert_eq!(h.resolve_host(None).unwrap(), "a.example");
    }

    #[test]
    fn resolve_errors_when_ambiguous() {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.upsert("b.example", host("bob"));
        h.current = None;
        assert!(h.resolve_host(None).is_err());
    }

    #[test]
    fn set_current_rejects_unknown() {
        let mut h = Hosts::default();
        assert!(h.set_current("nope").is_err());
    }

    #[test]
    fn api_base_path_defaults() {
        let mut h = host("alice");
        assert_eq!(api_base_path(&h), "/api/v1");
        h.api_base_path = Some("/api/v2".into());
        assert_eq!(api_base_path(&h), "/api/v2");
    }
}
