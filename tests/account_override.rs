//! Exercise CLI parsing, host resolution and the production request adapter together.
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use tokio::process::Command;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(first: &MockServer, second: &MockServer) -> TempDir {
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
fn command(dir: &Path) -> Command {
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
async fn identity(server: &MockServer, login: &str, token: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", format!("token {token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login":login})))
        .expect(1)
        .mount(server)
        .await;
}
async fn mutation(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(header("authorization", format!("token {token}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ok":true})))
        .expect(1)
        .mount(server)
        .await;
}
#[tokio::test]
async fn account_override_env_and_flag_preserve_stored_credentials() {
    for flag in [false, true] {
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        let dir = config(&first, &second);
        let before = std::fs::read(dir.path().join("fj/tokens.toml")).unwrap();
        let hosts = std::fs::read(dir.path().join("fj/hosts.toml")).unwrap();
        identity(&first, "qa-private", "override-pat").await;
        mutation(&first, "override-pat").await;
        let mut command = command(dir.path());
        command
            .env("FJ_TOKEN", "override-pat")
            .env("FJ_ACCOUNT", if flag { "wrong-env" } else { "qa-private" });
        if flag {
            command.args(["--account", "qa-private"]);
        }
        let output = command
            .args(["api", "repos/o/r/issues", "-X", "POST"])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("qa-private"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("override-pat"));
        assert_eq!(
            std::fs::read(dir.path().join("fj/tokens.toml")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(dir.path().join("fj/hosts.toml")).unwrap(),
            hosts
        );
        assert!(second.received_requests().await.unwrap().is_empty());
    }
}
#[tokio::test]
async fn account_override_nondefault_host_requires_scoped_token() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = config(&first, &second);
    let output = command(dir.path())
        .env("FJ_TOKEN", "wrong-host-pat")
        .args([
            "--host",
            "second.example",
            "--account",
            "qa-private",
            "api",
            "/user",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(second.received_requests().await.unwrap().is_empty());
    identity(&second, "qa-private", "scoped-pat").await;
    mutation(&second, "scoped-pat").await;
    let output = command(dir.path())
        .env("FJ_TOKEN", "wrong-host-pat")
        .env("FJ_TOKEN_H7365636F6E642E6578616D706C65", "scoped-pat")
        .args([
            "--host",
            "second.example",
            "--account",
            "qa-private",
            "api",
            "repos/o/r/issues",
            "-X",
            "POST",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(first.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn account_override_absent_keeps_stored_default() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = config(&first, &second);
    mutation(&first, "stored-pat").await;
    let output = command(dir.path())
        .args(["api", "repos/o/r/issues", "-X", "POST"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(first.received_requests().await.unwrap().len(), 1);
}
