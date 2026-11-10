use super::*;
use crate::config::{
    hosts::FjordAccount,
    muted,
    profiles::{HostProfiles, Profile},
};
fn fixture() -> Hosts {
    let mut hosts = Hosts::default();
    for name in ["p", "q"] {
        let metadata = FjordAccount {
            platform_url: format!("https://{name}.test"),
            default_instance_id: Some("legacy".into()),
            ..Default::default()
        };
        hosts.hosts.insert(
            name.into(),
            Host {
                auth_kind: AuthKind::FjordAccount,
                fjord_account: Some(metadata.clone()),
                ..Default::default()
            },
        );
        let mut set = HostProfiles {
            default_account: Some("a@test".into()),
            ..Default::default()
        };
        for account in ["a@test", "b@test"] {
            set.profiles.insert(
                account.into(),
                Profile {
                    user: account.into(),
                    generation: "generation".into(),
                    fjord: Some(FjordAccount {
                        default_instance_id: Some("shared-instance".into()),
                        default_instance_public_url: Some(format!("https://{account}.invalid")),
                        ..metadata.clone()
                    }),
                },
            );
        }
        hosts.accounts.insert(name.into(), set);
    }
    hosts
}
#[test]
fn metadata_resolves_profiles_without_credentials_or_instance_and_refuses_mismatch() {
    let mut hosts = fixture();
    let mut default = hosts.hosts["p"].clone();
    apply(&hosts, "p", &mut default, None).unwrap();
    let mut explicit = hosts.hosts["p"].clone();
    apply(&hosts, "p", &mut explicit, Some("A@TEST")).unwrap();
    assert_eq!(
        default
            .fjord_account
            .as_ref()
            .unwrap()
            .default_instance_public_url,
        explicit
            .fjord_account
            .as_ref()
            .unwrap()
            .default_instance_public_url
    );
    assert_eq!(default.user.as_deref(), Some("[redacted account]"));
    hosts
        .accounts
        .get_mut("p")
        .unwrap()
        .profiles
        .get_mut("a@test")
        .unwrap()
        .fjord
        .as_mut()
        .unwrap()
        .default_instance_id = None;
    apply(&hosts, "p", &mut explicit, None).unwrap();
    assert!(explicit
        .fjord_account
        .as_ref()
        .unwrap()
        .default_instance_id
        .is_none());
    assert!(muted::scope_key("p", Some(&explicit)).is_err());
    assert!(apply(&hosts, "p", &mut explicit, Some("missing@test")).is_err());
    explicit = hosts.hosts["p"].clone();
    explicit.fjord_account.as_mut().unwrap().platform_url = "https://changed.test".into();
    assert!(apply(&hosts, "p", &mut explicit, None).is_err());
    let mut pat = Host::default();
    apply(
        &Hosts::default(),
        "unconfigured",
        &mut pat,
        Some("ephemeral"),
    )
    .unwrap();
    assert!(pat.selected_profile.is_none());
    assert!(muted::scope_key("unconfigured", Some(&pat))
        .unwrap()
        .starts_with("profile:"));
    assert_eq!(pat.selected_account.unwrap().as_str(), "ephemeral");
}
#[test]
fn named_mute_identity_isolates_accounts_hosts_and_instances_and_retains_legacy() {
    let mut hosts = fixture();
    let mut scopes = Vec::new();
    for (name, account) in [("p", "a@test"), ("p", "b@test"), ("q", "a@test")] {
        let mut config = hosts.hosts[name].clone();
        apply(&hosts, name, &mut config, Some(account)).unwrap();
        let key = muted::scope_key(name, Some(&config)).unwrap();
        assert!(!key.contains(account));
        assert!(!scopes.contains(&key));
        scopes.push(key);
    }
    let mut mutes = muted::MutedRepos::default();
    mutes.mute(&scopes[0], "o/r");
    for key in &scopes[1..] {
        assert!(!mutes.is_muted(key, "o/r"));
    }
    mutes.mute("p#legacy", "old/repo");
    assert!(!mutes.is_muted(&scopes[0], "old/repo"));
    assert_eq!(
        muted::scope_key("p", hosts.hosts.get("p")).unwrap(),
        "p#legacy"
    );
    let p = hosts
        .accounts
        .get_mut("p")
        .unwrap()
        .profiles
        .get_mut("a@test")
        .unwrap();
    p.generation = "rotated".into();
    let mut config = hosts.hosts["p"].clone();
    apply(&hosts, "p", &mut config, None).unwrap();
    assert_eq!(muted::scope_key("p", Some(&config)).unwrap(), scopes[0]);
    config.fjord_account.as_mut().unwrap().default_instance_id = Some("different".into());
    assert_ne!(muted::scope_key("p", Some(&config)).unwrap(), scopes[0]);
}
