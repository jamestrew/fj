#[path = "support/fjord_profiles.rs"]
mod support;
use serde_json::json;
use support::*;
use wiremock::{
    matchers::{body_string_contains, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn fjord_profiles_isolate_login_instances_refresh_and_failed_publication() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    for s in [&first, &second] {
        platform(s).await;
    }
    let a = "private-a@example.test";
    let b = "private-b@example.test";
    identity(&first, &token(a, 0), "instance-a").await;
    identity(&first, &token(b, 0), "instance-b").await;
    identity(&first, &token(a, 1), "instance-a").await;
    identity(&second, &token(a, 0), "second-a").await;
    std::fs::write(
        dir.path().join("hosts.toml"),
        format!(
            r#"current = "{}"
[hosts."{}"]
user = "{}"
git_protocol = "https"
auth_kind = "fjord_account"
[hosts."{}".fjord_account]
platform_url = "{}"
default_instance_id = "legacy-instance"
"#,
            first.uri(),
            first.uri(),
            a,
            first.uri(),
            first.uri()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("tokens.toml"),
        "[tokens]\nlegacy = 'retained'\n",
    )
    .unwrap();
    success(login(dir.path(), &first, a).await);
    let initial = config(dir.path());
    assert_eq!(
        initial["accounts"][first.uri()]["default_account"].as_str(),
        Some(a)
    );
    success(login(dir.path(), &first, b).await);
    success(login(dir.path(), &second, a).await);
    let after = config(dir.path());
    assert_eq!(after["current"], initial["current"]);
    assert_eq!(after["hosts"][first.uri()], initial["hosts"][first.uri()]);
    assert_eq!(
        after["accounts"][first.uri()]["default_account"].as_str(),
        Some(a)
    );
    for (server, user, id) in [
        (&first, a, "instance-a"),
        (&first, b, "instance-b"),
        (&second, a, "second-a"),
    ] {
        success(
            call(
                dir.path(),
                &[
                    "--host",
                    &server.uri(),
                    "--account",
                    user,
                    "instances",
                    "use",
                    id,
                ],
            )
            .await,
        );
    }
    let selected = config(dir.path());
    assert_eq!(
        selected["accounts"][first.uri()]["profiles"][a]["fjord"]["default_instance_id"].as_str(),
        Some("instance-a")
    );
    assert_eq!(
        selected["accounts"][first.uri()]["profiles"][b]["fjord"]["default_instance_id"].as_str(),
        Some("instance-b")
    );
    assert_eq!(
        selected["accounts"][second.uri()]["profiles"][a]["fjord"]["default_instance_id"].as_str(),
        Some("second-a")
    );
    assert_eq!(
        selected["hosts"][first.uri()]["fjord_account"]["default_instance_id"].as_str(),
        Some("legacy-instance")
    );
    for (bearer, id, status) in [
        (token(a, 0), "instance-a", 401),
        (token(a, 1), "instance-a", 201),
        (token(b, 0), "instance-b", 201),
    ] {
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/forge-gateway/{id}/api/v1/repos/o/r/issues"
            )))
            .and(header("authorization", format!("Bearer {bearer}")))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({"ok":true})))
            .mount(&first)
            .await;
    }
    expire(&first, &token(a, 0)).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains(
            "refresh_token=refresh-private-a%40example.test",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token":token(a,1),"refresh_token":"rotated-a"})),
        )
        .expect(1)
        .mount(&first)
        .await;
    let post = [
        "--host",
        &first.uri(),
        "--account",
        a,
        "api",
        "repos/o/r/issues",
        "-X",
        "POST",
    ];
    let (one, two) = tokio::join!(call(dir.path(), &post), call(dir.path(), &post));
    success(one);
    success(two);
    success(
        call(
            dir.path(),
            &[
                "--host",
                &first.uri(),
                "--account",
                b,
                "api",
                "repos/o/r/issues",
                "-X",
                "POST",
            ],
        )
        .await,
    );
    let secrets = std::fs::read_to_string(dir.path().join("tokens.toml")).unwrap();
    assert!(secrets.contains("rotated-a"));
    assert!(secrets.contains("refresh-private-b@example.test"));
    assert!(secrets.contains("retained"));
    let before = std::fs::read(dir.path().join("hosts.toml")).unwrap();
    let rejected = login(dir.path(), &first, "wrong@example.test").await;
    assert!(!rejected.status.success());
    assert_eq!(
        std::fs::read(dir.path().join("hosts.toml")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tokens.toml")).unwrap(),
        secrets
    );
    let status = call(dir.path(), &["auth", "status"]).await;
    assert!(status.status.success());
    for private in [a, b, &token(a, 1), "rotated-a"] {
        assert!(!String::from_utf8_lossy(&status.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&status.stderr).contains(private));
    }
    // Token-store failure after successful verification cannot publish a descriptor.
    let failed = tempfile::tempdir().unwrap();
    std::fs::create_dir(failed.path().join("tokens.toml")).unwrap();
    assert!(!login(failed.path(), &first, a).await.status.success());
    assert!(!failed.path().join("hosts.toml").exists());
    assert!(!call(
        dir.path(),
        &[
            "--host",
            &first.uri(),
            "--account",
            "missing@example.test",
            "instances",
            "list"
        ]
    )
    .await
    .status
    .success());
}
