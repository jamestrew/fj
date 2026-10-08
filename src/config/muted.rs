//! Locally muted repositories for the notifications inbox (`fj status`).
//!
//! A mute is a client-side filter only. It never touches the server's watch
//! state, so a muted repository keeps producing notifications and keeps
//! counting toward the server unread count. It is not a secret, so it lives
//! next to `hosts.toml` in the config directory, not in the keychain.
//!
//! The list is keyed by account scope (see [`account_scope`]) so two accounts
//! never share one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::config_dir;
use super::hosts::{AuthKind, Host, Hosts};

pub fn muted_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("muted_repos.toml"))
}

/// Top-level shape of `muted_repos.toml`: account scope -> repository full
/// names, stored as the user typed them and compared case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutedRepos {
    #[serde(default)]
    pub accounts: BTreeMap<String, Vec<String>>,
}

impl MutedRepos {
    pub fn load() -> Result<Self> {
        Self::load_from(&muted_path()?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&muted_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing muted_repos.toml")?;
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// The muted repositories for one account scope, in stored (sorted) order.
    pub fn list(&self, scope: &str) -> &[String] {
        self.accounts.get(scope).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_muted(&self, scope: &str, full_name: &str) -> bool {
        self.list(scope)
            .iter()
            .any(|r| r.eq_ignore_ascii_case(full_name))
    }

    /// Mute `full_name` for `scope`. Returns `false` when it was already muted
    /// (under any casing), in which case nothing changes.
    pub fn mute(&mut self, scope: &str, full_name: &str) -> bool {
        if self.is_muted(scope, full_name) {
            return false;
        }
        let repos = self.accounts.entry(scope.to_string()).or_default();
        repos.push(full_name.to_string());
        repos.sort_by_key(|r| r.to_ascii_lowercase());
        true
    }

    /// Unmute `full_name` for `scope`. Returns the stored spelling that was
    /// removed, or `None` when it was not muted.
    pub fn unmute(&mut self, scope: &str, full_name: &str) -> Option<String> {
        let repos = self.accounts.get_mut(scope)?;
        let at = repos
            .iter()
            .position(|r| r.eq_ignore_ascii_case(full_name))?;
        let removed = repos.remove(at);
        if repos.is_empty() {
            self.accounts.remove(scope);
        }
        Some(removed)
    }
}

/// The key a mute list is stored under for a host entry.
///
/// A PAT host is one account on one forge, so the hostname is the scope. A
/// Fjord Account host fronts several forge instances behind one hostname, and
/// `owner/repo` on one instance is a different repository from the same name
/// on another, so the selected instance is part of the scope. Switching with
/// `fj instances use` therefore switches mute lists too.
pub fn scope_key(name: &str, host: Option<&Host>) -> Result<String> {
    let Some(host) = host.filter(|h| h.auth_kind == AuthKind::FjordAccount) else {
        return Ok(name.to_string());
    };
    let instance = host
        .fjord_account
        .as_ref()
        .and_then(|fa| fa.default_instance_id.as_deref())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "no default Fjord instance selected. \
                 Run `fj instances list` and then `fj instances use <id>`."
            )
        })?;
    Ok(format!("{name}#{instance}"))
}

/// Resolve the account scope for the host a command targets, with the same
/// precedence as the API client: an explicit `--host`, else the configured
/// default. Reads only `hosts.toml` (no token, no network).
pub fn account_scope(host_flag: Option<&str>) -> Result<String> {
    let hosts = Hosts::load()?;
    let name = match host_flag {
        Some(name) => name,
        None => hosts.resolve_host(None)?,
    };
    scope_key(name, hosts.hosts.get(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::hosts::FjordAccount;

    fn fjord_host(instance: Option<&str>) -> Host {
        Host {
            auth_kind: AuthKind::FjordAccount,
            fjord_account: Some(FjordAccount {
                platform_url: "https://p.example".into(),
                default_instance_id: instance.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn mute_matches_case_insensitively_and_dedupes() {
        let mut m = MutedRepos::default();
        assert!(m.mute("h", "Acme/Widgets"));
        assert!(!m.mute("h", "acme/widgets"), "same repo, other casing");
        assert_eq!(m.list("h"), ["Acme/Widgets"]);
        assert!(m.is_muted("h", "ACME/WIDGETS"));
        assert!(!m.is_muted("h", "acme/other"));
    }

    #[test]
    fn mutes_are_scoped_to_one_account() {
        let mut m = MutedRepos::default();
        m.mute("a.example", "o/r");
        assert!(m.is_muted("a.example", "o/r"));
        assert!(!m.is_muted("b.example", "o/r"));
        assert!(m.list("b.example").is_empty());
    }

    #[test]
    fn unmute_returns_stored_spelling_and_prunes_empty_scope() {
        let mut m = MutedRepos::default();
        m.mute("h", "Acme/Widgets");
        assert_eq!(m.unmute("h", "nope/nope"), None);
        assert_eq!(m.unmute("other", "acme/widgets"), None);
        assert_eq!(
            m.unmute("h", "acme/widgets").as_deref(),
            Some("Acme/Widgets")
        );
        assert!(!m.is_muted("h", "Acme/Widgets"));
        assert!(m.accounts.is_empty());
    }

    #[test]
    fn list_is_sorted_ignoring_case() {
        let mut m = MutedRepos::default();
        m.mute("h", "zeta/z");
        m.mute("h", "Alpha/a");
        m.mute("h", "beta/b");
        assert_eq!(m.list("h"), ["Alpha/a", "beta/b", "zeta/z"]);
    }

    #[test]
    fn persists_across_load_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("muted_repos.toml");
        assert_eq!(MutedRepos::load_from(&path).unwrap(), MutedRepos::default());

        let mut m = MutedRepos::default();
        m.mute("forge.example", "Acme/Widgets");
        m.mute("fjord.example#inst_1", "o/r");
        m.save_to(&path).unwrap();

        let back = MutedRepos::load_from(&path).unwrap();
        assert_eq!(back, m);
        assert!(back.is_muted("forge.example", "acme/widgets"));
        assert!(!back.is_muted("forge.example", "o/r"));
    }

    #[test]
    fn load_reports_a_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("muted_repos.toml");
        fs::write(&path, "accounts = 3").unwrap();
        let err = MutedRepos::load_from(&path).unwrap_err();
        assert!(format!("{err:#}").contains("parsing"), "{err:#}");
    }

    #[test]
    fn scope_key_is_the_hostname_for_pat_and_unconfigured_hosts() {
        assert_eq!(scope_key("forge.example", None).unwrap(), "forge.example");
        let pat = Host::default();
        assert_eq!(
            scope_key("forge.example", Some(&pat)).unwrap(),
            "forge.example"
        );
    }

    #[test]
    fn scope_key_includes_the_selected_fjord_instance() {
        let a = scope_key("fjord.example", Some(&fjord_host(Some("inst_a")))).unwrap();
        let b = scope_key("fjord.example", Some(&fjord_host(Some("inst_b")))).unwrap();
        assert_eq!(a, "fjord.example#inst_a");
        assert_ne!(a, b);
    }

    #[test]
    fn scope_key_refuses_a_fjord_host_with_no_instance() {
        let err = scope_key("fjord.example", Some(&fjord_host(None))).unwrap_err();
        assert!(err.to_string().contains("fj instances use"), "{err}");
    }
}
