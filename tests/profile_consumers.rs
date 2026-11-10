#![cfg(unix)]
#[path = "support/profile_consumers.rs"]
mod consumers;
#[path = "support/fjord_profiles.rs"]
#[allow(dead_code)]
mod support;
#[path = "support/work_clone.rs"]
mod work_clone;
use consumers::*;
use support::*;
use wiremock::MockServer;
#[tokio::test]
async fn profile_consumers_share_metadata_and_isolate_mutes() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let log = browser(dir.path());
    std::env::set_current_dir(dir.path()).unwrap();
    let a = "a@test";
    let b = "b@test";
    for server in [&first, &second] {
        platform(server).await;
    }
    for (server, account, instance) in [
        (&first, a, "first-a"),
        (&first, b, "first-b"),
        (&second, a, "second-a"),
    ] {
        routes(server, account, instance).await;
        success(login(dir.path(), server, account).await);
        success(
            call(
                dir.path(),
                &[
                    "--host",
                    &server.uri(),
                    "--account",
                    account,
                    "instances",
                    "use",
                    instance,
                ],
            )
            .await,
        );
    }
    success(
        call(
            dir.path(),
            &["auth", "switch", &first.uri(), "--account", a],
        )
        .await,
    );
    let _direct_host_trap = work_clone::clone(dir.path()).await;
    // Legacy mute state must be retained without leaking into named profiles.
    std::fs::write(
        dir.path().join("muted_repos.toml"),
        format!("[accounts]\n'{}#legacy' = ['old/repo']\n", first.uri()),
    )
    .unwrap();
    for (server, account, instance) in [
        (&first, a, "first-a"),
        (&first, b, "first-b"),
        (&second, a, "second-a"),
    ] {
        let host = server.uri();
        let inbox = command_json(
            dir.path(),
            &["--host", &host, "--account", account, "status", "--json"],
        )
        .await;
        assert_eq!(inbox[0]["subject"]["title"].as_str(), Some(instance));
        let work = command_json(
            dir.path(),
            &[
                "--host",
                &host,
                "--account",
                account,
                "work",
                "context",
                "-R",
                "o/r",
                "--json",
            ],
        )
        .await;
        assert_eq!(work["account"]["fjord_instance"].as_str(), Some(instance));
        assert_eq!(
            work["account"]["fjord_platform"].as_str(),
            Some(host.as_str())
        );
        for command in ["issue", "pr", "release"] {
            success(
                call(
                    dir.path(),
                    &[
                        "--host",
                        &host,
                        "--account",
                        account,
                        command,
                        "list",
                        "-R",
                        "o/r",
                        "--web",
                    ],
                )
                .await,
            );
        }
        let captured = std::fs::read_to_string(&log).unwrap();
        for suffix in ["issues", "pulls", "releases"] {
            assert!(captured.contains(&format!("https://{instance}.example/base/o/r/{suffix}")));
        }
    }
    let implicit = command_json(dir.path(), &["work", "context", "--json"]).await;
    assert_eq!(implicit["api_reachable"], serde_json::json!(true));
    assert_eq!(
        implicit["account"]["fjord_instance"].as_str(),
        Some("first-a")
    );
    let resumed = command_json(dir.path(), &["work", "resume", "--json"]).await;
    assert_eq!(resumed["current"]["api_reachable"], serde_json::json!(true));
    assert_eq!(
        resumed["current"]["host"].as_str(),
        Some(first.uri().as_str())
    );
    success(call(dir.path(), &["issue", "list", "--web"]).await);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().last(),
        Some("https://first-a.example/base/o/r/issues")
    );
    let missing = call(
        dir.path(),
        &[
            "--host",
            &first.uri(),
            "--account",
            "missing@test",
            "issue",
            "list",
            "-R",
            "o/r",
            "--web",
        ],
    )
    .await;
    assert!(!missing.status.success());
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 10);
    work_clone::invalid_selection(dir.path()).await;
    mutes(dir.path(), &first, &second, a, b).await;
    let hosts: toml::Value = std::fs::read_to_string(dir.path().join("hosts.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(hosts["hosts"][first.uri()]["fjord_account"]
        .get("default_instance_id")
        .is_none());
    assert!(!call(
        dir.path(),
        &[
            "--host",
            &first.uri(),
            "--account",
            "missing@test",
            "status",
            "mute",
            "o/r"
        ]
    )
    .await
    .status
    .success());
}
