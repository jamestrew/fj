use super::*;
use base64::Engine;
use serde_json::json;
use wiremock::{
    matchers::{body_string_contains, header, method, path},
    Mock, MockServer, ResponseTemplate,
};
fn bearer(revision: u8) -> String {
    base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&json!({
            "payload":{"user_id":"id","email":"a@test","revision":revision}
        }))
        .unwrap(),
    )
}
#[tokio::test]
async fn rotation_survives_verification_failure_and_expired_peer_is_refreshed() {
    if std::env::var_os("FJ_SCOPE_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "config::fjord_scope::tests::rotation_survives_verification_failure_and_expired_peer_is_refreshed", "--nocapture"])
            .env("FJ_SCOPE_CHILD", "1").output().unwrap();
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
    let server = MockServer::start().await;
    let platform = server.uri();
    for revision in 0..3 {
        Mock::given(method("GET"))
            .and(path("/api/v1/me/forge-instances"))
            .and(header(
                "authorization",
                format!("Bearer {}", bearer(revision)),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"instances":[]})))
            .mount(&server)
            .await;
    }
    super::super::fjord_profiles::login(
        &platform,
        "a@test",
        &oidc::TokenResponse {
            access_token: bearer(0),
            refresh_token: Some("grant-a".into()),
        },
        "https",
    )
    .await
    .unwrap();
    let hosts = super::super::hosts::Hosts::load().unwrap();
    let profile = profiles::selection(&hosts, &platform, Some("a@test"))
        .unwrap()
        .unwrap();
    let scope = Scope {
        host: platform.clone(),
        profile,
        destination: hosts.hosts[&platform].clone(),
    };
    let before = std::fs::read(dir.path().join("hosts.toml")).unwrap();
    for (grant, revision, replacement) in [("grant-a", 1, "grant-b"), ("grant-b", 2, "grant-c")] {
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains(format!("refresh_token={grant}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token":bearer(revision),"refresh_token":replacement
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let status = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(503));
    let responding = status.clone();
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header("authorization", format!("Bearer {}", bearer(1))))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(responding.load(std::sync::atomic::Ordering::SeqCst))
                .set_body_json(json!({"instances":[]}))
        })
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(scope.refresh(&bearer(0)).await.is_err());
    let stored = load(&platform, &scope.profile).unwrap();
    assert_eq!(stored.bearer, bearer(1));
    assert_eq!(stored.refresh.as_deref(), Some("grant-b"));
    assert_eq!(
        std::fs::read(dir.path().join("hosts.toml")).unwrap(),
        before
    );
    assert!(scope.refresh(&bearer(0)).await.is_err());
    status.store(200, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(scope.refresh(&bearer(0)).await.unwrap(), Some(bearer(1)));
    status.store(401, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(scope.refresh(&bearer(0)).await.unwrap(), Some(bearer(2)));
    assert_eq!(
        load(&platform, &scope.profile).unwrap().refresh.as_deref(),
        Some("grant-c")
    );
    assert_eq!(
        std::fs::read(dir.path().join("hosts.toml")).unwrap(),
        before
    );
}
