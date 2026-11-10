#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use wiremock::MockServer;

#[tokio::test]
async fn pat_profile_recovery_and_credential_boundaries() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    server(&first, "qa-pat", "qa-private").await;
    let before = std::fs::read(dir.path().join("fj/hosts.toml")).unwrap();
    let failed = call(
        dir.path(),
        &[
            "auth",
            "login",
            "--host",
            "first.example",
            "--account",
            "wrong-user",
            "--token",
            "qa-pat",
        ],
    )
    .await;
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read(dir.path().join("fj/hosts.toml")).unwrap(),
        before
    );
    login(dir.path(), "first.example", "qa-private", "qa-pat").await;
    let before = std::fs::read(dir.path().join("fj/hosts.toml")).unwrap();
    std::fs::write(
        dir.path().join("fj/tokens.toml"),
        "[tokens]\n'first.example'='stored-pat'\n",
    )
    .unwrap();
    let failed = call(
        dir.path(),
        &["auth", "switch", "first.example", "--account", "qa-private"],
    )
    .await;
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read(dir.path().join("fj/hosts.toml")).unwrap(),
        before
    );
    let before_first = first.received_requests().await.unwrap().len();
    server(&second, "b-pat", "qa-private").await;
    let mut child = support::command(dir.path())
        .env("FJ_CONFIG_DIR", dir.path().join("fj"))
        .env("FJ_TOKEN", "a-private-pat")
        .args([
            "auth",
            "login",
            "--host",
            "second.example",
            "--account",
            "qa-private",
            "--with-token",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"b-pat\n")
        .await
        .unwrap();
    let out = child.wait_with_output().await.unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(first.received_requests().await.unwrap().len(), before_first);
    assert!(second
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.headers["authorization"] == "token b-pat"));
    let before = second.received_requests().await.unwrap().len();
    let mut child = support::command(dir.path())
        .env("FJ_CONFIG_DIR", dir.path().join("fj"))
        .args(["auth", "login"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"second.example\n")
        .await
        .unwrap();
    let out = child.wait_with_output().await.unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("use auth login --account"));
    assert_eq!(second.received_requests().await.unwrap().len(), before);
    let out = call(dir.path(), &["auth", "setup-git", "--dry-run"]).await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--unset-all"));
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_fj"))
        .parent()
        .unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let out = tokio::process::Command::new("sh")
        .env("PATH", path)
        .env("FJ_CONFIG_DIR", dir.path().join("fj"))
        .env("FJ_NO_KEYCHAIN", "1")
        .args([
            "-c",
            "f() { fj auth token --host first.example | sed 's/^/password=/'; }; f get",
        ])
        .output()
        .await
        .unwrap();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--unset-all"));
    wiremock::Mock::given(wiremock::matchers::path("/api/auth/status"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"strategy":"oidc"})),
        )
        .expect(1)
        .mount(&first)
        .await;
    let out = call(
        dir.path(),
        &[
            "auth",
            "login",
            "--fjord",
            "--device",
            "--platform-url",
            &first.uri(),
        ],
    )
    .await;
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("legacy maintenance"));
    let out = call(dir.path(), &["auth", "logout"]).await;
    assert!(String::from_utf8_lossy(&out.stderr).contains("multiple hosts"));
    let file = dir.path().join("fj/hosts.toml");
    let mut sole: toml::Value = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    sole.as_table_mut().unwrap().remove("current");
    for table in ["hosts", "accounts"] {
        sole[table].as_table_mut().unwrap().remove("second.example");
    }
    let before = toml::to_string(&sole).unwrap();
    std::fs::write(&file, &before).unwrap();
    for action in ["token", "refresh", "session", "setup-git", "logout"] {
        let args = match action {
            "setup-git" => vec!["auth", action, "--dry-run"],
            "session" => vec!["auth", action, "--clear"],
            _ => vec!["auth", action],
        };
        let out = call(dir.path(), &args).await;
        assert!(!out.status.success(), "{action}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("profiled"),
            "{action}"
        );
        assert!(out.stdout.is_empty());
    }
    assert_eq!(std::fs::read_to_string(file).unwrap(), before);
}
