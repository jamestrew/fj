#[path = "support/fjord_profiles.rs"]
mod support;
use serde_json::json;
use support::*;
use wiremock::{
    matchers::{body_string_contains, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn named_listing_errors_suppress_provider_bodies_before_and_after_refresh() {
    for command in [vec!["instances", "list"], vec!["instances", "use", "mine"]] {
        for refresh in [false, true] {
            for status in [403, 503] {
                let server = MockServer::start().await;
                let dir = tempfile::tempdir().unwrap();
                let account = "private@example.test";
                platform(&server).await;
                identity(&server, &token(account, 0), "mine").await;
                success(login(dir.path(), &server, account).await);
                let before = std::fs::read(dir.path().join("hosts.toml")).unwrap();
                let private = "private provider account diagnostics";
                Mock::given(method("GET"))
                    .and(path("/api/v1/me/forge-instances"))
                    .and(header(
                        "authorization",
                        format!("Bearer {}", token(account, 0)),
                    ))
                    .respond_with(
                        ResponseTemplate::new(if refresh { 401 } else { status })
                            .set_body_string(private),
                    )
                    .with_priority(1)
                    .expect(1)
                    .mount(&server)
                    .await;
                if refresh {
                    Mock::given(method("POST"))
                        .and(path("/oauth/token"))
                        .and(body_string_contains("grant_type=refresh_token"))
                        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                            "access_token":token(account,1),"refresh_token":"rotated"
                        })))
                        .expect(1)
                        .mount(&server)
                        .await;
                    // Refresh verifies the new bearer before retrying the listing.
                    let calls = std::sync::atomic::AtomicUsize::new(0);
                    Mock::given(method("GET"))
                        .and(path("/api/v1/me/forge-instances"))
                        .and(header(
                            "authorization",
                            format!("Bearer {}", token(account, 1)),
                        ))
                        .respond_with(move |_: &wiremock::Request| {
                            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                                ResponseTemplate::new(200).set_body_json(json!({"instances":[]}))
                            } else {
                                ResponseTemplate::new(status).set_body_string(private)
                            }
                        })
                        .expect(2)
                        .mount(&server)
                        .await;
                }
                let uri = server.uri();
                let mut args = vec!["--debug", "--host", &uri, "--account", account];
                args.extend(command.iter().copied());
                let out = call(dir.path(), &args).await;
                assert!(!out.status.success());
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    stderr.contains("profile instance listing failed"),
                    "{stderr}"
                );
                assert!(!stderr.contains(private), "{stderr}");
                assert!(!String::from_utf8_lossy(&out.stdout).contains(private));
                assert_eq!(
                    std::fs::read(dir.path().join("hosts.toml")).unwrap(),
                    before
                );
                server.verify().await;
            }
        }
    }
}
