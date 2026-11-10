use super::support;
use serde_json::json;
use std::path::Path;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub async fn call(dir: &Path, args: &[&str]) -> std::process::Output {
    support::command(dir)
        .env("FJ_CONFIG_DIR", dir.join("fj"))
        .args(args)
        .output()
        .await
        .unwrap()
}
pub async fn login(dir: &Path, host: &str, user: &str, token: &str) {
    let out = call(
        dir,
        &[
            "auth",
            "login",
            "--host",
            host,
            "--account",
            user,
            "--token",
            token,
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains(user));
}
pub async fn server(server: &MockServer, token: &str, user: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", format!("token {token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":1,"login":user})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(header("authorization", format!("token {token}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ok":true})))
        .mount(server)
        .await;
}
