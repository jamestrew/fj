//! Integration tests for the HTTP client against a local wiremock server.
//! Lives inline (not under `tests/`) because our crate is a binary, not a
//! library — `tests/` can't `use crate::client::...`.

#![cfg(test)]

use reqwest::Method;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::Client;

async fn client_for(server: &MockServer) -> Client {
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base url");
    Client::for_base_url(base, "test-token".into()).expect("client")
}

#[tokio::test]
async fn auth_header_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "login": "alice"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect("ok");
    assert_eq!(res["login"], "alice");
}

#[tokio::test]
async fn retries_on_5xx_and_succeeds() {
    let server = MockServer::start().await;
    // First two attempts: 503. Third: 200.
    Mock::given(method("GET"))
        .and(path("/api/v1/flaky"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/flaky"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/flaky", &[], None::<&()>)
        .await
        .expect("eventually ok");
    assert_eq!(res["ok"], true);
}

#[tokio::test]
async fn does_not_retry_post_on_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/once"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = c
        .json::<serde_json::Value, _>(Method::POST, "/api/v1/once", &[], Some(&json!({})))
        .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn maps_401_to_friendly_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"message": "Bad token"})))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = c
        .json::<serde_json::Value, _>(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect_err("should fail");
    let text = format!("{err:#}");
    assert!(text.contains("401"), "got: {text}");
    assert!(
        text.to_lowercase().contains("authentication") || text.to_lowercase().contains("token"),
        "got: {text}"
    );
}

#[tokio::test]
async fn custom_headers_forwarded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/echo"))
        .and(header("x-trace-id", "abc-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"seen": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-trace-id", "abc-123".parse().unwrap());
    let res = c
        .request_with_headers(Method::GET, "/api/v1/echo", &[], None, &h)
        .await
        .expect("ok");
    assert!(res.status().is_success());
}

#[tokio::test]
async fn get_page_handles_null_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/things"))
        .respond_with(ResponseTemplate::new(200).set_body_string("null"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page: super::Page<serde_json::Value> = c
        .get_page("/api/v1/things", &[])
        .await
        .expect("null tolerated");
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn get_all_follows_link_rel_next() {
    let server = MockServer::start().await;
    // First page: 50 items, Link: <…?page=2>; rel="next"
    let page1: Vec<i32> = (1..=50).collect();
    let page2: Vec<i32> = (51..=80).collect();
    let next_url = format!("{}/api/v1/items?page=2&limit=50", server.uri());
    // Page 2 mock first (more specific). wiremock matches in LIFO order, so
    // we mount the more specific mock LAST to make sure it wins.
    Mock::given(method("GET"))
        .and(path("/api/v1/items"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/items"))
        .and(query_param("limit", "50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page1)
                .insert_header("Link", format!(r#"<{next_url}>; rel="next""#).as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let items: Vec<i32> = c
        .get_all("/api/v1/items", &[], 100)
        .await
        .expect("paginated ok");
    assert_eq!(items.len(), 80);
    assert_eq!(items.first(), Some(&1));
    assert_eq!(items.last(), Some(&80));
}

#[tokio::test]
async fn get_all_respects_total_limit() {
    let server = MockServer::start().await;
    let big: Vec<i32> = (1..=50).collect();
    let next_url = format!("{}/api/v1/items?page=2&limit=50", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/items"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(big)
                .insert_header("Link", format!(r#"<{next_url}>; rel="next""#).as_str()),
        )
        .expect(1) // exactly one call: the limit caps the walk early
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let items: Vec<i32> = c.get_all("/api/v1/items", &[], 30).await.expect("ok");
    assert_eq!(items.len(), 30);
}

#[tokio::test]
async fn header_value_rejects_bad_token() {
    let server = MockServer::start().await;
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).unwrap();
    let bad = Client::for_base_url(base, "tok\nen".into()).expect("constructed");
    let err = bad
        .json::<serde_json::Value, _>(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect_err("bad token should not panic");
    let text = format!("{err:#}");
    assert!(text.contains("clean token"), "got: {text}");
}
