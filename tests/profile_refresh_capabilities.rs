#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;

use profile_support::{call, login, server};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn pat_refresh_publishes_upgraded_server_capabilities() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-new", "qa-a").await;
    login(dir.path(), "first.example", "qa-a", "tok-a").await;
    let switched = call(
        dir.path(),
        &["auth", "switch", "first.example", "--account", "qa-a"],
    )
    .await;
    assert!(switched.status.success());
    Mock::given(method("GET"))
        .and(path("/api/forgejo/v1/version"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "16.0.0"})),
        )
        .expect(4)
        .mount(&first)
        .await;

    let file = dir.path().join("fj/hosts.toml");
    for explicit in [false, true] {
        for replacement in [false, true] {
            let mut stale: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
            let host = stale["hosts"]["first.example"].as_table_mut().unwrap();
            host.insert("flavor".into(), "forgejo".into());
            host.insert("server_version".into(), "15.0.0".into());
            std::fs::write(&file, toml::to_string(&stale).unwrap()).unwrap();
            let mut args = Vec::new();
            if explicit {
                args.extend(["--account", "qa-a"]);
            }
            args.extend(["auth", "refresh"]);
            if replacement {
                args.extend(["--token", "tok-new"]);
            }
            let result = call(dir.path(), &args).await;
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let saved: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
            assert_eq!(
                saved["hosts"]["first.example"]["flavor"].as_str(),
                Some("forgejo")
            );
            assert_eq!(
                saved["hosts"]["first.example"]["server_version"].as_str(),
                Some("16.0.0")
            );
            assert_eq!(
                saved["hosts"]["second.example"],
                stale["hosts"]["second.example"]
            );
            assert_eq!(
                saved["accounts"]["first.example"]["default_account"],
                stale["accounts"]["first.example"]["default_account"]
            );
        }
    }
}
