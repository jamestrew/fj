#[path = "support/fjord_profile_refresh.rs"]
#[allow(dead_code)]
mod support;
use serde_json::json;
use support::*;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};
#[tokio::test]
async fn fjord_profile_switch_and_verified_destination_race() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let account = "private@example.test";
    platform(&server).await;
    identity(&server, &token(account, 0), "instance").await;
    success(login(dir.path(), &server, account).await);
    assert!(config(dir.path()).get("current").is_none());
    success(
        call(
            dir.path(),
            &["auth", "switch", &server.uri(), "--account", account],
        )
        .await,
    );
    assert_eq!(
        config(dir.path())["current"].as_str(),
        Some(server.uri().as_str())
    );
    assert_eq!(
        config(dir.path())["accounts"][server.uri()]["default_account"].as_str(),
        Some(account)
    );
    success(call(dir.path(), &["instances", "use", "instance"]).await);
    let original = config(dir.path());
    let mut changed = original.clone();
    changed["hosts"][server.uri()]
        .as_table_mut()
        .unwrap()
        .insert(
            "web_url".into(),
            toml::Value::String("https://changed.example".into()),
        );
    let replacement = toml::to_string(&changed).unwrap();
    let file = dir.path().join("hosts.toml");
    let bytes = replacement.clone();
    let race = Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header(
            "authorization",
            format!("Bearer {}", token(account, 0)),
        ))
        .respond_with(move |_: &wiremock::Request| {
            std::fs::write(&file, &bytes).unwrap();
            ResponseTemplate::new(200).set_body_json(json!({"instances":[]}))
        })
        .with_priority(1)
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    let failed = login(dir.path(), &server, account).await;
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hosts.toml")).unwrap(),
        replacement
    );
    assert_eq!(config(dir.path())["accounts"], original["accounts"]);
    for private in [account, &token(account, 0)] {
        assert!(!String::from_utf8_lossy(&failed.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&failed.stderr).contains(private));
    }
    drop(race);
}
