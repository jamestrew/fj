//! The inbox filters with the mute list of the account the client connected as.
#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn thread(id: u64, repo: &str) -> serde_json::Value {
    json!({
        "id": id, "unread": true, "pinned": false,
        "updated_at": "2026-11-12T00:00:00Z",
        "url": format!("http://x/{id}"), "html_url": "",
        "subject": {"title": format!("t{id}"), "type": "Issue", "state": "open"},
        "repository": {"full_name": repo, "html_url": ""},
    })
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test]
async fn the_inbox_hides_what_the_connected_account_muted_and_only_that() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-b", "qa-b").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([thread(1, "o/muted"), thread(2, "o/open")])),
        )
        .mount(&first)
        .await;

    // qa-a mutes o/muted; qa-b has no mutes.
    let mute = call(d, &["--account", "qa-a", "status", "mute", "o/muted"]).await;
    assert!(mute.status.success(), "{}", out(&mute));

    let a = call(d, &["--account", "qa-a", "status", "--json"]).await;
    assert!(a.status.success(), "{}", out(&a));
    let items: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    let repos: Vec<_> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["repository"]["full_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(repos, ["o/open"], "qa-a's mute hides o/muted for qa-a");
    assert!(
        out(&a).contains("hidden"),
        "the hidden note is shown: {}",
        out(&a)
    );

    let b = call(d, &["--account", "qa-b", "status", "--json"]).await;
    assert!(b.status.success(), "{}", out(&b));
    let items: serde_json::Value = serde_json::from_slice(&b.stdout).unwrap();
    assert_eq!(items.as_array().unwrap().len(), 2, "qa-b sees both threads");
}
