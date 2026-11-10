#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use wiremock::MockServer;

#[tokio::test]
async fn pat_profile_failed_verification_and_missing_credentials_preserve_config() {
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
}
