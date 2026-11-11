use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A mixed-case host key written by an older release keeps resolving, and a
/// repeat login updates that entry instead of adding a lowercase twin.
#[tokio::test]
async fn legacy_mixed_case_host_key_resolves_and_login_updates_it() {
    let server = MockServer::start().await;
    let port = server.address().port();
    let bare = format!("localhost:{port}");
    let (legacy, url) = (
        format!("LOCALHOST:{port}"),
        format!("http://localhost:{port}"),
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("fj")).unwrap();
    let hosts = dir.path().join("fj/hosts.toml");
    std::fs::write(
        &hosts,
        format!("current = '{legacy}'\n[hosts.'{legacy}']\nuser = 'operator'\nweb_url = '{url}'\n"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("fj/tokens.toml"),
        format!("[tokens]\n'{legacy}' = 'old-pat'\n"),
    )
    .unwrap();
    for token in ["old-pat", "new-pat"] {
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .and(header("authorization", format!("token {token}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id":1,"login":"operator"})),
            )
            .mount(&server)
            .await;
    }
    let run = |args: Vec<&str>| {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_fj"))
            .env("XDG_CONFIG_HOME", dir.path())
            .env("FJ_NO_KEYCHAIN", "1")
            .current_dir(dir.path())
            .env("FJ_CONFIG_DIR", dir.path().join("fj"))
            .args(args)
            .output()
    };
    let out = run(vec!["--host", &bare, "api", "user"]).await.unwrap();
    assert!(
        out.status.success(),
        "lookup: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(vec!["auth", "login", "--host", &url, "--token", "new-pat"])
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "login: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let config: toml::Value = std::fs::read_to_string(&hosts).unwrap().parse().unwrap();
    let keys: Vec<_> = config["hosts"].as_table().unwrap().keys().collect();
    assert_eq!(keys, [&legacy], "exactly one entry, under the legacy key");
    let out = run(vec!["--host", &bare, "api", "user"]).await.unwrap();
    assert!(
        out.status.success(),
        "after login: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
