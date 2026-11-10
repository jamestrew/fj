#[path = "support/fjord_profiles.rs"]
#[allow(dead_code)]
mod support;
use serde_json::json;
use support::*;
use wiremock::{
    matchers::{body_string_contains, method, path},
    Mock, MockServer, ResponseTemplate,
};
#[tokio::test]
async fn fjord_switch_refreshes_expired_target_and_preserves_default_on_rejection() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let first = "first@test";
    let target = "target@test";
    platform(&server).await;
    identity(&server, &token(first, 0), "first").await;
    identity(&server, &token(target, 0), "target").await;
    identity(&server, &token(target, 1), "target").await;
    success(login(dir.path(), &server, first).await);
    success(login(dir.path(), &server, target).await);
    success(
        call(
            dir.path(),
            &["auth", "switch", &server.uri(), "--account", first],
        )
        .await,
    );
    expire(&server, &token(target, 0)).await;
    let before = std::fs::read(dir.path().join("hosts.toml")).unwrap();
    let secrets = std::fs::read(dir.path().join("tokens.toml")).unwrap();
    let rejected = Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=refresh-target%40test"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error":"invalid_grant", "error_description":"private-provider-body"
        })))
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    let args = [
        "--debug",
        "auth",
        "switch",
        &server.uri(),
        "--account",
        target,
    ];
    let failed = call(dir.path(), &args).await;
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read(dir.path().join("hosts.toml")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read(dir.path().join("tokens.toml")).unwrap(),
        secrets
    );
    for private in [target, "private-provider-body", "refresh-target@test"] {
        assert!(!String::from_utf8_lossy(&failed.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&failed.stderr).contains(private));
    }
    drop(rejected);
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("refresh_token=refresh-target%40test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":token(target, 1), "refresh_token":"renewed-target"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let state = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(503));
    let response = state.clone();
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {}", token(target, 1)),
        ))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(response.load(std::sync::atomic::Ordering::SeqCst))
        })
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(!call(dir.path(), &args).await.status.success());
    assert_eq!(
        std::fs::read(dir.path().join("hosts.toml")).unwrap(),
        before
    );
    assert!(std::fs::read_to_string(dir.path().join("tokens.toml"))
        .unwrap()
        .contains("renewed-target"));
    state.store(200, std::sync::atomic::Ordering::SeqCst);
    success(call(dir.path(), &args).await);
    let changed = config(dir.path());
    assert_eq!(
        changed["accounts"][server.uri()]["default_account"].as_str(),
        Some(target)
    );
    assert_eq!(changed["current"].as_str(), Some(server.uri().as_str()));
    let stored = std::fs::read_to_string(dir.path().join("tokens.toml")).unwrap();
    assert!(stored.contains("renewed-target"));
    assert!(stored.contains("refresh-first@test"));
}
