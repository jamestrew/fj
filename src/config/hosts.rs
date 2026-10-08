use std::collections::BTreeMap;
use std::fs;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::hosts_path;

/// A single Forgejo host entry. The auth token is intentionally NOT serialized
/// here: tokens live in the `auth` module stores, not hosts.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Host {
    /// Username (login) of the authenticated user on this host. For Fjord
    /// Account hosts this is the Fjord email.
    pub user: Option<String>,
    /// Protocol used for git operations (`https` or `ssh`).
    #[serde(default = "default_git_protocol")]
    pub git_protocol: String,
    /// Base URL of the Forgejo web origin for PAT-mode hosts. Defaults to
    /// `https://<host key>` for older configs and simple hostname logins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// Optional API base path. Defaults to `/api/v1` when absent.
    pub api_base_path: Option<String>,
    /// How this host authenticates. `pat` (default) means the auth token store
    /// holds a Forgejo personal access token and requests go direct to
    /// the host. `fjord_account` means the auth token store holds a Fjord
    /// Account bearer and requests are routed through the
    /// fjord-platform gateway.
    #[serde(default, skip_serializing_if = "AuthKind::is_default")]
    pub auth_kind: AuthKind,
    /// Fjord-Account-specific config. Present only when
    /// `auth_kind == FjordAccount`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fjord_account: Option<FjordAccount>,
}

/// How a host authenticates fj's requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    #[default]
    Pat,
    FjordAccount,
}

impl AuthKind {
    fn is_default(&self) -> bool {
        matches!(self, AuthKind::Pat)
    }
}

/// Per-host Fjord Account configuration. The `platform_url` is the
/// fjord-platform base URL the bearer was minted against; the default
/// instance id is the Forgejo instance routed to when the user doesn't
/// pass an explicit `--instance`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FjordAccount {
    /// Base URL of the fjord-platform deployment, e.g.
    /// `https://forgejo-site.pages.dev`. No trailing slash.
    pub platform_url: String,
    /// Currently-selected Forgejo instance id. `None` means the user
    /// has signed in but not yet picked one.
    pub default_instance_id: Option<String>,
    /// Cached display name + slug for the default instance, so commands
    /// can print it without re-fetching.
    pub default_instance_display_name: Option<String>,
    pub default_instance_slug: Option<String>,
    /// Cached browser URL for the default instance. Used by `--web` shortcuts
    /// that need the actual Forgejo origin rather than the Fjord platform URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_instance_public_url: Option<String>,
    /// Which sign-in flow minted this session: `"oidc"` (SSO, refresh-token in
    /// the keychain) or `"credentials"` (email + password). Absent on entries
    /// written before the field existed; treated as `"credentials"` for
    /// display. Purely informational: refresh keys off the presence of a stored
    /// refresh token, not this marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_flow: Option<String>,
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
    /// User-named groups: group name -> the hosts assigned to it. A purely
    /// organizational label; nothing about auth or host resolution reads it.
    /// A host appears under at most one group (the mutators below keep that
    /// true). Absent from the file until the first group is created, so a
    /// config that never used groups is written exactly as before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<String, Vec<String>>,
}

/// Longest group name accepted. Generous for a label, small enough that it
/// cannot wreck the `fj auth list` table.
const MAX_GROUP_NAME_LEN: usize = 64;

