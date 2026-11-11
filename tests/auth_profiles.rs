#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn pat_profiles_preserve_legacy_default_migrate_switch_and_isolate_hosts() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    server(&first, "qa-pat", "qa-private").await;
    server(&first, "stored-pat", "operator").await;
    server(&first, "rotated-qa-pat", "qa-private").await;
    server(&second, "second-qa-pat", "qa-private").await;
    login(dir.path(), "first.example", "qa-private", "qa-pat").await;
    assert!(
        call(dir.path(), &["auth", "group", "create", "profile-team"])
            .await
            .status
            .success()
    );
    for args in [
        vec![
            "--account",
            "qa-private",
            "api",
            "repos/o/r/issues",
            "-X",
            "POST",
        ],
        vec!["api", "repos/o/r/issues", "-X", "POST"],
    ] {
        assert!(call(dir.path(), &args).await.status.success());
    }
    let migrated = call(dir.path(), &["auth", "login", "--account", "operator"]).await;
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(call(dir.path(), &["api", "repos/o/r/issues", "-X", "POST"])
        .await
        .status
        .success());
    assert!(call(
        dir.path(),
        &["auth", "switch", "first.example", "--account", "qa-private"]
    )
    .await
    .status
    .success());
    assert!(call(dir.path(), &["api", "repos/o/r/issues", "-X", "POST"])
        .await
        .status
        .success());
    let original = std::fs::read_to_string(dir.path().join("fj/hosts.toml")).unwrap();
    login(dir.path(), "first.example", "qa-private", "rotated-qa-pat").await;
    assert_ne!(
        std::fs::read_to_string(dir.path().join("fj/hosts.toml")).unwrap(),
        original
    );
    assert!(call(dir.path(), &["api", "repos/o/r/issues", "-X", "POST"])
        .await
        .status
        .success());
    login(dir.path(), "second.example", "qa-private", "second-qa-pat").await;
    assert!(call(
        dir.path(),
        &[
            "--host",
            "second.example",
            "--account",
            "qa-private",
            "api",
            "repos/o/r/issues",
            "-X",
            "POST"
        ]
    )
    .await
    .status
    .success());
    let missing = call(
        dir.path(),
        &[
            "--host",
            "second.example",
            "--account",
            "absent",
            "api",
            "repos/o/r/issues",
            "-X",
            "POST",
        ],
    )
    .await;
    assert!(!missing.status.success());
    let requests = first.received_requests().await.unwrap();
    let tokens: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| r.headers["authorization"].to_str().unwrap())
        .collect();
    assert_eq!(
        tokens,
        [
            "token qa-pat",
            "token stored-pat",
            "token stored-pat",
            "token qa-pat",
            "token rotated-qa-pat"
        ]
    );
    assert_eq!(
        second
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1
    );
    let tokens: toml::Value =
        toml::from_str(&std::fs::read_to_string(dir.path().join("fj/tokens.toml")).unwrap())
            .unwrap();
    assert_eq!(
        tokens["tokens"]["first.example"].as_str(),
        Some("stored-pat")
    );
    assert_eq!(
        tokens["tokens"]["second.example"].as_str(),
        Some("second-stored")
    );
    // The login that rotated the PAT replaced that generation, so its secret is gone.
    assert!(!tokens["tokens"]
        .as_table()
        .unwrap()
        .values()
        .any(|v| v.as_str() == Some("qa-pat")));
    assert!(tokens["tokens"]
        .as_table()
        .unwrap()
        .values()
        .any(|v| v.as_str() == Some("rotated-qa-pat")));
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(dir.path().join("fj/hosts.toml")).unwrap())
            .unwrap();
    assert!(config["groups"]
        .as_table()
        .unwrap()
        .contains_key("profile-team"));
    assert_eq!(
        config["hosts"]["first.example"]["user"].as_str(),
        Some("operator")
    );
    assert_eq!(
        config["accounts"]["first.example"]["default_account"].as_str(),
        Some("qa-private")
    );
    let status = call(dir.path(), &["auth", "status"]).await;
    assert!(status.status.success());
    for secret in ["qa-private", "operator", "qa-pat", "stored-pat"] {
        assert!(!String::from_utf8_lossy(&status.stdout).contains(secret));
    }
    first.reset().await;
    let file = dir.path().join("fj/hosts.toml");
    let mut changed = config;
    changed["hosts"]["first.example"]["web_url"] = toml::Value::String(second.uri());
    let changed = toml::to_string(&changed).unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token stored-pat"))
        .respond_with(move |_: &wiremock::Request| {
            std::fs::write(&file, &changed).unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":1,"login":"operator"}))
        })
        .expect(1)
        .mount(&first)
        .await;
    let out = call(
        dir.path(),
        &["auth", "switch", "first.example", "--account", "operator"],
    )
    .await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("host configuration changed"));
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(dir.path().join("fj/hosts.toml")).unwrap())
            .unwrap();
    assert_eq!(
        after["accounts"]["first.example"]["default_account"].as_str(),
        Some("qa-private")
    );
}
