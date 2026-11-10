use base64::Engine;
use serde_json::json;
use std::path::Path;
use wiremock::{
    matchers::{body_string_contains, header, method, path},
    Mock, MockServer, ResponseTemplate,
};
pub async fn call(dir: &Path, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_fj"))
        .env("FJ_CONFIG_DIR", dir)
        .env("FJ_NO_KEYCHAIN", "1")
        .env_remove("FJ_ACCOUNT")
        .env_remove("FJ_TOKEN")
        .env_remove("FJ_SESSION")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .await
        .unwrap()
}
pub fn token(email: &str, revision: u8) -> String {
    base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&json!({
            "payload":{"user_id":email,"email":email,"revision":revision}
        }))
        .unwrap(),
    )
}
pub async fn platform(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/auth/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"strategy":"oidc"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
}
pub async fn identity(server: &MockServer, bearer: &str, instance: &str) {
    Mock::given(method("GET")).and(path("/api/v1/me/forge-instances"))
        .and(header("authorization",format!("Bearer {bearer}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"instances":[{
            "id":instance,"displayName":instance,"slug":instance,"state":"ready","publicUrl":"https://forge.example"
        }]}))).mount(server).await;
}
pub async fn login(dir: &Path, server: &MockServer, email: &str) -> std::process::Output {
    let device = Mock::given(method("POST")).and(path("/oauth/device_authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code":"dummy","user_code":"ABCD","verification_uri":"https://example.test/verify",
            "expires_in":60,"interval":1
        }))).mount_as_scoped(server).await;
    let answer = Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("device_code=dummy"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":token(email,0),"refresh_token":format!("refresh-{email}")
        })))
        .mount_as_scoped(server)
        .await;
    let out = call(
        dir,
        &[
            "auth",
            "login",
            "--fjord",
            "--device",
            "--platform-url",
            &server.uri(),
            "--account",
            email,
        ],
    )
    .await;
    drop((device, answer));
    out
}
pub fn config(dir: &Path) -> toml::Value {
    std::fs::read_to_string(dir.join("hosts.toml"))
        .unwrap()
        .parse()
        .unwrap()
}
pub fn success(out: std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub async fn expire(server: &MockServer, bearer: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header("authorization", format!("Bearer {bearer}")))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(server)
        .await;
}
