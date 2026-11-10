use super::*;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn account_override_never_falls_back_to_storage() {
    let token = select_pat(
        true,
        Some("host-token".into()),
        Some("generic-token".into()),
        || panic!("storage must not be read"),
    )
    .unwrap();
    assert_eq!(token.as_deref(), Some("host-token"));
    assert_eq!(
        select_pat(true, None, Some("generic-token".into()), || panic!(
            "storage must not be read"
        ))
        .unwrap()
        .as_deref(),
        Some("generic-token")
    );
    assert!(select_pat(true, None, None, || panic!("storage must not be read")).is_err());
    assert_eq!(
        select_pat(false, None, None, || Ok(Some("stored-token".into())))
            .unwrap()
            .as_deref(),
        Some("stored-token")
    );
}

#[test]
fn account_debug_is_redacted() {
    let account: Account = "private-login".parse().unwrap();
    assert_eq!(format!("{account:?}"), "[redacted account]");
}

async fn client(server: &MockServer) -> Client {
    Client::for_base_url(
        Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        "override-pat".into(),
    )
    .unwrap()
    .with_account("qa-private")
}

#[tokio::test]
async fn account_override_verifies_once_and_uses_token_for_each_mutation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token override-pat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login":"qa-private"})))
        .expect(1)
        .mount(&server)
        .await;
    for route in [
        "repos/o/r/pulls/3/reviews",
        "repos/o/r/issues/3/comments",
        "repos/o/r/issues",
    ] {
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/{route}")))
            .and(header("authorization", "token override-pat"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = client(&server).await;
    for route in [
        "repos/o/r/pulls/3/reviews",
        "repos/o/r/issues/3/comments",
        "repos/o/r/issues",
    ] {
        assert_eq!(
            client
                .clone()
                .request(Method::POST, route, &[], Some(&json!({"body":"test"})))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
    }
    assert!(client
        .without_credentials()
        .request(Method::POST, "repos/o/r/issues", &[], None)
        .await
        .is_err());
}

#[tokio::test]
async fn account_override_rejects_wrong_identity_and_auth_failures_without_mutating() {
    for (status, body) in [
        (200, json!({"login":"operator-private"})),
        (401, json!({"message":"private-login override-pat"})),
        (200, json!({"unexpected":"identity"})),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let client = client(&server).await;
        let error = client
            .request(Method::POST, "repos/o/r/issues", &[], None)
            .await
            .unwrap_err();
        let message = format!("{error:?}");
        for private in [
            "qa-private",
            "operator-private",
            "override-pat",
            "private-login",
        ] {
            assert!(!message.contains(private), "{message}");
        }
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET"));
    }
}

#[tokio::test]
async fn account_override_never_attaches_token_off_host() {
    let server = MockServer::start().await;
    let external = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login":"qa-private"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/external"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&external)
        .await;
    client(&server)
        .await
        .request(
            Method::GET,
            &format!("{}/external", external.uri()),
            &[],
            None,
        )
        .await
        .unwrap();
    assert!(!external.received_requests().await.unwrap()[0]
        .headers
        .contains_key("authorization"));
}

#[tokio::test]
async fn account_override_blocks_unverified_raw_http_auth() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    assert!(client.auth_header_value().is_err());
    assert!(client.without_credentials().auth_header_value().is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn account_override_rejects_custom_authorization_before_verification() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    for name in ["authorization", "x-forge-authorization"] {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_static("token other-pat"),
        );
        assert!(client
            .request_with_headers(Method::POST, "repos/o/r/issues", &[], None, &headers)
            .await
            .is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}
