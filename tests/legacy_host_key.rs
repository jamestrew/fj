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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let log = dir.path().join("browser.log");
        for program in ["open", "xdg-open"] {
            let file = bin.join(program);
            std::fs::write(
                &file,
                "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$FJ_BROWSER_LOG\"\n",
            )
            .unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fj"))
            .env("FJ_CONFIG_DIR", dir.path().join("fj"))
            .env("FJ_NO_KEYCHAIN", "1")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("FJ_BROWSER_LOG", &log)
            .current_dir(dir.path())
            .args(["--host", &bare, "issue", "list", "-R", "o/r", "--web"])
            .output()
            .await
            .unwrap();
        success(&out);
        assert_eq!(
            std::fs::read_to_string(log).unwrap().trim(),
            format!("{url}/o/r/issues")
        );
    }
    // Discovery must treat the legacy key as a gateway, not a PAT remote filter.
    let original = std::fs::read_to_string(&hosts).unwrap();
    // The gateway authenticates with the platform-scoped Fjord session, not the PAT.
    let tokens = dir.path().join("fj/tokens.toml");
    let mut stored = std::fs::read_to_string(&tokens).unwrap();
    stored.push_str(&format!("'fjord-account:{url}' = 'old-pat'\n"));
    std::fs::write(&tokens, stored).unwrap();
    std::fs::write(&hosts, format!(
        "current = '{legacy}'\n[hosts.'{legacy}']\nauth_kind = 'fjord_account'\n[hosts.'{legacy}'.fjord_account]\nplatform_url = '{url}'\ndefault_instance_id = 'instance'\ndefault_instance_public_url = 'https://instance.example'\n"
    )).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://instance.example/o/r.git",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/forge-gateway/instance/api/v1/repos/o/r/issues",
        ))
        .and(header("authorization", "Bearer old-pat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let out = run(vec!["--host", &bare, "issue", "list", "--json"])
        .await
        .unwrap();
    success(&out);
    std::fs::write(&hosts, original).unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id":1,"unread":true,"pinned":false,"updated_at":"2026-11-10T00:00:00Z","url":"",
                "subject":{"title":"hidden","type":"Issue","state":"open"},
                "repository":{"full_name":"o/r"}
            }])),
        )
        .mount(&server)
        .await;
    let out = run(vec!["--host", &bare, "status", "mute", "o/r"])
        .await
        .unwrap();
    success(&out);
    for host in [&bare, &legacy] {
        let out = run(vec!["--host", host, "status", "muted", "--json"])
            .await
            .unwrap();
        success(&out);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            serde_json::json!(["o/r"])
        );
        let out = run(vec!["--host", host, "status", "--json"]).await.unwrap();
        success(&out);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            serde_json::json!([])
        );
    }
    let out = run(vec!["--host", &legacy, "status", "unmute", "o/r"])
        .await
        .unwrap();
    success(&out);
    let out = run(vec!["--host", &bare, "status", "muted", "--json"])
        .await
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!([])
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

fn success(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
