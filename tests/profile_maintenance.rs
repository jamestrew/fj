#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use std::path::Path;
use wiremock::MockServer;

async fn out(dir: &Path, args: &[&str]) -> String {
    let o = call(dir, args).await;
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
async fn fails(dir: &Path, args: &[&str]) -> String {
    let o = call(dir, args).await;
    assert!(!o.status.success(), "{args:?} should fail");
    let text = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        !text.contains("tok-") && !text.contains("ck-"),
        "redacted: {text}"
    );
    text
}
/// Make the credential store unusable (even for root) and restore it afterwards.
fn block_store(d: &Path, on: bool) {
    let (file, saved) = (d.join("fj/tokens.toml"), d.join("fj/tokens.saved"));
    if on {
        std::fs::rename(&file, &saved).unwrap();
        std::fs::create_dir(&file).unwrap();
    } else {
        std::fs::remove_dir(&file).unwrap();
        std::fs::rename(saved, file).unwrap();
    }
}
fn first_url(dir: &Path, url: Option<&str>) {
    let file = dir.join("fj/hosts.toml");
    let mut config: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
    let host = config["hosts"]["first.example"].as_table_mut().unwrap();
    if let Some(url) = url {
        host.insert("web_url".into(), url.into());
    }
    std::fs::write(file, toml::to_string(&config).unwrap()).unwrap();
}
#[tokio::test]
async fn profile_maintenance_scopes_cookies_tokens_refresh_and_logout() {
    let (first, second) = (MockServer::start().await, MockServer::start().await);
    let dir = support::config(&first, &second);
    let d = dir.path();
    for (token, user) in [("tok-a", "qa-a"), ("tok-b", "qa-b"), ("tok-b2", "qa-b")] {
        server(&first, token, user).await;
    }
    server(&second, "tok-a2", "qa-a").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;
    login(d, "second.example", "qa-a", "tok-a2").await;
    out(d, &["auth", "switch", "first.example", "--account", "qa-a"]).await;
    assert_eq!(
        out(d, &["--account", "qa-a", "auth", "token", "--force"]).await,
        "tok-a"
    );
    let helper = fails(d, &["auth", "token", "--force"]).await;
    assert!(helper.contains("--unset-all"), "{helper}");
    assert_eq!(
        out(d, &["--account", "qa-b", "auth", "token", "--force"]).await,
        "tok-b"
    );
    fails(d, &["--account", "nobody", "auth", "token", "--force"]).await;
    // Cookies are per account and per destination instance.
    out(
        d,
        &["--account", "qa-a", "auth", "session", "--cookie", "ck-a"],
    )
    .await;
    assert!(out(d, &["auth", "status"])
        .await
        .contains("first.example: 2 profile(s); default: profile [redacted]; web session(s): 1"));
    first_url(d, Some(&second.uri()));
    assert!(!out(d, &["auth", "status"]).await.contains("web session"));
    first_url(d, Some(&first.uri()));
    assert!(out(d, &["auth", "status"])
        .await
        .contains("web session(s): 1"));
    // --with-cookie reads stdin only, even when FJ_SESSION is set.
    let o = support::command(d)
        .env("FJ_CONFIG_DIR", d.join("fj"))
        .env("FJ_SESSION", "ck-env")
        .stdin(std::process::Stdio::null())
        .args(["--account", "qa-a", "auth", "session", "--with-cookie"])
        .output()
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).contains("empty cookie value"));
    // A failed staging write keeps the previous cookie and stores no new one.
    block_store(d, true);
    fails(
        d,
        &["--account", "qa-a", "auth", "session", "--cookie", "ck-a2"],
    )
    .await;
    block_store(d, false);
    let stored = std::fs::read_to_string(d.join("fj/tokens.toml")).unwrap();
    assert!(stored.contains("ck-a") && !stored.contains("ck-a2") && !stored.contains("ck-env"));
    out(d, &["--account", "qa-a", "auth", "session", "--clear"]).await;
    assert!(!out(d, &["auth", "status"]).await.contains("web session"));
    // A cookie survives credential rotation, and replacing it leaves one secret.
    out(
        d,
        &["--account", "qa-b", "auth", "session", "--cookie", "ck-b"],
    )
    .await;
    // A rotated PAT is verified before it replaces the profile credential.
    fails(
        d,
        &["--account", "qa-b", "auth", "refresh", "--token", "tok-bad"],
    )
    .await;
    out(
        d,
        &["--account", "qa-b", "auth", "refresh", "--token", "tok-b2"],
    )
    .await;
    assert_eq!(
        out(d, &["--account", "qa-b", "auth", "token", "--force"]).await,
        "tok-b2"
    );
    assert!(out(d, &["auth", "status"])
        .await
        .contains("web session(s): 1"));
    out(
        d,
        &["--account", "qa-b", "auth", "session", "--cookie", "ck-b2"],
    )
    .await;
    let stored = std::fs::read_to_string(d.join("fj/tokens.toml")).unwrap();
    assert!(stored.contains("ck-b2") && !stored.contains("\"ck-b\"") && !stored.contains("'ck-b'"));
    // A failed secret delete keeps the profile selectable, so logout can be retried.
    block_store(d, true);
    fails(d, &["auth", "logout", "--host", "first.example"]).await;
    block_store(d, false);
    assert_eq!(
        out(d, &["--account", "qa-a", "auth", "token", "--force"]).await,
        "tok-a"
    );
    // Logout removes one profile and never falls back to legacy credentials.
    out(d, &["auth", "logout", "--host", "first.example"]).await;
    let api = [
        "api",
        "repos/o/r/issues",
        "-X",
        "POST",
        "--host",
        "first.example",
    ];
    let gone = fails(d, &api).await;
    assert!(
        gone.contains("default account profile is missing"),
        "{gone}"
    );
    let tokens = std::fs::read_to_string(d.join("fj/tokens.toml")).unwrap();
    assert!(tokens.contains("stored-pat"));
    out(
        d,
        &[
            "--host",
            "second.example",
            "--account",
            "qa-a",
            "auth",
            "token",
            "--force",
        ],
    )
    .await;
    out(d, &["auth", "switch", "first.example", "--account", "qa-b"]).await;
    assert_eq!(
        out(d, &["--account", "qa-b", "auth", "token", "--force"]).await,
        "tok-b2"
    );
}