/// Normalize and validate a user-supplied group name: surrounding whitespace
/// is dropped, and the result must be non-empty, of sane length, and free of
/// control characters (it is printed in tables and written as a TOML key).
pub fn normalize_group_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(anyhow!("group name is required"));
    }
    if name.chars().count() > MAX_GROUP_NAME_LEN {
        return Err(anyhow!(
            "group name is too long (at most {MAX_GROUP_NAME_LEN} characters)"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(anyhow!("group name must not contain control characters"));
    }
    Ok(name.to_string())
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
        // A removed host leaves no assignment behind for a later host of the
        // same name to inherit. The group itself stays.
        self.unassign_group(hostname);
        removed
    }

    /// The group `hostname` is assigned to, if any.
    pub fn group_of(&self, hostname: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|(_, members)| members.iter().any(|m| m == hostname))
            .map(|(name, _)| name.as_str())
    }

    /// Create an empty group. Errors if one of that name already exists.
    pub fn create_group(&mut self, name: &str) -> Result<String> {
        let name = normalize_group_name(name)?;
        if self.groups.contains_key(&name) {
            return Err(anyhow!("group '{name}' already exists"));
        }
        self.groups.insert(name.clone(), Vec::new());
        Ok(name)
    }

    /// Rename a group, keeping its assignments. Returns the new name.
    pub fn rename_group(&mut self, old: &str, new: &str) -> Result<String> {
        let old = old.trim();
        let new = normalize_group_name(new)?;
        if !self.groups.contains_key(old) {
            return Err(anyhow!("no group named '{old}'"));
        }
        if old == new {
            return Ok(new);
        }
        if self.groups.contains_key(&new) {
            return Err(anyhow!("group '{new}' already exists"));
        }
        let members = self.groups.remove(old).unwrap_or_default();
        self.groups.insert(new.clone(), members);
        Ok(new)
    }

    /// Delete a group and clear its assignments. The hosts themselves are
    /// untouched. Returns the hosts that were in it, now ungrouped.
    pub fn delete_group(&mut self, name: &str) -> Result<Vec<String>> {
        let name = name.trim();
        self.groups
            .remove(name)
            .ok_or_else(|| anyhow!("no group named '{name}'"))
    }

    /// Assign a configured host to an existing group, replacing any prior
    /// assignment so the host belongs to exactly one. Returns the group the
    /// host was in before, when that was a different one.
    pub fn assign_group(&mut self, hostname: &str, group: &str) -> Result<Option<String>> {
        let group = group.trim();
        if !self.hosts.contains_key(hostname) {
            return Err(anyhow!("host '{hostname}' is not configured"));
        }
        if !self.groups.contains_key(group) {
            return Err(anyhow!(
                "no group named '{group}'. Create it with `fj auth group create`."
            ));
        }
        let previous = self.unassign_group(hostname).filter(|p| p != group);
        if let Some(members) = self.groups.get_mut(group) {
            members.push(hostname.to_string());
            members.sort();
        }
        Ok(previous)
    }

    /// Remove `hostname` from whatever group holds it. Returns that group.
    /// Clears every occurrence, so a hand-edited file listing a host under
    /// two groups is repaired rather than half-fixed.
    pub fn unassign_group(&mut self, hostname: &str) -> Option<String> {
        let mut previous = None;
        for (name, members) in self.groups.iter_mut() {
            let before = members.len();
            members.retain(|m| m != hostname);
            if members.len() != before && previous.is_none() {
                previous = Some(name.clone());
            }
        }
        previous
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
            web_url: None,
            api_base_path: None,
            auth_kind: AuthKind::Pat,
            fjord_account: None,
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

    fn two_hosts() -> Hosts {
        let mut h = Hosts::default();
        h.upsert("a.example", host("alice"));
        h.upsert("b.example", host("bob"));
        h
    }

    #[test]
    fn group_create_assign_and_lookup() {
        let mut h = two_hosts();
        assert_eq!(h.create_group("  work ").unwrap(), "work");
        assert_eq!(h.assign_group("a.example", "work").unwrap(), None);
        assert_eq!(h.group_of("a.example"), Some("work"));
        assert_eq!(h.group_of("b.example"), None);
    }

    #[test]
    fn assigning_replaces_the_prior_assignment() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.create_group("personal").unwrap();
        h.assign_group("a.example", "work").unwrap();

        let previous = h.assign_group("a.example", "personal").unwrap();

        assert_eq!(previous.as_deref(), Some("work"));
        assert_eq!(h.group_of("a.example"), Some("personal"));
        assert!(h.groups["work"].is_empty());
        // Re-assigning to the same group is a no-op, not a duplicate entry.
        assert_eq!(h.assign_group("a.example", "personal").unwrap(), None);
        assert_eq!(h.groups["personal"], vec!["a.example".to_string()]);
    }

    #[test]
    fn assign_rejects_an_unknown_host_or_group() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        assert!(h.assign_group("nope.example", "work").is_err());
        assert!(h.assign_group("a.example", "nope").is_err());
        assert_eq!(h.group_of("a.example"), None);
    }

    #[test]
    fn create_rejects_duplicates_and_bad_names() {
        let mut h = Hosts::default();
        h.create_group("work").unwrap();
        assert!(h.create_group("work").is_err());
        assert!(h.create_group("   ").is_err());
        assert!(h.create_group("a\nb").is_err());
        assert!(h.create_group(&"x".repeat(65)).is_err());
    }

    #[test]
    fn rename_keeps_assignments() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.create_group("other").unwrap();
        h.assign_group("a.example", "work").unwrap();

        assert_eq!(h.rename_group("work", "clients").unwrap(), "clients");

        assert_eq!(h.group_of("a.example"), Some("clients"));
        assert!(!h.groups.contains_key("work"));
        assert!(h.rename_group("work", "x").is_err());
        assert!(h.rename_group("clients", "other").is_err());
        assert_eq!(h.rename_group("clients", "clients").unwrap(), "clients");
    }

    #[test]
    fn deleting_a_group_clears_assignments_and_leaves_hosts_alone() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.assign_group("a.example", "work").unwrap();
        h.assign_group("b.example", "work").unwrap();
        h.set_current("b.example").unwrap();

        let freed = h.delete_group("work").unwrap();

        assert_eq!(
            freed,
            vec!["a.example".to_string(), "b.example".to_string()]
        );
        assert_eq!(h.group_of("a.example"), None);
        assert_eq!(h.group_of("b.example"), None);
        assert_eq!(h.hosts.len(), 2);
        assert_eq!(h.hosts["a.example"].user.as_deref(), Some("alice"));
        assert_eq!(h.current.as_deref(), Some("b.example"));
        assert!(h.delete_group("work").is_err());
    }

    #[test]
    fn grouping_does_not_affect_host_resolution() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.assign_group("b.example", "work").unwrap();
        assert_eq!(h.resolve_host(None).unwrap(), "a.example");
        assert_eq!(h.resolve_host(Some("b.example")).unwrap(), "b.example");
    }

    #[test]
    fn removing_a_host_drops_its_assignment_but_keeps_the_group() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.assign_group("a.example", "work").unwrap();
        h.remove("a.example");
        assert!(h.groups.contains_key("work"));
        assert!(h.groups["work"].is_empty());
    }

    #[test]
    fn re_login_upsert_keeps_the_group_assignment() {
        let mut h = two_hosts();
        h.create_group("work").unwrap();
        h.assign_group("a.example", "work").unwrap();
        h.upsert("a.example", host("alice2"));
        assert_eq!(h.group_of("a.example"), Some("work"));
    }

    #[test]
    fn unassign_repairs_a_host_listed_under_two_groups() {
        let mut h: Hosts = toml::from_str(
            r#"
            [hosts."a.example"]
            user = "alice"
            [groups]
            one = ["a.example"]
            two = ["a.example"]
            "#,
        )
        .unwrap();
        assert!(h.unassign_group("a.example").is_some());
        assert_eq!(h.group_of("a.example"), None);
    }

    #[test]
    fn a_config_without_groups_loads_and_writes_no_groups_key() {
        let old = r#"
            current = "a.example"
            [hosts."a.example"]
            user = "alice"
            git_protocol = "https"
        "#;
        let h: Hosts = toml::from_str(old).expect("pre-groups config still loads");
        assert!(h.groups.is_empty());
        let written = toml::to_string_pretty(&h).unwrap();
        assert!(!written.contains("groups"), "{written}");
    }

    #[test]
    fn groups_survive_a_save_and_load_round_trip() {
        let mut h = two_hosts();
        h.create_group("client work").unwrap();
        h.create_group("empty").unwrap();
        h.assign_group("b.example", "client work").unwrap();

        let written = toml::to_string_pretty(&h).unwrap();
        let back: Hosts = toml::from_str(&written).unwrap();

        assert_eq!(back.group_of("b.example"), Some("client work"));
        assert!(back.groups["empty"].is_empty());
        assert_eq!(back.current.as_deref(), Some("a.example"));
        assert_eq!(back.hosts.len(), 2);
    }

    #[test]
    fn api_base_path_defaults() {
        let mut h = host("alice");
        assert_eq!(api_base_path(&h), "/api/v1");
        h.api_base_path = Some("/api/v2".into());
        assert_eq!(api_base_path(&h), "/api/v2");
    }
}
