use super::*;

#[test]
fn profile_keys_scope_host_account_kind_and_generation() {
    let p = Profile {
        user: "qa".into(),
        generation: "1".into(),
    };
    assert_ne!(key("a", &p), key("b", &p));
    assert_ne!(
        key("a", &p),
        key(
            "a",
            &Profile {
                user: "other".into(),
                generation: "1".into()
            }
        )
    );
    assert_ne!(
        key("a", &p),
        key(
            "a",
            &Profile {
                user: "qa".into(),
                generation: "2".into()
            }
        )
    );
    assert_ne!(
        key("a:qa", &p),
        key(
            "a",
            &Profile {
                user: "qa:qa".into(),
                generation: "1".into()
            }
        )
    );
    assert!(key("a", &p).contains(":pat:"));
}

#[test]
fn profile_staging_requires_successful_write_and_matching_readback() {
    use std::cell::Cell;
    for failure in 0..5 {
        let wrote = Cell::new(false);
        let result = stage(
            "new-generation",
            "new-pat",
            |_, _| {
                wrote.set(true);
                if failure == 0 {
                    Err(anyhow!("write failed"))
                } else {
                    Ok(())
                }
            },
            |_| match failure {
                1 => Err(anyhow!("read failed")),
                2 => Ok(Some("wrong".into())),
                3 => Ok(None),
                _ => Ok(Some("new-pat".into())),
            },
        );
        assert_eq!(result.is_ok(), failure == 4);
        assert!(wrote.get());
    }
}

#[test]
fn profile_selection_is_host_scoped_and_status_is_redacted() {
    let mut hosts = Hosts::default();
    let profile = Profile {
        user: "private-account".into(),
        generation: "1".into(),
    };
    hosts
        .accounts
        .entry("a".into())
        .or_default()
        .profiles
        .insert(profile.user.clone(), profile);
    assert!(selection(&hosts, "a", Some("PRIVATE-ACCOUNT"))
        .unwrap()
        .is_some());
    assert!(selection(&hosts, "b", Some("private-account"))
        .unwrap()
        .is_none());
    assert!(selection(&hosts, "a", None).unwrap().is_none());
    hosts.accounts.get_mut("a").unwrap().default_account = Some("private-account".into());
    assert!(selection(&hosts, "a", None).unwrap().is_some());
    assert!(!status(&hosts, "a").contains("private-account"));
    assert!(!format!("{hosts:?}").contains("private-account"));
    hosts.accounts.get_mut("a").unwrap().profiles.clear();
    assert!(selection(&hosts, "a", None).is_err());
}

#[test]
fn profile_legacy_config_roundtrip_remains_without_profile_tables() {
    let hosts: Hosts = toml::from_str("current='a'\n[hosts.a]\nuser='operator'\n").unwrap();
    let text = toml::to_string(&hosts).unwrap();
    assert!(!text.contains("accounts"));
    assert!(text.contains("operator"));
}
