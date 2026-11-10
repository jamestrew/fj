#[path = "support/account_override.rs"]
mod support;
use serde_json::json;
use support::{command, config, identity};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn account_override_cli_rejects_each_auth_query_carrier_without_logging() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = config(&first, &second);
    for name in ["token", "access_token", "sudo", "ToKeN", "ACCESS_TOKEN"] {
        for in_path in [false, true] {
            let mut command = command(dir.path());
            command
                .env("FJ_TOKEN", "qa-pat")
                .env("FJ_ACCOUNT", "qa-private")
                .args(["--debug", "api"]);
            if in_path {
                command
                    .arg(format!("repos/o/r/issues?{name}=private-operator-pat"))
                    .args(["-X", "POST"]);
            } else {
                command
                    .args(["repos/o/r/issues/42", "-X", "DELETE", "-f"])
                    .arg(format!("{name}=private-operator-pat"));
            }
            let output = command.output().await.unwrap();
            assert!(!output.status.success());
            let logged = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                logged.contains("alternate authentication conflicts"),
                "{logged}"
            );
            for secret in ["private-operator-pat", "qa-private", "qa-pat"] {
                assert!(!logged.contains(secret), "{logged}");
            }
        }
    }
    let output = command(dir.path())
        .env("FJ_TOKEN", "qa-pat")
        .env("FJ_ACCOUNT", "qa-private")
        .args([
            "--debug",
            "api",
            "repos/o/r/issues",
            "-X",
            "POST",
            "-H",
            "Content-Type=application/x-www-form-urlencoded",
            "--input",
            "\"&token=private-operator-pat&\"",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let logged = String::from_utf8_lossy(&output.stderr);
    assert!(
        logged.contains("alternate authentication conflicts"),
        "{logged}"
    );
    assert!(!logged.contains("private-operator-pat"));
    assert!(first.received_requests().await.unwrap().is_empty());
    assert!(second.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn account_override_cli_standalone_attach_and_release_upload_verify_before_upload() {
    for release in [false, true] {
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        let dir = config(&first, &second);
        let file = dir.path().join("shot.png");
        std::fs::write(&file, b"data").unwrap();
        identity(&first, "qa-private", "qa-pat").await;
        let kind = if release { "releases" } else { "issues" };
        let asset_path = format!("/api/v1/repos/o/r/{kind}/42/assets");
        Mock::given(method("POST")).and(path(&asset_path))
            .and(header("authorization", "token qa-pat"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id":1,"name":"shot.png","size":4,"browser_download_url":"https://files.example/shot.png","created_at":"2026-11-10T00:00:00Z"
            }))).expect(1).mount(&first).await;
        if release {
            Mock::given(method("GET")).and(path("/api/v1/repos/o/r/releases/tags/v1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":42,"tag_name":"v1","target_commitish":"main","draft":false,"prerelease":false,"html_url":"https://example/release/v1","created_at":"2026-11-10T00:00:00Z","assets":[]
                }))).expect(1).mount(&first).await;
        } else {
            let pull = json!({"id":42,"number":42,"title":"Test","body":"Original","state":"open","user":{"id":1,"login":"operator"},"html_url":"https://example/pulls/42","head":{},"base":{},"created_at":"2026-11-10T00:00:00Z","updated_at":"2026-11-10T00:00:00Z"});
            for verb in ["GET", "PATCH"] {
                Mock::given(method(verb))
                    .and(path("/api/v1/repos/o/r/pulls/42"))
                    .and(header("authorization", "token qa-pat"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(pull.clone()))
                    .expect(if verb == "GET" { 2 } else { 1 })
                    .mount(&first)
                    .await;
            }
        }
        let mut command = command(dir.path());
        command
            .env("FJ_TOKEN", "qa-pat")
            .args(["--account", "qa-private"]);
        if release {
            command.args(["release", "upload", "v1"]);
        } else {
            command.args(["pr", "attach", "42"]);
        }
        let output = command
            .arg(&file)
            .args(["-R", "o/r"])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = first.received_requests().await.unwrap();
        assert_eq!(requests[0].url.path(), "/api/v1/user");
        let upload = requests
            .iter()
            .position(|r| r.url.path() == asset_path)
            .unwrap();
        assert_eq!(upload, if release { 2 } else { 1 });
        assert!(requests[upload].headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("multipart/form-data"));
        assert!(String::from_utf8_lossy(&requests[upload].body).contains("data"));
        if !release {
            let patch = requests.iter().find(|r| r.method == "PATCH").unwrap();
            assert!(String::from_utf8_lossy(&patch.body).contains("https://files.example/shot.png"));
        }
    }
}
