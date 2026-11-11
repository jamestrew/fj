use super::*;

#[test]
fn profile_keys_scope_host_account_kind_and_generation() {
    let p = Profile {
        fjord: None,
        user: "qa".into(),
        generation: "1".into(),
    };
    assert_ne!(key("a", &p), key("b", &p));
    assert_ne!(
        key("a", &p),
        key(
            "a",
            &Profile {
                fjord: None,
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
                fjord: None,
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
                fjord: None,
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
        fjord: None,
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

fn pat_host() -> Host {
    Host {
        user: Some("operator".into()),
        auth_kind: AuthKind::Pat,
        ..Default::default()
    }
}

/// Runs `body` in a child process with its own config dir and no keychain, so
/// the tests below can use real files without racing the others on process env.
fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var_os("FJ_PROFILE_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("FJ_PROFILE_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("FJ_CONFIG_DIR", dir.path());
    std::env::set_var("FJ_NO_KEYCHAIN", "1");
    body();
}

#[test]
fn a_stale_logout_after_a_replacement_leaves_the_new_secret() {
    isolated(
        "config::profiles::tests::a_stale_logout_after_a_replacement_leaves_the_new_secret",
        || {
            store("h", "Operator", "pat-1", pat_host()).unwrap();
            let old = selection(&Hosts::load().unwrap(), "h", Some("operator"))
                .unwrap()
                .unwrap();
            store("h", "operator", "pat-2", pat_host()).unwrap();
            let new = selection(&Hosts::load().unwrap(), "h", Some("operator"))
                .unwrap()
                .unwrap();
            assert_ne!(old.generation, new.generation);
            let stale = super::super::profile_session::logout("h", &old);
            assert!(stale.is_err(), "a stale logout must not run");
            assert_eq!(token("h", &new).unwrap(), "pat-2");
        },
    );
}

#[test]
fn a_fenced_store_refuses_a_changed_profile_and_stages_nothing() {
    isolated(
        "config::profiles::tests::a_fenced_store_refuses_a_changed_profile_and_stages_nothing",
        || {
            store("h", "operator", "pat-1", pat_host()).unwrap();
            let seen = selection(&Hosts::load().unwrap(), "h", Some("operator"))
                .unwrap()
                .unwrap();
            let tokens = || {
                std::fs::read_to_string(crate::config::config_dir().unwrap().join("tokens.toml"))
                    .unwrap()
            };
            // Unchanged: the fenced store publishes.
            store_fenced("h", "operator", "pat-2", pat_host(), &seen).unwrap();
            let current = selection(&Hosts::load().unwrap(), "h", Some("operator"))
                .unwrap()
                .unwrap();
            assert_eq!(token("h", &current).unwrap(), "pat-2");
            // `seen` is now stale: a replacement happened in between.
            let before = tokens();
            let refused = store_fenced("h", "operator", "pat-3", pat_host(), &seen);
            assert!(refused.is_err());
            assert_eq!(tokens(), before, "the refused store staged a secret");
            assert_eq!(token("h", &current).unwrap(), "pat-2");
        },
    );
}

#[test]
fn a_replacement_waits_for_the_account_lock() {
    isolated(
        "config::profiles::tests::a_replacement_waits_for_the_account_lock",
        || {
            let held = super::super::fjord_scope::lock(
                "h",
                &Profile {
                    fjord: None,
                    user: "operator".into(),
                    generation: String::new(),
                },
            )
            .unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                store("h", "operator", "pat-1", pat_host()).unwrap();
                tx.send(()).unwrap();
            });
            assert!(
                rx.recv_timeout(std::time::Duration::from_millis(300))
                    .is_err(),
                "store ran while another holder had the account lock"
            );
            drop(held);
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            worker.join().unwrap();
        },
    );
}
