use super::*;
use crate::config::hosts::{AuthKind, Host};
use crate::config::profiles::{self, Profile};
use std::cell::RefCell;

fn pat_host() -> Host {
    Host {
        user: Some("operator".into()),
        auth_kind: AuthKind::Pat,
        ..Default::default()
    }
}
fn current(host: &str) -> Profile {
    profiles::selection(&Hosts::load().unwrap(), host, Some("operator"))
        .unwrap()
        .unwrap()
}
fn retired(host: &str) -> usize {
    Hosts::load()
        .unwrap()
        .accounts
        .get(host)
        .map_or(0, |s| s.retired.len())
}
fn has_secret(host: &str, p: &Profile) -> bool {
    auth::load_token(&profiles::key(host, p)).unwrap().is_some()
}

/// Same isolation as the profile tests: a child process with its own config dir.
fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var_os("FJ_RETIRED_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("FJ_RETIRED_CHILD", "1")
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
fn an_old_hosts_file_loads_and_round_trips_without_a_retired_list() {
    let old = "current = 'h'\n[hosts.h]\nuser = 'operator'\n\n[accounts.h]\ndefault_account = 'operator'\n\n[accounts.h.profiles.operator]\nuser = 'operator'\ngeneration = 'g1'\n";
    let hosts: Hosts = toml::from_str(old).unwrap();
    assert!(hosts.accounts["h"].retired.is_empty());
    assert!(!toml::to_string(&hosts).unwrap().contains("retired"));
}

#[test]
fn a_retired_generation_survives_a_save_and_load() {
    let mut hosts = Hosts::default();
    hosts
        .accounts
        .entry("h".into())
        .or_default()
        .retired
        .push(Profile {
            user: "operator".into(),
            generation: "g0".into(),
            fjord: None,
        });
    let back: Hosts = toml::from_str(&toml::to_string(&hosts).unwrap()).unwrap();
    assert_eq!(back.accounts["h"].retired.len(), 1);
}

#[test]
fn replacing_a_profile_deletes_the_previous_generations_secret() {
    isolated(
        "config::profile_retired::tests::replacing_a_profile_deletes_the_previous_generations_secret",
        || {
            profiles::store("h", "operator", "pat-1", pat_host()).unwrap();
            let old = current("h");
            profiles::store("h", "operator", "pat-2", pat_host()).unwrap();
            assert!(!has_secret("h", &old), "the replaced secret was left behind");
            assert_eq!(profiles::token("h", &current("h")).unwrap(), "pat-2");
            assert_eq!(retired("h"), 0);
        },
    );
}

#[test]
fn a_failed_delete_stays_listed_and_is_retried() {
    isolated(
        "config::profile_retired::tests::a_failed_delete_stays_listed_and_is_retried",
        || {
            profiles::store("h", "operator", "pat-1", pat_host()).unwrap();
            let old = current("h");
            profiles::store("h", "operator", "pat-2", pat_host()).unwrap();
            // Put the old generation back on the list, as if its delete had failed.
            crate::config::profile_publish::edit(|hosts| {
                hosts
                    .accounts
                    .get_mut("h")
                    .unwrap()
                    .retired
                    .push(old.clone());
                Ok(())
            })
            .unwrap();
            let tried = RefCell::new(0);
            let refused = drain_with("h", |_| {
                *tried.borrow_mut() += 1;
                Err(anyhow!("keychain locked"))
            });
            assert!(refused.is_err());
            assert_eq!(*tried.borrow(), 1);
            assert_eq!(retired("h"), 1, "a failed delete must keep the identity");
            // Retry through a real delete: the entry is dropped.
            drain("h").unwrap();
            assert_eq!(retired("h"), 0);
        },
    );
}

#[test]
fn the_next_store_retries_a_leftover_retired_generation() {
    isolated(
        "config::profile_retired::tests::the_next_store_retries_a_leftover_retired_generation",
        || {
            profiles::store("h", "operator", "pat-1", pat_host()).unwrap();
            let leftover = current("h");
            profiles::store("h", "operator", "pat-2", pat_host()).unwrap();
            // The old secret exists again and is listed, as after a failed delete.
            auth::store_token(&profiles::key("h", &leftover), "pat-1").unwrap();
            crate::config::profile_publish::edit(|hosts| {
                hosts
                    .accounts
                    .get_mut("h")
                    .unwrap()
                    .retired
                    .push(leftover.clone());
                Ok(())
            })
            .unwrap();
            profiles::store("h", "operator", "pat-3", pat_host()).unwrap();
            assert!(!has_secret("h", &leftover));
            assert_eq!(retired("h"), 0);
        },
    );
}

#[test]
fn logout_retries_a_leftover_retired_generation() {
    isolated(
        "config::profile_retired::tests::logout_retries_a_leftover_retired_generation",
        || {
            profiles::store("h", "operator", "pat-1", pat_host()).unwrap();
            let leftover = current("h");
            profiles::store("h", "operator", "pat-2", pat_host()).unwrap();
            auth::store_token(&profiles::key("h", &leftover), "pat-1").unwrap();
            crate::config::profile_publish::edit(|hosts| {
                hosts
                    .accounts
                    .get_mut("h")
                    .unwrap()
                    .retired
                    .push(leftover.clone());
                Ok(())
            })
            .unwrap();
            profile_session_logout(&current("h"));
            assert!(!has_secret("h", &leftover));
            assert_eq!(retired("h"), 0);
        },
    );
}

fn profile_session_logout(p: &Profile) {
    crate::config::profile_session::logout("h", p).unwrap();
}

#[test]
fn an_abandoned_generation_whose_delete_fails_is_kept_then_removed() {
    isolated(
        "config::profile_retired::tests::an_abandoned_generation_whose_delete_fails_is_kept_then_removed",
        || {
            let tokens = crate::config::config_dir().unwrap().join("tokens.toml");
            for (n, fjord) in [None, Some(crate::config::hosts::FjordAccount::default())]
                .into_iter()
                .enumerate()
            {
                let staged = Profile {
                    user: "operator".into(),
                    generation: format!("staged-{n}"),
                    fjord,
                };
                // An unreadable token store makes every delete fail.
                std::fs::create_dir(&tokens).unwrap();
                abandon("h", &staged);
                assert_eq!(retired("h"), 1, "the failed delete lost the identity");
                std::fs::remove_dir(&tokens).unwrap();
                drain("h").unwrap();
                assert_eq!(retired("h"), 0);
            }
        },
    );
}
