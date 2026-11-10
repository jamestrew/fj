use super::*;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn account_override_rejects_query_credentials_before_verification() {
    let server = MockServer::start().await;
    let client = Client::for_base_url(
        Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        "qa-pat".into(),
    )
    .unwrap()
    .with_account("qa");
    for name in [
        "token",
        "access_token",
        "sudo",
        "ToKeN",
        "ACCESS_TOKEN",
        "%74oken",
    ] {
        let endpoint = format!("repos/o/r/issues?{name}=private-operator-pat");
        let error = client
            .request(Method::POST, &endpoint, &[], None)
            .await
            .unwrap_err();
        assert!(!format!("{error:?}").contains("private-operator-pat"));
        if !name.starts_with('%') {
            let query = vec![(name.into(), "private-operator-pat".into())];
            let error = client
                .request(Method::POST, "repos/o/r/issues", &query, None)
                .await
                .unwrap_err();
            assert!(!format!("{error:?}").contains("private-operator-pat"));
        }
    }
    assert!(query::check(
        &client,
        "repos/o/r/issues?client_secret=filter",
        &[("api_key".into(), "filter".into())],
        &HeaderMap::new()
    )
    .is_ok());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn account_override_fresh_multipart_adapters_verify_identity_first() {
    for release in [false, true] {
        for matching in [false, true] {
            let server = MockServer::start().await;
            let client = Client::for_base_url(
                Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
                "qa-pat".into(),
            )
            .unwrap()
            .with_account("qa");
            Mock::given(method("GET"))
                .and(path("/api/v1/user"))
                .and(header("authorization", "token qa-pat"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"login":if matching {"qa"} else {"operator"}})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let kind = if release { "releases" } else { "issues" };
            Mock::given(method("POST")).and(path(format!("/api/v1/repos/o/r/{kind}/42/assets")))
                .and(header("authorization", "token qa-pat"))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":1,"name":"shot.png","size":4,"browser_download_url":"https://example/shot.png","created_at":"2026-11-10T00:00:00Z"})))
                .expect(if matching {1} else {0}).mount(&server).await;
            let uploaded = if release {
                crate::api::release::upload_asset(
                    &client,
                    "o",
                    "r",
                    42,
                    "shot.png",
                    b"data".to_vec(),
                )
                .await
                .map(|a| a.id)
            } else {
                crate::api::issue::upload_asset(&client, "o", "r", 42, "shot.png", b"data".to_vec())
                    .await
                    .map(|a| a.id)
            };
            assert_eq!(uploaded.is_ok(), matching);
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), if matching { 2 } else { 1 });
            assert_eq!(requests[0].url.path(), "/api/v1/user");
            if matching {
                assert!(requests[1].headers["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("multipart/form-data"));
            }
        }
    }
}

#[tokio::test]
async fn account_override_rejects_form_auth_before_verification() {
    let server = MockServer::start().await;
    let client = Client::for_base_url(
        Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        "qa-pat".into(),
    )
    .unwrap()
    .with_account("qa");
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("Application/X-Www-Form-Urlencoded; charset=utf-8"),
    );
    let body = json!("&token=private-operator-pat&");
    let error = client
        .request_with_headers(Method::POST, "repos/o/r/issues", &[], Some(&body), &headers)
        .await
        .unwrap_err();
    assert!(!format!("{error:?}").contains("private-operator-pat"));
    assert!(server.received_requests().await.unwrap().is_empty());
}
