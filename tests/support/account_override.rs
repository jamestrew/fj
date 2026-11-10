use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use tokio::process::Command;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub fn config(first: &MockServer, second: &MockServer) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("fj")).unwrap();
    std::fs::write(dir.path().join("fj/hosts.toml"), format!(
        "current = 'first.example'\n[hosts.'first.example']\nuser = 'operator'\nweb_url = '{}'\n[hosts.'second.example']\nuser = 'operator'\nweb_url = '{}'\n", first.uri(), second.uri()
    )).unwrap();
    std::fs::write(
        dir.path().join("fj/tokens.toml"),
        "[tokens]\n'first.example' = 'stored-pat'\n'second.example' = 'second-stored'\n",
    )
    .unwrap();
    dir
}
pub fn command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fj"));
    command
        .env("XDG_CONFIG_HOME", dir)
        .env("FJ_NO_KEYCHAIN", "1")
        .env_remove("FJ_ACCOUNT")
        .env_remove("FJ_TOKEN")
        .env_remove("FJ_HOST")
        .env_remove("FJ_TOKEN_H66697273742E6578616D706C65")
        .env_remove("FJ_TOKEN_H7365636F6E642E6578616D706C65")
        .current_dir(dir);
    command
}
pub async fn identity(server: &MockServer, login: &str, token: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", format!("token {token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login":login})))
        .expect(1)
        .mount(server)
        .await;
}
