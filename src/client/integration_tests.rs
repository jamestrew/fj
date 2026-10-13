//! Integration tests for the HTTP client against a local wiremock server.
//! Lives inline (not under `tests/`) because our crate is a binary, not a
//! library — `tests/` can't `use crate::client::...`.

#![cfg(test)]

use reqwest::{Method, StatusCode};
use serde_json::json;
use url::Url;
use wiremock::matchers::{body_json, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Capability, Client, Flavor, PatTransport, ProxyCredential, ServerInfo, Support};

async fn client_for(server: &MockServer) -> Client {
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base url");
    Client::for_base_url(base, "test-token".into()).expect("client")
}

/// A client whose server flavor and version are already known, so a test does
/// not have to mock the version routes to reach a particular gate.
async fn pinned_client(server: &MockServer, flavor: Flavor, version: &str) -> Client {
    client_for(server)
        .await
        .with_server_info(ServerInfo::new(flavor, version))
}

/// Matches a request that carries no `Authorization` header at all.
fn no_authorization(req: &wiremock::Request) -> bool {
    !req.headers.contains_key("authorization")
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
async fn anonymous_client_omits_auth_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .and(|req: &wiremock::Request| !req.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": "15.0.6+gitea-1.22.0"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).expect("base url");
    let c = Client::anonymous_for_base_url(base).expect("anonymous client");
    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/version", &[], None::<&()>)
        .await
        .expect("ok");
    assert_eq!(res["version"], "15.0.6+gitea-1.22.0");
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
async fn maps_403_to_friendly_error_and_exit_code() {
    let err = api_error_for_status(
        StatusCode::FORBIDDEN,
        Method::GET,
        "/api/v1/repos/o/r/pulls/12",
        json!({"message": "token lacks scope"}),
    )
    .await;
    let headline = err.to_string();
    assert!(headline.contains("forbidden: o/r #12"), "got: {headline}");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_AUTH_FORBIDDEN);
}

#[tokio::test]
async fn maps_404_to_friendly_error_and_exit_code() {
    let err = api_error_for_status(
        StatusCode::NOT_FOUND,
        Method::GET,
        "/api/v1/repos/o/missing",
        json!({"message": "not found"}),
    )
    .await;
    let headline = err.to_string();
    assert_eq!(headline, "not found: o/missing");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_NOT_FOUND);
}

#[tokio::test]
async fn maps_429_to_friendly_error_and_exit_code() {
    let err = api_error_for_status(
        StatusCode::TOO_MANY_REQUESTS,
        Method::POST,
        "/api/v1/repos/o/r/issues/7",
        json!({"message": "slow down"}),
    )
    .await;
    let headline = err.to_string();
    assert_eq!(headline, "rate limited: retry later");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
    assert_eq!(
        crate::error_exit_code(&err),
        crate::EXIT_RATE_LIMIT_TRANSIENT
    );
}

#[tokio::test]
async fn maps_409_to_friendly_error_and_exit_code() {
    // A conflict (e.g. branch already exists) surfaces the server's message,
    // not the raw API URL. 409 is a request problem, so the exit code stays the
    // generic 1 (not the not-found / auth / transient classes).
    let err = api_error_for_status(
        StatusCode::CONFLICT,
        Method::POST,
        "/api/v1/repos/o/r/branches",
        json!({"message": "The branch already exists."}),
    )
    .await;
    let headline = err.to_string();
    assert_eq!(headline, "conflict: o/r: The branch already exists.");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_GENERIC);
}

#[tokio::test]
async fn maps_422_to_friendly_error_and_exit_code() {
    let err = api_error_for_status(
        StatusCode::UNPROCESSABLE_ENTITY,
        Method::POST,
        "/api/v1/repos/o/r/pulls",
        json!({"message": "head and base are the same"}),
    )
    .await;
    let headline = err.to_string();
    assert_eq!(headline, "invalid request: o/r: head and base are the same");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_GENERIC);
}

async fn api_error_for_status(
    status: StatusCode,
    request_method: Method,
    request_path: &str,
    body: serde_json::Value,
) -> anyhow::Error {
    let server = MockServer::start().await;
    Mock::given(method(request_method.as_str()))
        .and(path(request_path))
        .respond_with(ResponseTemplate::new(status.as_u16()).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    c.json::<serde_json::Value, _>(request_method, request_path, &[], Some(&json!({})))
        .await
        .expect_err("status should fail")
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
async fn get_all_handles_null_body() {
    // Mirrors `get_page_handles_null_body`: an endpoint that answers an empty
    // collection with `null` must yield an empty Vec, not a decode error, even
    // on the multi-page `get_all` path (regression: it used to `res.json()`
    // straight into a Vec and choke on `null`).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/items"))
        .respond_with(ResponseTemplate::new(200).set_body_string("null"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let items: Vec<i32> = c
        .get_all("/api/v1/items", &[], 100)
        .await
        .expect("null tolerated");
    assert!(items.is_empty());
}

#[tokio::test]
async fn honors_retry_after_on_429() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/rate"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(json!({"message": "slow down"})),
        )
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/rate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/rate", &[], None::<&()>)
        .await
        .expect("eventually ok");
    assert_eq!(res["ok"], true);
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

#[tokio::test]
async fn protect_get_tolerates_null_arrays() {
    // Forgejo returns `null` (not `[]`) for status_check_contexts and
    // push_whitelist_usernames when unset; deserialization must not fail.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "branch_name": "main",
            "enable_status_check": false,
            "status_check_contexts": null,
            "push_whitelist_usernames": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let p = crate::api::protect::get(&c, "o", "r", "main")
        .await
        .expect("null arrays tolerated");
    assert!(p.status_check_contexts.is_empty());
    assert!(p.push_whitelist_usernames.is_empty());
}

#[tokio::test]
async fn workflow_org_secrets_and_variables_use_org_actions_paths() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/team/actions/secrets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "name": "REGISTRY_TOKEN",
                "created_at": "2026-06-01T00:00:00Z"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/orgs/team/actions/secrets/REGISTRY_TOKEN"))
        .and(body_json(json!({"data": "example-value"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orgs/team/actions/secrets/REGISTRY_TOKEN"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/team/actions/variables"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"name": "IMAGE", "data": "registry.example/app", "owner_id": 4, "repo_id": 0}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    // `set` looks the variable up (it exists, so PUT), then re-reads it.
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/team/actions/variables/IMAGE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(
            {"name": "IMAGE", "data": "registry.example/app", "owner_id": 4, "repo_id": 0}
        )))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/orgs/team/actions/variables/IMAGE"))
        .and(body_json(json!({"value": "registry.example/app"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orgs/team/actions/variables/IMAGE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let secrets = crate::api::workflow::list_org_secrets(&c, "team")
        .await
        .expect("org secrets list");
    assert_eq!(secrets[0].name, "REGISTRY_TOKEN");
    crate::api::workflow::set_org_secret(&c, "team", "REGISTRY_TOKEN", "example-value")
        .await
        .expect("org secret set");
    crate::api::workflow::delete_org_secret(&c, "team", "REGISTRY_TOKEN")
        .await
        .expect("org secret delete");

    let variables = crate::api::workflow::list_org_variables(&c, "team")
        .await
        .expect("org variables list");
    assert_eq!(variables[0].name, "IMAGE");
    let outcome =
        crate::api::workflow::set_org_variable(&c, "team", "IMAGE", "registry.example/app")
            .await
            .expect("org variable set");
    assert_eq!(outcome.write, crate::api::workflow::VariableWrite::Updated);
    assert_eq!(
        outcome.variable.scope(),
        crate::api::workflow::VariableScope::Owner
    );
    crate::api::workflow::delete_org_variable(&c, "team", "IMAGE")
        .await
        .expect("org variable delete");
}

#[tokio::test]
async fn maps_org_403_to_clear_error() {
    let err = api_error_for_status(
        StatusCode::FORBIDDEN,
        Method::GET,
        "/api/v1/orgs/team/actions/secrets",
        json!({"message": "token lacks scope"}),
    )
    .await;
    let headline = err.to_string();
    assert!(headline.contains("forbidden: org team"), "got: {headline}");
    assert!(!headline.contains("/api/v1"), "got: {headline}");
}

// --- Fjord Account (Phase 6) flows ---

/// `POST /api/v1/sign-in` happy path: the platform returns `kind:success`
/// with an access token + user info. We assert the request shape and the
/// decoded enum variant.
#[tokio::test]
async fn fjord_sign_in_success() {
    use crate::fjord::{sign_in, SignInRequest, SignInResponse};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sign-in"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "success",
            "accessToken": "tok-abc",
            "refreshToken": "rtok-def",
            "accessTokenExp": 1_900_000_000,
            "user": {
                "userId": "usr_1",
                "email": "alice@example.com",
                "role": "owner"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let res = sign_in(
        &server.uri(),
        &SignInRequest {
            email: "alice@example.com",
            password: "hunter2",
            totp_code: None,
            recovery_code: None,
        },
    )
    .await
    .expect("sign-in ok");
    match res {
        SignInResponse::Success {
            access_token, user, ..
        } => {
            assert_eq!(access_token, "tok-abc");
            assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[tokio::test]
async fn fjord_sign_in_html_response_points_at_platform_url() {
    use crate::fjord::{sign_in, SignInRequest};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sign-in"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<!DOCTYPE html><title>Sign in</title>"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let err = sign_in(
        &server.uri(),
        &SignInRequest {
            email: "alice@example.com",
            password: "hunter2",
            totp_code: None,
            recovery_code: None,
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("HTML page"), "got: {err}");
    assert!(err.contains("--platform-url"), "got: {err}");
}

#[test]
fn fjord_default_platform_url_is_production() {
    assert_eq!(crate::fjord::DEFAULT_PLATFORM_URL, "https://fjord.sh");
}

/// `POST /api/v1/sign-in` returns `kind:totp-required` when the password
/// is correct but the account has TOTP enrolled. The CLI must NOT treat
/// this as success.
#[tokio::test]
async fn fjord_sign_in_totp_required() {
    use crate::fjord::{sign_in, SignInRequest, SignInResponse};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sign-in"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "totp-required",
            "challengeToken": "chal_xyz",
            "message": "Enter your 6-digit TOTP."
        })))
        .expect(1)
        .mount(&server)
        .await;

    let res = sign_in(
        &server.uri(),
        &SignInRequest {
            email: "alice@example.com",
            password: "hunter2",
            totp_code: None,
            recovery_code: None,
        },
    )
    .await
    .expect("sign-in ok");
    assert!(matches!(res, SignInResponse::TotpRequired { .. }));
}

/// `POST /api/v1/sign-in` returns `kind:invalid-credentials` with HTTP 401
/// when the email or password is wrong. The decoder must still produce the
/// typed variant rather than failing.
#[tokio::test]
async fn fjord_sign_in_invalid_credentials() {
    use crate::fjord::{sign_in, SignInRequest, SignInResponse};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sign-in"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "kind": "invalid-credentials",
            "message": "Email or password is incorrect."
        })))
        .expect(1)
        .mount(&server)
        .await;
    let res = sign_in(
        &server.uri(),
        &SignInRequest {
            email: "alice@example.com",
            password: "nope",
            totp_code: None,
            recovery_code: None,
        },
    )
    .await
    .expect("body must still decode despite 401");
    match res {
        SignInResponse::InvalidCredentials { message } => {
            assert!(message.contains("incorrect"));
        }
        other => panic!("expected InvalidCredentials, got {other:?}"),
    }
}

/// `GET /api/v1/me/forge-instances` happy path: two instances returned,
/// bearer header forwarded verbatim.
#[tokio::test]
async fn fjord_list_instances_happy_path() {
    use crate::fjord::list_instances;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header("authorization", "Bearer alice-bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instances": [
                {
                    "id": "inst_1",
                    "displayName": "Primary",
                    "slug": "primary",
                    "region": "us-east-1",
                    "plan": "pro",
                    "state": "ready",
                    "createdAt": "2026-05-01T00:00:00Z",
                    "publicUrl": "https://primary.forgejo.example"
                },
                {
                    "id": "inst_2",
                    "displayName": "Sandbox",
                    "slug": "sandbox",
                    "region": "us-west-2",
                    "plan": "free",
                    "state": "provisioning",
                    "createdAt": "2026-05-12T00:00:00Z",
                    "publicUrl": "https://sandbox.forgejo.example"
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let instances = list_instances(&server.uri(), "alice-bearer")
        .await
        .expect("ok");
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].id, "inst_1");
    assert_eq!(instances[0].slug, "primary");
    assert_eq!(instances[1].state.as_deref(), Some("provisioning"));
}

/// `GET /api/v1/me/commons`, joined: the Commons becomes a switchable
/// instance carrying the gateway forge id, which is the id every later call
/// routes through. Field-for-field the same synthesis fjord-android does in
/// `account/ForgeInstance.kt`.
#[tokio::test]
async fn fjord_commons_joined_becomes_an_instance() {
    use crate::fjord::fetch_commons;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .and(header("authorization", "Bearer alice-bearer"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "commons": {
                "url": "https://commons.fjord.host",
                "name": "Fjord Commons",
                "memberCount": 42,
                "gatewayForgeId": "forge-commons-1",
                "membership": { "joined": true, "namespace": "alice" }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let commons = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect("ok")
        .expect("commons present");
    let instance = commons
        .to_switchable_instance()
        .expect("joined commons is switchable");
    assert_eq!(instance.id, "forge-commons-1");
    assert_eq!(instance.slug, "commons");
    assert_eq!(instance.display_name, "Fjord Commons");
    assert_eq!(
        instance.public_url.as_deref(),
        Some("https://commons.fjord.host")
    );
    assert_eq!(instance.state.as_deref(), Some("ready"));
}

/// Not joined: the account can see that a Commons exists and cannot use it,
/// so it must NOT become an instance. Offering it would turn a clear "you have
/// not joined" into a 403 from the gateway at the moment the user tries to open
/// a PR, which is the worst place to find out.
#[tokio::test]
async fn fjord_commons_not_joined_is_not_switchable() {
    use crate::fjord::fetch_commons;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "commons": {
                "url": "https://commons.fjord.host",
                "name": "Fjord Commons",
                "gatewayForgeId": "forge-commons-1",
                "membership": { "joined": false }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let commons = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect("ok")
        .expect("commons present");
    assert!(!commons.membership.joined);
    // The information survives for the caller's message even though the
    // instance does not.
    assert!(commons.to_switchable_instance().is_none());
    assert_eq!(commons.url, "https://commons.fjord.host");
}

/// Joined but with no gateway id: the platform knows about the Commons and
/// cannot route to it yet. Same refusal, since an instance with no id has no
/// gateway path to construct.
#[tokio::test]
async fn fjord_commons_without_gateway_id_is_not_switchable() {
    use crate::fjord::fetch_commons;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "commons": {
                "url": "https://commons.fjord.host",
                "name": "Fjord Commons",
                "gatewayForgeId": "",
                "membership": { "joined": true }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let commons = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect("ok")
        .expect("commons present");
    assert!(commons.to_switchable_instance().is_none());
}

/// A platform older than this endpoint, or one with the Commons feature off,
/// answers 404. That is "no Commons here", which is today's behaviour, so it
/// must be `Ok(None)` and not an error: `fj instances` has to keep working
/// against an older platform.
#[tokio::test]
async fn fjord_commons_404_is_none_not_an_error() {
    use crate::fjord::fetch_commons;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    let commons = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect("404 must not be an error");
    assert!(commons.is_none());
}

/// A 200 with `{"commons": null}` (feature on, nothing to report) is also None
/// rather than a decode failure.
#[tokio::test]
async fn fjord_commons_null_body_is_none() {
    use crate::fjord::fetch_commons;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "commons": null })))
        .expect(1)
        .mount(&server)
        .await;
    let commons = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect("ok");
    assert!(commons.is_none());
}

/// A 401 surfaces as the bare `SessionExpired` marker, the same contract
/// `list_instances` has, so the caller can refresh once and retry rather than
/// showing the user an error for an expired session it could have renewed.
#[tokio::test]
async fn fjord_commons_401_is_session_expired() {
    use crate::fjord::{fetch_commons, SessionExpired};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/commons"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .expect(1)
        .mount(&server)
        .await;
    let err = fetch_commons(&server.uri(), "alice-bearer")
        .await
        .expect_err("401 should error");
    assert!(err.downcast_ref::<SessionExpired>().is_some());
}

/// The gateway path built for the Commons is the ordinary one. Nothing about
/// the Commons is special after synthesis, which is the point: `fj api` and
/// `fj pr create` need no Commons-specific branch.
#[test]
fn commons_routes_through_the_ordinary_gateway_path() {
    use crate::fjord::gateway_base_url;
    let url = gateway_base_url("https://platform.example", "forge-commons-1").expect("url");
    assert_eq!(
        url.as_str(),
        "https://platform.example/api/v1/forge-gateway/forge-commons-1/api/v1/"
    );
}

/// Paragon's 404 (no instances provisioned yet) maps to an empty list
/// rather than an error, so the CLI can print a friendly "no instances yet"
/// message rather than an exception trace.
#[tokio::test]
async fn fjord_list_instances_404_is_empty() {
    use crate::fjord::list_instances;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    let instances = list_instances(&server.uri(), "alice-bearer")
        .await
        .expect("404 should map to empty list");
    assert!(instances.is_empty());
}

/// When a `Client` is constructed with FjordBearer auth and the gateway
/// base URL, every Forgejo API call lands at the gateway path (not the
/// raw Forgejo origin) and carries `Authorization: Bearer …`.
#[tokio::test]
async fn fjord_client_routes_through_gateway_with_bearer() {
    use crate::client::AuthScheme;
    let server = MockServer::start().await;
    // The gateway base for instance "inst_x" against this mock platform.
    let base = Url::parse(&format!(
        "{}/api/v1/forge-gateway/inst_x/api/v1/",
        server.uri()
    ))
    .expect("base url");

    // The mock only matches if the request hits the gateway path AND
    // carries the Fjord bearer in Authorization. A PAT-style request
    // wouldn't satisfy either matcher.
    Mock::given(method("GET"))
        .and(path("/api/v1/forge-gateway/inst_x/api/v1/user"))
        .and(header("authorization", "Bearer jwt-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7, "login": "alice"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c =
        Client::for_base_url_with_scheme(base, Some("jwt-token".into()), AuthScheme::FjordBearer)
            .expect("client");
    // Existing call sites use `/api/v1/<path>`; verify that survives the
    // gateway re-base. The Client's `url()` strips the `/api/v1/` prefix
    // before joining against the base, so the result is the gateway-routed
    // URL `…/forge-gateway/inst_x/api/v1/user`.
    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect("ok");
    assert_eq!(res["login"], "alice");
}

fn workflow_runs_payload(runs: serde_json::Value) -> serde_json::Value {
    json!({
        "total_count": runs.as_array().map_or(0, Vec::len),
        "workflow_runs": runs,
    })
}

#[tokio::test]
async fn run_list_tolerates_inconsistent_forgejo_rows() {
    // This Forgejo emits inconsistent run rows: workflow_id as a file
    // path or an int, and queued/odd rows missing name, html_url, and
    // timestamps. The whole list must still deserialize.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        // Forgejo only honors `limit` when `page` is also present, so the
        // wrapper must send both or `-L` is silently ignored.
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 3,
            "workflow_runs": [
                {
                    "id": 1, "name": "ci", "status": "failure",
                    "conclusion": null, "workflow_id": "cache-smoke.yml",
                    "html_url": "https://x/1",
                    "created_at": "2026-05-18T00:00:00Z",
                    "updated_at": "2026-05-18T00:01:00Z", "run_number": 788
                },
                {
                    "id": 2, "name": "ci", "status": "success",
                    "conclusion": "success", "workflow_id": 42,
                    "html_url": "https://x/2",
                    "created_at": "2026-05-18T00:00:00Z",
                    "updated_at": "2026-05-18T00:01:00Z", "run_number": 789
                },
                {
                    "id": 3, "status": "waiting", "run_number": 790
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs(&c, "o", "r", 50)
        .await
        .expect("inconsistent rows must not fail the whole list");
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].workflow_id, "cache-smoke.yml");
    assert_eq!(runs[1].workflow_id, "42");
    // Row 3 omits name/html_url/workflow_id/timestamps entirely.
    assert_eq!(runs[2].workflow_id, "");
    assert_eq!(runs[2].name, "");
    assert_eq!(runs[2].status, "waiting");
    assert!(runs[2].updated_at.is_none());
    assert_eq!(runs[2].run_number, 790);
}

#[tokio::test]
async fn run_list_status_filter_is_sent_and_applied_client_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .and(query_param("status", "failure"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
            { "id": 1, "status": "failure", "workflow_id": "ci.yml", "run_number": 1 },
            { "id": 2, "status": "completed", "conclusion": "failure", "workflow_id": "ci.yml", "run_number": 2 },
            { "id": 3, "status": "success", "workflow_id": "ci.yml", "run_number": 3 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        50,
        false,
        &crate::api::workflow::RunListFilters {
            status: Some("failure".into()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");

    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs.iter().map(|run| run.run_number).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn run_list_branch_filter_is_sent_and_applied_client_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("branch", "main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
            { "id": 1, "status": "success", "head_branch": "main", "workflow_id": "ci.yml", "run_number": 10 },
            { "id": 2, "status": "success", "head_branch": "feature/x", "workflow_id": "ci.yml", "run_number": 11 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        false,
        &crate::api::workflow::RunListFilters {
            branch: Some("main".into()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_number, 10);
}

#[tokio::test]
async fn run_list_event_filter_is_sent_and_applied_client_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("event", "pull_request"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
            { "id": 1, "status": "success", "event": "pull_request", "workflow_id": "ci.yml", "run_number": 20 },
            { "id": 2, "status": "success", "event": "push", "workflow_id": "ci.yml", "run_number": 21 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        false,
        &crate::api::workflow::RunListFilters {
            event: Some("pull_request".into()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_number, 20);
}

#[tokio::test]
async fn run_list_workflow_filter_is_sent_and_applied_client_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("workflow", "ci.yml"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
            { "id": 1, "status": "success", "workflow_id": ".forgejo/workflows/ci.yml", "run_number": 30 },
            { "id": 2, "status": "success", "workflow_id": "deploy.yml", "run_number": 31 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        false,
        &crate::api::workflow::RunListFilters {
            workflow: Some("ci.yml".into()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_number, 30);
}

#[tokio::test]
async fn run_list_combines_filters_with_and() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("status", "failure"))
        .and(query_param("branch", "main"))
        .and(query_param("event", "push"))
        .and(query_param("workflow", "ci.yml"))
        .respond_with(ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
            { "id": 1, "status": "failure", "head_branch": "main", "event": "push", "workflow_id": "ci.yml", "run_number": 40 },
            { "id": 2, "status": "success", "head_branch": "main", "event": "push", "workflow_id": "ci.yml", "run_number": 41 },
            { "id": 3, "status": "failure", "head_branch": "feature/x", "event": "push", "workflow_id": "ci.yml", "run_number": 42 },
            { "id": 4, "status": "failure", "head_branch": "main", "event": "pull_request", "workflow_id": "ci.yml", "run_number": 43 },
            { "id": 5, "status": "failure", "head_branch": "main", "event": "push", "workflow_id": "deploy.yml", "run_number": 44 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        false,
        &crate::api::workflow::RunListFilters {
            status: Some("failure".into()),
            branch: Some("main".into()),
            event: Some("push".into()),
            workflow: Some("ci.yml".into()),
        },
    )
    .await
    .expect("filtered list");

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_number, 40);
}

#[tokio::test]
async fn run_list_paginate_keeps_filters_across_pages() {
    let server = MockServer::start().await;
    let next_url = format!(
        "{}/api/v1/repos/o/r/actions/runs?page=2&limit=50&status=success",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .and(query_param("status", "success"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(workflow_runs_payload(json!([
                    { "id": 1, "status": "success", "workflow_id": "ci.yml", "run_number": 60 },
                    { "id": 2, "status": "failure", "workflow_id": "ci.yml", "run_number": 61 }
                ])))
                .insert_header("Link", format!(r#"<{next_url}>; rel="next""#).as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "2"))
        .and(query_param("limit", "50"))
        .and(query_param("status", "success"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
                { "id": 3, "status": "success", "workflow_id": "ci.yml", "run_number": 62 },
                { "id": 4, "status": "cancelled", "workflow_id": "ci.yml", "run_number": 63 }
            ]))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let runs = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        true,
        &crate::api::workflow::RunListFilters {
            status: Some("success".into()),
            ..Default::default()
        },
    )
    .await
    .expect("paginated filtered list");

    assert_eq!(
        runs.iter().map(|run| run.run_number).collect::<Vec<_>>(),
        vec![60, 62]
    );
}

#[tokio::test]
async fn run_list_unknown_filter_value_errors_after_unfiltered_check() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("status", "typo"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
                { "id": 1, "status": "success", "workflow_id": "ci.yml", "run_number": 50 }
            ]))),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param_is_missing("status"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(workflow_runs_payload(json!([
                { "id": 1, "status": "success", "workflow_id": "ci.yml", "run_number": 50 }
            ]))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let err = crate::api::workflow::list_runs_filtered(
        &c,
        "o",
        "r",
        20,
        false,
        &crate::api::workflow::RunListFilters {
            status: Some("typo".into()),
            ..Default::default()
        },
    )
    .await
    .expect_err("unknown filter should error");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown workflow run filter value"),
        "got: {msg}"
    );
    assert!(msg.contains("status `typo`"), "got: {msg}");
}

// ── Run logs (web routes, not /api/v1) ───────────────────────────────────────
// These exercise the deliberately-non-API log readers in api::workflow. The
// key things under test: the web URL is anchored at the host ORIGIN (no
// `/api/v1/` prefix), the `/attempt/{n}` segment is present, an empty cursor
// list yields step metadata, and a populated list yields lines.

#[tokio::test]
async fn run_logs_falls_back_to_loki_task_container_logs() {
    let server = MockServer::start().await;
    let loki = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 5600, "index_in_repo": 56, "status": "failure" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/5600/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 9001, "name": "build", "status": "failure" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .and(query_param("run", "56"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tasks": [
            {
                "id": 123,
                "name": "build",
                "status": "failure",
                "started_at": "2026-07-03T00:00:00Z",
                "stopped_at": "2026-07-03T00:05:00Z"
            }
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/9001/logs"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(query_param(
            "query",
            r#"{job="docker", container=~"/FORGEJO-ACTIONS-TASK-123-.*"}"#,
        ))
        .and(query_param("direction", "forward"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {
                "resultType": "streams",
                "result": [{
                    "stream": {
                        "repo": "o/r",
                        "run_id": "56",
                        "container": "/FORGEJO-ACTIONS-TASK-123-WORKFLOW-abcd",
                        "stream": "stdout"
                    },
                    "values": [
                        ["1793577000000000000", r#"{"log":"first line\n"}"#],
                        ["1793577001000000000", "second line\n"]
                    ]
                }]
            }
        })))
        .expect(1)
        .mount(&loki)
        .await;

    let c = client_for(&server).await;
    let logs = crate::api::workflow::fetch_run_logs_with_loki_base(
        &c,
        "o",
        "r",
        56,
        None,
        None,
        false,
        &loki.uri(),
    )
    .await
    .expect("logs");

    assert_eq!(logs.source, "loki");
    assert_eq!(logs.jobs.len(), 1);
    assert_eq!(logs.jobs[0].task_id, Some(123));
    assert_eq!(logs.jobs[0].job_id, Some(9001));
    assert_eq!(logs.jobs[0].name, "build");
    assert_eq!(logs.jobs[0].lines[0].message, "first line");
    assert_eq!(logs.jobs[0].lines[1].message, "second line");
}

#[tokio::test]
async fn run_logs_reads_forgejo_16_workflow_runs_task_envelope() {
    // Forgejo 16 answers `/actions/tasks` with a `workflow_runs` key even
    // though the entries are tasks. Measured on 16.0.3; before this shape was
    // accepted the Loki fallback aborted with "unexpected workflow task list
    // response shape".
    let server = MockServer::start().await;
    let loki = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 5800, "index_in_repo": 58, "status": "failure" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/5800/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 9201, "name": "build", "status": "failure" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": [
                {
                    "id": 321,
                    "name": "build",
                    "status": "failure",
                    "started": "2026-07-03T00:00:00Z",
                    "stopped": "2026-07-03T00:05:00Z"
                }
            ]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/9201/logs"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(query_param(
            "query",
            r#"{job="docker", container=~"/FORGEJO-ACTIONS-TASK-321-.*"}"#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": { "resultType": "streams", "result": [{
                "stream": {"stream": "stdout"},
                "values": [["1793577200000000000", "envelope line"]]
            }]}
        })))
        .expect(1)
        .mount(&loki)
        .await;

    let c = client_for(&server).await;
    let logs = crate::api::workflow::fetch_run_logs_with_loki_base(
        &c,
        "o",
        "r",
        58,
        None,
        None,
        false,
        &loki.uri(),
    )
    .await
    .expect("logs");

    assert_eq!(logs.source, "loki");
    assert_eq!(logs.jobs[0].task_id, Some(321));
    assert_eq!(logs.jobs[0].lines[0].message, "envelope line");
}

#[tokio::test]
async fn run_logs_uses_selected_task_id_for_loki_query() {
    let server = MockServer::start().await;
    let loki = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 5700, "index_in_repo": 57, "status": "failure" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/5700/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 9101, "name": "lint", "status": "success" },
            { "id": 9102, "name": "test", "status": "failure" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .and(query_param("run", "57"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tasks": [
            {
                "id": 111,
                "name": "lint",
                "status": "success",
                "started_at": "2026-07-03T00:00:00Z",
                "stopped_at": "2026-07-03T00:02:00Z"
            },
            {
                "id": 456,
                "name": "test",
                "status": "failure",
                "started_at": "2026-07-03T00:03:00Z",
                "stopped_at": "2026-07-03T00:06:00Z"
            }
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/9102/logs"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(query_param(
            "query",
            r#"{job="docker", container=~"/FORGEJO-ACTIONS-TASK-456-.*"}"#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": { "resultType": "streams", "result": [{
                "stream": {"stream": "stderr"},
                "values": [["1793577100000000000", "selected job"]]
            }]}
        })))
        .expect(1)
        .mount(&loki)
        .await;

    let c = client_for(&server).await;
    let logs = crate::api::workflow::fetch_run_logs_with_loki_base(
        &c,
        "o",
        "r",
        57,
        Some(1),
        None,
        false,
        &loki.uri(),
    )
    .await
    .expect("logs");

    assert_eq!(logs.jobs.len(), 1);
    assert_eq!(logs.jobs[0].job, 1);
    assert_eq!(logs.jobs[0].task_id, Some(456));
    assert_eq!(logs.jobs[0].job_id, Some(9102));
    assert_eq!(logs.jobs[0].lines[0].stream.as_deref(), Some("stderr"));
}

#[tokio::test]
async fn run_logs_reads_forgejo_api_job_logs_by_job_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 1200, "index_in_repo": 12, "status": "failure" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/1200/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 101, "name": "build", "status": "success", "attempt": 2 },
            { "id": 102, "name": "test", "status": "failure", "task_id": 999 }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    // The task list is unfiltered and multi-megabyte on Forgejo 16, so the
    // native log path must never reach for it.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/102/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("line one\nline two\n"))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let logs = crate::api::workflow::fetch_run_logs(&c, "o", "r", 12, Some(1), None, false)
        .await
        .expect("logs");
    assert_eq!(logs.source, "forgejo-api");
    assert_eq!(logs.jobs.len(), 1);
    assert_eq!(logs.jobs[0].job, 1);
    assert_eq!(logs.jobs[0].job_id, Some(102));
    assert_eq!(logs.jobs[0].task_id, Some(999));
    assert_eq!(logs.jobs[0].name, "test");
    assert_eq!(logs.jobs[0].status, "failure");
    assert_eq!(logs.jobs[0].lines[0].message, "line one");
}

#[tokio::test]
async fn run_logs_resolves_run_number_beyond_first_page() {
    let server = MockServer::start().await;
    let next_url = format!(
        "{}/api/v1/repos/o/r/actions/runs?page=2&limit=50",
        server.uri()
    );

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", format!(r#"<{next_url}>; rel="next""#).as_str())
                .set_body_json(json!({
                    "workflow_runs": [{ "id": 1000, "index_in_repo": 100, "status": "success" }]
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "2"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 5100, "index_in_repo": 51, "status": "success" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/5100/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 51001, "name": "verify", "status": "success" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/51001/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("late-page log\n"))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let logs = crate::api::workflow::fetch_run_logs(&c, "o", "r", 51, None, None, false)
        .await
        .expect("logs");
    assert_eq!(logs.jobs.len(), 1);
    assert_eq!(logs.jobs[0].job_id, Some(51001));
    assert_eq!(logs.jobs[0].lines[0].message, "late-page log");
}

#[tokio::test]
async fn run_logs_jobs_api_404_reports_the_limitation_with_the_run_url() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 1200, "index_in_repo": 12, "status": "failure" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/1200/jobs"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found."))
        .expect(1)
        .mount(&server)
        .await;

    // The server cannot be identified, so the gate does not withhold the
    // route; the 404 it answers is then the evidence.
    let c = client_for(&server).await;
    let err = crate::api::workflow::fetch_run_logs_without_loki(&c, "o", "r", 12)
        .await
        .expect_err("a server without the jobs route should get the limitation");
    assert!(crate::api::workflow::is_actions_unavailable(&err));
    let msg = err.to_string();
    assert!(msg.contains("run #12"), "got: {msg}");
    assert!(msg.contains("HTTP 404"), "got: {msg}");
    assert!(msg.contains("rasterstate/fj#103"), "got: {msg}");
    assert!(
        msg.contains(&format!("{}/o/r/actions/runs/12", server.uri())),
        "got: {msg}"
    );
}

#[tokio::test]
async fn run_logs_latest_attempt_follows_reruns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/7/jobs/0/attempt/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "logs": { "stepsLog": [] },
            "state": {
                "run": { "done": true, "status": "success", "jobs": [] },
                "currentJob": { "title": "t", "steps": [],
                    "allAttempts": [{"number":1,"status":"failure"},{"number":2,"status":"success"}] }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let n = crate::api::workflow::latest_attempt(&c, "o", "r", 7, 0)
        .await
        .expect("attempt");
    assert_eq!(n, 2);
}

#[tokio::test]
async fn run_logs_pruned_run_is_friendly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/99/jobs/0/attempt/1"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("task with job_id 4744 and attempt 0: resource does not exist"),
        )
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::workflow::fetch_run_view(&c, "o", "r", 99, 0, 1, &[])
        .await
        .expect_err("should error");
    assert!(
        err.to_string().contains("no longer available"),
        "got: {err}"
    );
}

#[tokio::test]
async fn run_logs_private_repo_404_reports_token_auth_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/395/jobs/0/attempt/1"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found."))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1,
            "workflow_runs": [{ "id": 44, "status": "failure", "run_number": 395 }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let err = crate::api::workflow::fetch_run_summary(&c, "o", "r", 395)
        .await
        .expect_err("private web-route 404 should be classified via run list");
    let msg = err.to_string();
    assert!(msg.contains("do not accept an API token"), "got: {msg}");
    assert!(msg.contains("cookie/CSRF"), "got: {msg}");
    assert!(msg.contains("rasterstate/fj#103"), "got: {msg}");
    assert!(
        !msg.contains("no logs for that run/job/attempt"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn run_logs_missing_run_404_keeps_missing_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/404/jobs/0/attempt/1"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found."))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 0,
            "workflow_runs": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let err = crate::api::workflow::fetch_run_summary(&c, "o", "r", 404)
        .await
        .expect_err("missing run should stay a missing-log error");
    let msg = err.to_string();
    assert!(
        msg.contains("no logs for that run/job/attempt"),
        "got: {msg}"
    );
    assert!(msg.contains("fj run list"), "got: {msg}");
    assert!(!msg.contains("do not accept an API token"), "got: {msg}");
}

#[tokio::test]
async fn run_summary_reads_run_level_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/56/jobs/0/attempt/1"))
        .and(body_json(json!({ "logCursors": [] })))
        .and(no_authorization)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "logs": { "stepsLog": [] },
            "state": {
                "run": {
                    "title": "Fix the thing",
                    "status": "success",
                    "done": true,
                    "link": "/o/r/actions/runs/56",
                    "commit": {
                        "shortSHA": "abc1234567",
                        "branch": { "name": "main" },
                        "pusher": { "displayName": "stephen" }
                    },
                    "jobs": [{"name":"check","status":"success","duration":"3m8s"}]
                },
                "currentJob": { "title": "check", "steps": [], "allAttempts": [] }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let view = crate::api::workflow::fetch_run_summary(&c, "o", "r", 56)
        .await
        .expect("summary");
    let run = &view.state.run;
    assert_eq!(run.title, "Fix the thing");
    assert_eq!(run.status, "success");
    assert_eq!(run.commit.short_sha, "abc1234567");
    assert_eq!(run.commit.branch.name, "main");
    assert_eq!(run.commit.pusher.display_name, "stephen");
    assert_eq!(run.jobs.len(), 1);
    assert_eq!(run.jobs[0].status, "success");
}

// ── Rerun and cancel (token API, gated per flavor and version) ───────────────
// Gitea 1.26 has rerun routes; Forgejo has none. Forgejo 16 has a cancel route;
// Gitea 1.27.3 has none. A withheld action makes no request at all.

async fn mount_run_900(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 900, "run_number": 56, "status": "failure" }]
        })))
        .mount(server)
        .await;
}

async fn refuse_everything(server: &MockServer) {
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

#[tokio::test]
async fn rerun_uses_the_gitea_run_route() {
    use crate::api::workflow::{rerun, RerunOutcome, RerunTarget};
    let server = MockServer::start().await;
    mount_run_900(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/rerun"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Gitea, "1.26.0").await;
    let out = rerun(&c, "o", "r", 56, RerunTarget::Run)
        .await
        .expect("rerun");
    assert_eq!(out, RerunOutcome::Started);
}

#[tokio::test]
async fn rerun_failed_uses_the_gitea_route_and_reads_no_failed_jobs() {
    use crate::api::workflow::{rerun, RerunOutcome, RerunTarget};
    let server = MockServer::start().await;
    mount_run_900(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/rerun-failed-jobs"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "message": "no failed jobs to re-run" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Gitea, "1.27.3").await;
    let out = rerun(&c, "o", "r", 56, RerunTarget::Failed)
        .await
        .expect("a run with nothing failed is not an error");
    assert_eq!(out, RerunOutcome::NothingFailed);
}

#[tokio::test]
async fn rerun_job_maps_the_index_to_the_gitea_job_id() {
    use crate::api::workflow::{rerun, RerunOutcome, RerunTarget};
    let server = MockServer::start().await;
    mount_run_900(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 2,
            "jobs": [
                { "id": 11, "name": "build", "status": "completed", "conclusion": "success" },
                { "id": 12, "name": "test", "status": "completed", "conclusion": "failure" }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/jobs/12/rerun"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Gitea, "1.26.2").await;
    let out = rerun(&c, "o", "r", 56, RerunTarget::Job(1))
        .await
        .expect("rerun job");
    assert_eq!(out, RerunOutcome::Started);

    let err = rerun(&c, "o", "r", 56, RerunTarget::Job(5))
        .await
        .expect_err("an index past the last job is rejected before any POST");
    assert!(err.to_string().contains("out of range"), "got: {err}");
}

#[tokio::test]
async fn rerun_is_withheld_where_the_route_does_not_exist() {
    use crate::api::workflow::{is_actions_unavailable, rerun, RerunTarget};
    let server = MockServer::start().await;
    refuse_everything(&server).await;
    // Forgejo has no rerun route at any version, and a Forgejo version number
    // is never read as a Gitea one: 16 is not "at least Gitea 1.26".
    for (flavor, version) in [(Flavor::Forgejo, "16.0.4"), (Flavor::Gitea, "1.25.4")] {
        let c = pinned_client(&server, flavor, version).await;
        let err = rerun(&c, "o", "r", 56, RerunTarget::Run)
            .await
            .expect_err("rerun should be withheld");
        assert!(is_actions_unavailable(&err), "{version}: {err}");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("{}/o/r/actions/runs/56", server.uri())),
            "got: {msg}"
        );
    }
}

#[tokio::test]
async fn cancel_uses_the_forgejo_16_route_and_names_a_permission_denial() {
    use crate::api::workflow::{cancel, is_actions_unavailable};
    let server = MockServer::start().await;
    mount_run_900(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/cancel"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "16.0.4").await;
    cancel(&c, "o", "r", 56).await.expect("cancel");

    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/cancel"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;
    let err = cancel(&c, "o", "r", 56).await.expect_err("403");
    assert!(!is_actions_unavailable(&err));
    assert!(err.to_string().contains("permission denied"), "got: {err}");
}

#[tokio::test]
async fn cancel_is_withheld_on_gitea_and_older_forgejo() {
    use crate::api::workflow::{cancel, is_actions_unavailable};
    let server = MockServer::start().await;
    refuse_everything(&server).await;
    for (flavor, version) in [(Flavor::Gitea, "1.27.3"), (Flavor::Forgejo, "15.0.2")] {
        let c = pinned_client(&server, flavor, version).await;
        let err = cancel(&c, "o", "r", 56).await.expect_err("withheld");
        assert!(is_actions_unavailable(&err), "{version}: {err}");
        assert!(err.to_string().contains("/o/r/actions/runs/56"), "{err}");
    }
}

// ── Parity batch: issue delete, pr update-branch, release delete-asset, workflow dispatch ──

#[tokio::test]
async fn issue_delete_hits_delete_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/7"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::issue::delete(&c, "o", "r", 7)
        .await
        .expect("ok");
}

#[tokio::test]
async fn pr_update_branch_posts_update_with_style() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/update"))
        .and(query_param("style", "rebase"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::update_branch(&c, "o", "r", 12, "rebase")
        .await
        .expect("ok");
}

#[tokio::test]
async fn issue_upload_asset_posts_multipart_to_issue_index() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        // PRs share the issue index, so PR #7 uploads to issues/7/assets.
        .and(path("/api/v1/repos/o/r/issues/7/assets"))
        .and(query_param("name", "diagram.png"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 3,
            "name": "diagram.png",
            "size": 11,
            "browser_download_url": "https://x/attachments/abc",
            "created_at": "2026-06-07T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let att =
        crate::api::issue::upload_asset(&c, "o", "r", 7, "diagram.png", b"hello world".to_vec())
            .await
            .expect("ok");
    assert_eq!(att.id, 3);
    assert_eq!(att.size, 11);
    assert_eq!(att.browser_download_url, "https://x/attachments/abc");
}

#[tokio::test]
async fn release_delete_asset_uses_release_scoped_path() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/releases/5/assets/9"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::release::delete_asset(&c, "o", "r", 5, 9)
        .await
        .expect("ok");
}

#[tokio::test]
async fn workflow_dispatch_sends_ref_and_inputs() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/repos/o/r/actions/workflows/ci.yml/dispatches",
        ))
        .and(body_json(
            json!({ "ref": "main", "inputs": { "env": "prod" } }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let mut inputs = std::collections::BTreeMap::new();
    inputs.insert("env".to_string(), "prod".to_string());
    crate::api::workflow::dispatch(&c, "o", "r", "ci.yml", "main", inputs)
        .await
        .expect("ok");
}

#[tokio::test]
async fn workflow_dispatch_omits_empty_inputs() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/repos/o/r/actions/workflows/ci.yml/dispatches",
        ))
        .and(body_json(json!({ "ref": "main" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::workflow::dispatch(
        &c,
        "o",
        "r",
        "ci.yml",
        "main",
        std::collections::BTreeMap::new(),
    )
    .await
    .expect("ok");
}

// ── Parity follow-ups: issue pin/unpin, repo deploy keys ──

#[tokio::test]
async fn issue_pin_posts_unpin_deletes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/pin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/pin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::issue::set_pinned(&c, "o", "r", 3, true)
        .await
        .expect("pin");
    crate::api::issue::set_pinned(&c, "o", "r", 3, false)
        .await
        .expect("unpin");
}

#[tokio::test]
async fn deploy_key_add_sends_read_only_flag() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/keys"))
        .and(body_json(json!({
            "title": "ci", "key": "ssh-ed25519 AAAA", "read_only": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 8, "key": "ssh-ed25519 AAAA", "title": "ci",
            "read_only": true, "created_at": "2026-05-30T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let k = crate::api::repo::add_key(&c, "o", "r", "ci", "ssh-ed25519 AAAA", true)
        .await
        .expect("add");
    assert_eq!(k.id, 8);
    assert!(k.read_only);
}

#[tokio::test]
async fn deploy_key_list_and_delete() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "key": "ssh-rsa B", "title": "deploy",
             "read_only": false, "created_at": "2026-05-30T00:00:00Z"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/keys/1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let keys = crate::api::repo::list_keys(&c, "o", "r")
        .await
        .expect("list");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].title, "deploy");
    crate::api::repo::delete_key(&c, "o", "r", 1)
        .await
        .expect("delete");
}

// ── Repo core + social ──────────────────────────────────────────────────────

#[tokio::test]
async fn repo_get_decodes_single_repo() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42,
            "name": "r",
            "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "https://x/o/r",
            "clone_url": "https://x/o/r.git",
            "ssh_url": "git@x:o/r.git",
            "default_branch": "main",
            "default_merge_style": "squash",
            "allow_fast_forward_only_merge": true,
            "stars_count": 7,
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repo = crate::api::repo::get(&c, "o", "r").await.expect("get");
    assert_eq!(repo.id, 42);
    assert_eq!(repo.full_name, "o/r");
    assert_eq!(repo.default_branch, "main");
    assert_eq!(repo.default_merge_style.as_deref(), Some("squash"));
    assert!(repo.allows_style("fast-forward-only"));
    assert!(repo.allowed_styles().contains(&"fast-forward-only"));
    assert_eq!(repo.stars_count, 7);
}

#[tokio::test]
async fn repo_list_branches_decodes_commit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "main", "protected": true,
              "commit": { "id": "abc123", "message": "init" } },
            { "name": "dev", "commit": { "id": "def456" } }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let branches = crate::api::repo::list_branches(&c, "o", "r")
        .await
        .expect("branches");
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0].name, "main");
    assert!(branches[0].protected);
    assert_eq!(branches[0].commit.id, "abc123");
    // `message` defaults when omitted.
    assert_eq!(branches[1].commit.message, "");
}

#[tokio::test]
async fn repo_list_topics_unwraps_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/topics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "topics": ["rust", "cli", "forgejo"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let topics = crate::api::repo::list_topics(&c, "o", "r")
        .await
        .expect("topics");
    assert_eq!(topics, vec!["rust", "cli", "forgejo"]);
}

#[tokio::test]
async fn repo_set_topics_puts_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/topics"))
        .and(body_json(json!({ "topics": ["rust", "cli"] })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::set_topics(&c, "o", "r", &["rust".into(), "cli".into()])
        .await
        .expect("set topics");
}

#[tokio::test]
async fn repo_collaborator_list_decodes_permissions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1,
                "login": "alice",
                "full_name": "Alice Example",
                "permissions": { "admin": false, "push": true, "pull": true }
            },
            {
                "id": 2,
                "login": "bob",
                "permissions": { "admin": true, "push": true, "pull": true }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let collaborators = crate::api::repo::list_collaborators_page(&c, "o", "r", None)
        .await
        .expect("collaborators")
        .items;

    assert_eq!(collaborators.len(), 2);
    assert_eq!(collaborators[0].user.login, "alice");
    assert_eq!(collaborators[0].permission_level(), "write");
    assert_eq!(collaborators[1].permission_level(), "admin");
}

#[tokio::test]
async fn repo_collaborator_add_puts_permission_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/collaborators/alice"))
        .and(body_json(json!({ "permission": "admin" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    crate::api::repo::add_collaborator(
        &c,
        "o",
        "r",
        "alice",
        crate::api::repo::CollaboratorPermission::Admin,
    )
    .await
    .expect("add collaborator");
}

#[tokio::test]
async fn repo_collaborator_remove_deletes_encoded_user() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/collaborators/alice%2Fbot"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    crate::api::repo::remove_collaborator(&c, "o", "r", "alice/bot")
        .await
        .expect("remove collaborator");
}

#[tokio::test]
async fn repo_collaborator_remove_not_found_is_clear() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/collaborators/alice"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "not found"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let err = crate::api::repo::remove_collaborator(&c, "o", "r", "alice")
        .await
        .expect_err("remove should fail clearly");

    assert!(
        err.to_string()
            .contains("'alice' is not a collaborator on o/r"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn repo_fork_posts_org_and_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/forks"))
        .and(body_json(
            json!({ "organization": "myorg", "name": "myfork" }),
        ))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "id": 99,
            "name": "myfork",
            "full_name": "myorg/myfork",
            "owner": { "id": 2, "login": "myorg" },
            "html_url": "https://x/myorg/myfork",
            "clone_url": "https://x/myorg/myfork.git",
            "ssh_url": "git@x:myorg/myfork.git",
            "fork": true,
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let fork = crate::api::repo::fork(&c, "o", "r", Some("myorg"), Some("myfork"))
        .await
        .expect("fork");
    assert_eq!(fork.full_name, "myorg/myfork");
    assert!(fork.fork);
}

#[tokio::test]
async fn repo_star_puts_to_starred_path() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/user/starred/o/r"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::star(&c, "o", "r").await.expect("star");
}

#[tokio::test]
async fn repo_watch_puts_subscription_and_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/subscription"))
        .and(body_json(json!({ "subscribed": true, "ignored": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "subscribed": true,
            "ignored": false,
            "reason": null,
            "url": "https://x/repos/o/r/subscription"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let sub = crate::api::repo::watch(&c, "o", "r").await.expect("watch");
    assert!(sub.subscribed);
    assert!(!sub.ignored);
}

#[tokio::test]
async fn repo_sync_with_upstream_posts_branch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/merge-upstream"))
        .and(body_json(json!({ "branch": "main" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "merge_style": "merge" })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::sync_with_upstream(&c, "o", "r", "main")
        .await
        .expect("sync");
}

#[tokio::test]
async fn repo_sync_missing_endpoint_is_friendly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/merge-upstream"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::repo::sync_with_upstream(&c, "o", "r", "main")
        .await
        .expect_err("404 should map to friendly error");
    assert!(err.to_string().contains("merge-upstream"), "got: {err}");
}

// ── Artifacts (web route reads) + workflow listing (contents API) ──

#[tokio::test]
async fn list_artifacts_parses_web_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/o/r/actions/runs/5/artifacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artifacts": [
                {"name": "binary", "size": 1024, "status": "completed"},
                {"name": "old", "size": 10, "status": "expired"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let arts = crate::api::workflow::list_artifacts(&c, "o", "r", 5)
        .await
        .expect("list");
    assert_eq!(arts.len(), 2);
    assert_eq!(arts[0].name, "binary");
    assert_eq!(arts[0].size, 1024);
    assert_eq!(arts[1].status, "expired");
}

#[tokio::test]
async fn download_artifact_returns_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/o/r/actions/runs/5/artifacts/binary"))
        .and(no_authorization)
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04zip".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    // An entry listed from the web route has no API id.
    let artifact: crate::api::workflow::Artifact =
        serde_json::from_value(json!({ "name": "binary" })).unwrap();
    let bytes = crate::api::workflow::download_artifact(&c, "o", "r", 5, &artifact)
        .await
        .expect("download");
    assert_eq!(&bytes[..2], b"PK");
}

#[tokio::test]
async fn list_workflows_reads_both_dirs_and_filters_yaml() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/.forgejo/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"name": "ci.yml", "path": ".forgejo/workflows/ci.yml", "type": "file"},
            {"name": "README.md", "path": ".forgejo/workflows/README.md", "type": "file"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/.github/workflows"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let files = crate::api::workflow::list_workflows(&c, "o", "r")
        .await
        .expect("list");
    assert_eq!(files.len(), 1, "only the .yml file counts");
    assert_eq!(files[0].name, "ci.yml");
}

#[tokio::test]
async fn list_secrets_parses_live_bare_array() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/secrets"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "DEPLOY_KEY", "created_at": "2024-01-02T03:04:05Z" },
            { "name": "TOKEN", "created_at": "2024-02-03T04:05:06Z" }
        ])))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let secrets = crate::api::workflow::list_secrets(&client, owner, name)
        .await
        .unwrap();
    assert_eq!(secrets.len(), 2);
    assert_eq!(secrets[0].name, "DEPLOY_KEY");
    assert_eq!(secrets[1].name, "TOKEN");
}

#[tokio::test]
async fn set_secret_puts_data_body() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/secrets/DEPLOY_KEY"
        )))
        .and(body_json(json!({ "data": "s3cr3t" })))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    crate::api::workflow::set_secret(&client, owner, name, "DEPLOY_KEY", "s3cr3t")
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_secret_issues_delete() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/secrets/DEPLOY_KEY"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    crate::api::workflow::delete_secret(&client, owner, name, "DEPLOY_KEY")
        .await
        .unwrap();
}

#[tokio::test]
async fn list_variables_parses_live_bare_array() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/variables"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "REGION", "data": "us-west-2", "owner_id": 3, "repo_id": 9 },
            { "name": "STAGE", "data": "prod", "owner_id": 3, "repo_id": 9 }
        ])))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let vars = crate::api::workflow::list_variables(&client, owner, name)
        .await
        .unwrap();
    assert_eq!(vars.len(), 2);
    assert_eq!(vars[0].name, "REGION");
    assert_eq!(vars[0].data, "us-west-2");
    assert_eq!(vars[1].name, "STAGE");
}

#[tokio::test]
async fn set_variable_updates_an_existing_variable_with_put() {
    let server = MockServer::start().await;
    let item = "/api/v1/repos/o/r/actions/variables/REGION";
    Mock::given(method("GET"))
        .and(path(item))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(
            { "name": "REGION", "data": "us-west-2", "owner_id": 3, "repo_id": 9 }
        )))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(item))
        .and(body_json(json!({ "value": "us-west-2" })))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(item))
        .respond_with(ResponseTemplate::new(409))
        .expect(0)
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let outcome = crate::api::workflow::set_variable(&client, "o", "r", "REGION", "us-west-2")
        .await
        .unwrap();
    assert_eq!(outcome.write, crate::api::workflow::VariableWrite::Updated);
}

#[tokio::test]
async fn set_variable_creates_a_missing_variable_with_post_and_confirms_it() {
    let server = MockServer::start().await;
    // The user typed a lower-case name; the server stores it upper-cased.
    let item = "/api/v1/repos/o/r/actions/variables/region";
    Mock::given(method("GET"))
        .and(path(item))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(item))
        .and(body_json(json!({ "value": "us-west-2" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(item))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(item))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(
            { "name": "REGION", "data": "us-west-2", "owner_id": 3, "repo_id": 9 }
        )))
        .expect(1)
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let outcome = crate::api::workflow::set_variable(&client, "o", "r", "region", "us-west-2")
        .await
        .unwrap();
    assert_eq!(outcome.write, crate::api::workflow::VariableWrite::Created);
    // Name and scope are the server's, not the request's.
    assert_eq!(outcome.variable.name, "REGION");
    assert_eq!(
        outcome.variable.scope(),
        crate::api::workflow::VariableScope::Repository
    );
}

#[tokio::test]
async fn variable_errors_tell_a_denied_token_from_disabled_actions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/off/actions/variables"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/denied/actions/variables"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/actions/variables/GONE"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let client = client_for(&server).await;

    let off = crate::api::workflow::list_variables(&client, "o", "off")
        .await
        .unwrap_err();
    assert!(
        format!("{off:#}").contains("Actions variables are unavailable for o/off"),
        "{off:#}"
    );

    let denied = crate::api::workflow::list_variables(&client, "o", "denied")
        .await
        .unwrap_err();
    let denied = format!("{denied:#}");
    assert!(denied.contains("forbidden"), "{denied}");
    assert!(!denied.contains("unavailable"), "{denied}");

    let gone = crate::api::workflow::delete_variable(&client, "o", "r", "GONE")
        .await
        .unwrap_err();
    assert!(
        format!("{gone:#}").contains("variable 'GONE' not found"),
        "{gone:#}"
    );
}

#[tokio::test]
async fn delete_variable_issues_delete() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/variables/REGION"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    crate::api::workflow::delete_variable(&client, owner, name, "REGION")
        .await
        .unwrap();
}

#[tokio::test]
async fn list_artifacts_parses_envelope() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("GET"))
        .and(path(format!("/{owner}/{name}/actions/runs/56/artifacts")))
        .and(no_authorization)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artifacts": [
                { "name": "build", "size": 1024, "status": "completed" },
                { "name": "coverage", "size": 2048, "status": "expired" }
            ]
        })))
        .mount(&server)
        .await;
    let client = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let artifacts = crate::api::workflow::list_artifacts(&client, owner, name, 56)
        .await
        .unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].name, "build");
    assert_eq!(artifacts[0].size, 1024);
    assert_eq!(artifacts[1].status, "expired");
}

// ── Pull core (list/get/create/merge/files/commits) + reviews + status ──

#[tokio::test]
async fn pull_list_decodes_and_sends_state_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("limit", "30"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {
                        "id": 1, "number": 12, "title": "Add feature",
                        "state": "open", "user": { "id": 1, "login": "alice" },
                        "html_url": "https://x/o/r/pulls/12",
                        "head": { "ref": "feature", "sha": "aaa" },
                        "base": { "ref": "main", "sha": "bbb" },
                        "created_at": "2026-05-30T00:00:00Z",
                        "updated_at": "2026-05-30T00:01:00Z"
                    }
                ]))
                .insert_header("X-Total-Count", "104"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(&c, "o", "r", crate::api::pull::ListOptions::default())
        .await
        .expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 12);
    assert_eq!(page.items[0].head.ref_, "feature");
    assert_eq!(page.items[0].base.ref_, "main");
    assert_eq!(page.total, Some(104));
}

#[tokio::test]
async fn pull_list_decodes_negative_id_requested_reviewer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("limit", "30"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {
                        "id": 245, "number": 245,
                        "title": "Handle null PR check status lists",
                        "state": "open",
                        "user": { "id": 3, "login": "stephen" },
                        "requested_reviewers": [
                            {
                                "id": -2,
                                "login": "forgejo-actions",
                                "email": "noreply@forgejo.org",
                                "created": "1970-01-01T00:00:00Z"
                            }
                        ],
                        "html_url": "https://x/o/r/pulls/245",
                        "head": { "ref": "fix/241-null-pr-check-statuses", "sha": "aaa" },
                        "base": { "ref": "main", "sha": "bbb" },
                        "created_at": "2026-08-09T00:00:00Z",
                        "updated_at": "2026-08-09T00:01:00Z"
                    }
                ]))
                .insert_header("X-Total-Count", "1"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(&c, "o", "r", crate::api::pull::ListOptions::default())
        .await
        .expect("list decodes Forgejo system reviewer ids");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 245);
    assert_eq!(page.items[0].requested_reviewers[0].id, -2);
    assert_eq!(
        page.items[0].requested_reviewers[0].login,
        "forgejo-actions"
    );
}

#[tokio::test]
async fn pull_list_decode_error_names_endpoint_and_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("limit", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": -245, "number": 245, "title": "Bad id",
                "state": "open", "user": { "id": 3, "login": "stephen" },
                "html_url": "https://x/o/r/pulls/245",
                "head": { "ref": "fix/negative", "sha": "aaa" },
                "base": { "ref": "main", "sha": "bbb" },
                "created_at": "2026-08-09T00:00:00Z",
                "updated_at": "2026-08-09T00:01:00Z"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = match crate::api::pull::list(&c, "o", "r", crate::api::pull::ListOptions::default())
        .await
    {
        Ok(_) => panic!("list should reject invalid pull id"),
        Err(err) => err,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("GET /api/v1/repos/o/r/pulls?state=open&limit=30"),
        "{msg}"
    );
    assert!(msg.contains("field [0].id"), "{msg}");
}

#[tokio::test]
async fn pull_list_sends_label_ids_and_poster() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("labels", "920,921"))
        .and(query_param("poster", "alice"))
        .and(query_param("limit", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pull_json(12, "open")])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(
        &c,
        "o",
        "r",
        crate::api::pull::ListOptions {
            labels: Some(vec![920, 921]),
            author: Some("alice".into()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 12);
}

#[tokio::test]
async fn pull_list_filters_base_head_and_draft_locally() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("limit", "50"))
        .and(query_param_is_missing("base"))
        .and(query_param_is_missing("head"))
        .and(query_param_is_missing("draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1, "number": 12, "title": "Add feature",
                "state": "open", "user": { "id": 1, "login": "alice" },
                "html_url": "https://x/o/r/pulls/12",
                "head": { "label": "feature", "ref": "feature", "sha": "aaa" },
                "base": { "label": "main", "ref": "main", "sha": "bbb" },
                "draft": true,
                "created_at": "2026-05-30T00:00:00Z",
                "updated_at": "2026-05-30T00:01:00Z"
            },
            {
                "id": 2, "number": 13, "title": "Other feature",
                "state": "open", "user": { "id": 1, "login": "alice" },
                "html_url": "https://x/o/r/pulls/13",
                "head": { "label": "other", "ref": "other", "sha": "ccc" },
                "base": { "label": "develop", "ref": "develop", "sha": "ddd" },
                "draft": false,
                "created_at": "2026-05-30T00:00:00Z",
                "updated_at": "2026-05-30T00:01:00Z"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(
        &c,
        "o",
        "r",
        crate::api::pull::ListOptions {
            base: Some("main".into()),
            head: Some("feature".into()),
            draft: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 12);
}

#[tokio::test]
async fn pull_list_omits_filter_params_when_unset() {
    let server = MockServer::start().await;
    // Empty label set must not emit a `labels` param; the mock only matches
    // when state/limit are present and nothing else is required.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("limit", "30"))
        .and(query_param_is_missing("labels"))
        .and(query_param_is_missing("poster"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pull_json(12, "open")])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(
        &c,
        "o",
        "r",
        crate::api::pull::ListOptions {
            labels: Some(vec![]),
            author: None,
            ..Default::default()
        },
    )
    .await
    .expect("unfiltered list");
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn pull_list_paginate_follows_next_and_keeps_total() {
    let server = MockServer::start().await;
    let page1 = json!([pull_json(1, "open")]);
    let page2 = json!([pull_json(2, "open")]);
    let next_url = format!("{}/api/v1/repos/o/r/pulls?page=2&limit=50", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("page", "2"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page1)
                .insert_header("X-Total-Count", "2")
                .insert_header("Link", format!(r#"<{next_url}>; rel="next""#).as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::pull::list(
        &c,
        "o",
        "r",
        crate::api::pull::ListOptions {
            paginate: true,
            ..Default::default()
        },
    )
    .await
    .expect("paginated list");
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total, Some(2));
}

#[tokio::test]
async fn pull_get_decodes_single_pull() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "body": "the body", "state": "open",
            "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "draft": true, "merged": false,
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let pr = crate::api::pull::get(&c, "o", "r", 12).await.expect("get");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.title, "Add feature");
    assert!(pr.draft);
}

#[tokio::test]
async fn pull_get_decodes_null_body_as_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "body": null, "state": "open",
            "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let pr = crate::api::pull::get(&c, "o", "r", 12).await.expect("get");
    assert_eq!(pr.body, "");
}

#[tokio::test]
async fn pull_create_posts_title_head_base() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(body_json(json!({
            "title": "Add feature", "head": "feature", "base": "main",
            "body": "details"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "state": "open", "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let pr = crate::api::pull::create(
        &c,
        "o",
        "r",
        &crate::api::pull::CreatePull {
            title: "Add feature",
            head: "feature",
            base: "main",
            body: Some("details"),
            draft: false,
        },
    )
    .await
    .expect("create");
    assert_eq!(pr.number, 12);
}

#[tokio::test]
async fn pull_create_can_be_followed_by_label_and_assignee_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(body_json(json!({
            "title": "Add feature", "head": "feature", "base": "main",
            "body": "details"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "state": "open", "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "area/api", "color": "ee0000" },
            { "id": 11, "name": "needs-review", "color": "00ee00" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/12/labels"))
        .and(body_json(json!({ "labels": [10, 11] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "area/api", "color": "ee0000" },
            { "id": 11, "name": "needs-review", "color": "00ee00" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(
            12,
            "Add feature",
            "open",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .and(body_json(json!({ "assignees": ["alice", "bob"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(
            12,
            "Add feature",
            "open",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let pr = crate::api::pull::create(
        &c,
        "o",
        "r",
        &crate::api::pull::CreatePull {
            title: "Add feature",
            head: "feature",
            base: "main",
            body: Some("details"),
            draft: false,
        },
    )
    .await
    .expect("create");
    let names = vec!["area/api".to_string(), "needs-review".to_string()];
    let label_ids = crate::api::label::resolve_names_to_ids(&c, "o", "r", &names)
        .await
        .expect("resolve labels");
    crate::api::label::add_to_issue(&c, "o", "r", pr.number, label_ids)
        .await
        .expect("add labels to pr");
    crate::api::issue::add_assignees(
        &c,
        "o",
        "r",
        pr.number,
        vec!["alice".to_string(), "bob".to_string()],
    )
    .await
    .expect("add assignees to pr");
}

#[tokio::test]
async fn pull_merge_posts_do_and_squash_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "squash",
            "MergeTitleField": "Squashed",
            "MergeMessageField": "all the things"
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions {
            title: Some("Squashed"),
            message: Some("all the things"),
            delete_branch: false,
            auto: false,
        },
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_defaults_message_to_pull_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "body": "full PR description\n\nwith details", "state": "open",
            "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "squash",
            "MergeMessageField": "full PR description\n\nwith details"
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions::default(),
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_explicit_empty_message_overrides_pull_body_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "squash",
            "MergeMessageField": ""
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions {
            message: Some(""),
            ..Default::default()
        },
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_defaults_null_pull_body_to_empty_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "body": null, "state": "open",
            "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "squash",
            "MergeMessageField": ""
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions::default(),
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_sets_delete_branch_after_merge_when_requested() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/7/merge"))
        .and(body_json(json!({
            "Do": "merge",
            "MergeMessageField": "",
            "delete_branch_after_merge": true
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        7,
        crate::api::pull::MergeStyle::Merge,
        &crate::api::pull::MergeOptions {
            message: Some(""),
            delete_branch: true,
            ..Default::default()
        },
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_fast_forward_only_posts_style_without_commit_message_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "fast-forward-only"
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::FastForwardOnly,
        &crate::api::pull::MergeOptions::default(),
    )
    .await
    .expect("merge");
}

#[tokio::test]
async fn pull_merge_fast_forward_only_rejects_commit_message_fields_locally() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::FastForwardOnly,
        &crate::api::pull::MergeOptions {
            message: Some("unused"),
            ..Default::default()
        },
    )
    .await
    .expect_err("fast-forward-only should reject message fields");
    assert!(
        err.to_string()
            .contains("fast-forward-only merge does not create a merge commit"),
        "got: {err}"
    );
}

#[tokio::test]
async fn pull_merge_surfaces_forgejo_message_on_405() {
    // Regression for fj#159/#160: a non-mergeable merge must not collapse to a
    // bare "405 Method Not Allowed"; the Forgejo `message` has to survive.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/9/merge"))
        .respond_with(ResponseTemplate::new(405).set_body_json(json!({
            "message": "The pull request can not be merged"
        })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::merge(
        &c,
        "o",
        "r",
        9,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions {
            message: Some(""),
            ..Default::default()
        },
    )
    .await
    .expect_err("merge should fail");
    assert!(err.to_string().contains("can not be merged"), "got: {err}");
}

#[tokio::test]
async fn pull_merge_auto_posts_merge_when_checks_succeed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "number": 12, "title": "Add feature",
            "body": "auto merge body", "state": "open",
            "user": { "id": 1, "login": "alice" },
            "html_url": "https://x/o/r/pulls/12",
            "head": { "ref": "feature", "sha": "aaa" },
            "base": { "ref": "main", "sha": "bbb" },
            "created_at": "2026-05-30T00:00:00Z",
            "updated_at": "2026-05-30T00:01:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/merge"))
        .and(body_json(json!({
            "Do": "squash",
            "MergeMessageField": "auto merge body",
            "merge_when_checks_succeed": true
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::merge(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::MergeStyle::Squash,
        &crate::api::pull::MergeOptions {
            auto: true,
            ..Default::default()
        },
    )
    .await
    .expect("schedule auto-merge");
}

#[tokio::test]
async fn pull_cancel_auto_merge_deletes_merge_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/pulls/7/merge"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::cancel_auto_merge(&c, "o", "r", 7)
        .await
        .expect("cancel");
}

#[tokio::test]
async fn pull_cancel_auto_merge_404_is_surfaced() {
    // No schedule exists: the endpoint 404s. The error must carry the status so
    // the CLI can turn it into a friendly "nothing to cancel" message.
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/pulls/7/merge"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "auto merge not scheduled"
        })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::cancel_auto_merge(&c, "o", "r", 7)
        .await
        .expect_err("should 404");
    let api_err = err
        .downcast_ref::<crate::client::ApiError>()
        .expect("ApiError in chain");
    assert_eq!(api_err.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn auto_merge_pending_reads_single_timeline_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/4/timeline"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "type": "pull_push" },
            { "type": "pull_scheduled_merge" }
        ])))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    assert!(crate::api::pull::auto_merge_pending(&c, "o", "r", 4)
        .await
        .expect("timeline"));
}

#[tokio::test]
async fn auto_merge_pending_false_after_cancel() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/5/timeline"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "type": "pull_scheduled_merge" },
            { "type": "pull_cancel_scheduled_merge" }
        ])))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    assert!(!crate::api::pull::auto_merge_pending(&c, "o", "r", 5)
        .await
        .expect("timeline"));
}

// ── pr merge failure diagnosis (cli::pr_merge_check) ──

/// Build a pull JSON with the mergeability knobs the diagnosis reads.
fn merge_diag_pull_json(
    number: u64,
    mergeable: bool,
    draft: bool,
    state: &str,
) -> serde_json::Value {
    json!({
        "id": 1, "number": number, "title": "t", "state": state,
        "user": { "id": 1, "login": "alice" },
        "html_url": "https://x/o/r/pulls/1",
        "head": { "ref": "feature", "sha": "abc1234" },
        "base": { "ref": "main", "sha": "bbb" },
        "draft": draft, "mergeable": mergeable, "merged": false,
        "created_at": "2026-05-30T00:00:00Z",
        "updated_at": "2026-05-30T00:01:00Z"
    })
}

async fn run_merge_diagnose(server: &MockServer, number: u64, style: &str) -> Option<String> {
    let c = client_for(server).await;
    let pr = crate::api::pull::get(&c, "o", "r", number)
        .await
        .expect("get pull");
    crate::cli::pr_merge_check::diagnose_merge_failure(&c, "o", "r", &pr, style, None, None).await
}

#[tokio::test]
async fn diagnose_reports_disabled_merge_style() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/5"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(5, true, false, "open")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "h", "clone_url": "c", "ssh_url": "s",
            "default_branch": "main",
            "allow_merge_commits": false, "allow_rebase": false,
            "allow_rebase_explicit": false, "allow_squash_merge": true,
            "allow_fast_forward_only_merge": false,
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .mount(&server)
        .await;
    // No branch protection on main.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let msg = run_merge_diagnose(&server, 5, "merge")
        .await
        .expect("a reason");
    assert!(
        msg.contains("merge style \"merge\" is not enabled"),
        "got: {msg}"
    );
    assert!(msg.contains("allowed: squash"), "got: {msg}");
}

#[tokio::test]
async fn diagnose_reports_merge_conflicts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/6"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(6, false, false, "open")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "h", "clone_url": "c", "ssh_url": "s",
            "default_branch": "main",
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let msg = run_merge_diagnose(&server, 6, "squash")
        .await
        .expect("a reason");
    assert!(msg.contains("merge conflicts with main"), "got: {msg}");
}

#[tokio::test]
async fn diagnose_reports_failing_required_check() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(7, true, false, "open")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "h", "clone_url": "c", "ssh_url": "s",
            "default_branch": "main",
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "branch_name": "main",
            "enable_status_check": true,
            "status_check_contexts": ["ci/test"],
            "required_approvals": 0
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "pending", "sha": "abc1234", "total_count": 1,
            "statuses": [ { "status": "pending", "context": "ci/test" } ]
        })))
        .mount(&server)
        .await;

    let msg = run_merge_diagnose(&server, 7, "squash")
        .await
        .expect("a reason");
    assert!(msg.contains("required check"), "got: {msg}");
    assert!(msg.contains("ci/test"), "got: {msg}");
    assert!(msg.contains("pending"), "got: {msg}");
}

/// Mount a PR whose base requires `status_check_contexts: ["*"]`, with the
/// given statuses reported on its head commit.
async fn mount_wildcard_protection(server: &MockServer, number: u64, statuses: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/repos/o/r/pulls/{number}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(merge_diag_pull_json(number, true, false, "open")),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "h", "clone_url": "c", "ssh_url": "s",
            "default_branch": "main",
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "branch_name": "main",
            "enable_status_check": true,
            "status_check_contexts": ["*"],
            "required_approvals": 0
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "success", "sha": "abc1234", "statuses": statuses
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn diagnose_wildcard_required_context_all_green_is_not_a_check_block() {
    // fj#238: `*` is a glob over every reported context, not a context named
    // "*". A fully green head must not be reported as a missing check.
    let server = MockServer::start().await;
    mount_wildcard_protection(
        &server,
        20,
        json!([
            { "status": "success", "context": "CI / build (pull_request)" },
            { "status": "success", "context": "CI / test (pull_request)" },
            { "status": "skipped", "context": "lint / develop-superset-of-main (pull_request)" }
        ]),
    )
    .await;
    let msg = run_merge_diagnose(&server, 20, "squash").await;
    assert!(
        msg.as_deref().is_none_or(|m| !m.contains("required check")),
        "got: {msg:?}"
    );
}

#[tokio::test]
async fn diagnose_wildcard_required_context_names_the_failing_check() {
    let server = MockServer::start().await;
    mount_wildcard_protection(
        &server,
        21,
        json!([
            { "status": "success", "context": "CI / build (pull_request)" },
            { "status": "failure", "context": "CI / test (pull_request)" }
        ]),
    )
    .await;
    let msg = run_merge_diagnose(&server, 21, "squash")
        .await
        .expect("a reason");
    assert!(
        msg.contains("\"CI / test (pull_request)\" is not green (state: failure)"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn diagnose_reports_missing_approvals() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/8"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(8, true, false, "open")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "full_name": "o/r",
            "owner": { "id": 1, "login": "o" },
            "html_url": "h", "clone_url": "c", "ssh_url": "s",
            "default_branch": "main",
            "updated_at": "2026-05-30T00:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "branch_name": "main",
            "required_approvals": 2,
            "enable_status_check": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/8/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "state": "APPROVED", "user": { "id": 2, "login": "bob" },
              "submitted_at": "2026-05-30T00:00:00Z", "html_url": "h" }
        ])))
        .mount(&server)
        .await;

    let msg = run_merge_diagnose(&server, 8, "squash")
        .await
        .expect("a reason");
    assert!(
        msg.contains("needs 2 approving review(s) but has 1"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn pull_files_decodes_changes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "filename": "src/main.rs", "status": "modified",
              "additions": 10, "deletions": 2, "changes": 12 },
            { "filename": "README.md", "status": "added" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let files = crate::api::pull::files(&c, "o", "r", 12)
        .await
        .expect("files");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].filename, "src/main.rs");
    assert_eq!(files[0].additions, 10);
    // omitted numeric fields default to 0.
    assert_eq!(files[1].changes, 0);
}

#[tokio::test]
async fn pull_commits_decodes_meta() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "sha": "aaa",
              "commit": { "message": "first", "author": { "name": "alice", "email": "a@x" } },
              "html_url": "https://x/o/r/commit/aaa" },
            { "sha": "bbb" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let commits = crate::api::pull::commits(&c, "o", "r", 12)
        .await
        .expect("commits");
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, "aaa");
    assert_eq!(commits[0].commit.message, "first");
    assert_eq!(commits[0].commit.author.as_ref().unwrap().name, "alice");
    // omitted `commit` defaults.
    assert_eq!(commits[1].commit.message, "");
}

#[tokio::test]
async fn pull_submit_review_maps_event_to_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(body_json(json!({ "event": "APPROVED", "body": "LGTM" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 5, "state": "APPROVED", "body": "LGTM",
            "user": { "id": 1, "login": "alice" },
            "submitted_at": "2026-05-30T00:00:00Z",
            "html_url": "https://x/o/r/pulls/12#review-5"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let review = crate::api::pull::submit_review(
        &c,
        "o",
        "r",
        12,
        crate::api::pull::ReviewEvent::Approve,
        Some("LGTM"),
    )
    .await
    .expect("review");
    assert_eq!(review.id, 5);
    assert_eq!(review.state, "APPROVED");
}

#[tokio::test]
async fn pull_request_reviewers_posts_reviewer_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/requested_reviewers"))
        .and(body_json(json!({ "reviewers": ["bob", "carol"] })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::request_reviewers(&c, "o", "r", 12, vec!["bob".into(), "carol".into()])
        .await
        .expect("request reviewers");
}

#[tokio::test]
async fn pull_list_reviews_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 5, "state": "APPROVED", "body": "ok",
              "user": { "id": 1, "login": "alice" },
              "submitted_at": "2026-05-30T00:00:00Z",
              "html_url": "https://x/r1" },
            { "id": 6, "state": "COMMENT",
              "user": { "id": 2, "login": "bob" },
              "submitted_at": null,
              "html_url": "https://x/r2" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let reviews = crate::api::pull::list_reviews(&c, "o", "r", 12)
        .await
        .expect("reviews");
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].state, "APPROVED");
    // omitted body defaults; null submitted_at tolerated.
    assert_eq!(reviews[1].body, "");
    assert!(reviews[1].submitted_at.is_none());
}

#[tokio::test]
async fn pull_combined_status_decodes_state_and_statuses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/aaa/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "success",
            "sha": "aaa",
            "total_count": 2,
            "statuses": [
                { "status": "success", "context": "ci/build",
                  "description": "passed", "target_url": "https://x/1" },
                { "status": "pending", "context": "ci/lint" }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let cs = crate::api::pull::combined_status(&c, "o", "r", "aaa")
        .await
        .expect("status");
    assert_eq!(cs.state, "success");
    assert_eq!(cs.sha, "aaa");
    assert_eq!(cs.statuses.len(), 2);
    assert_eq!(cs.statuses[0].context, "ci/build");
    // omitted fields default.
    assert_eq!(cs.statuses[1].description, "");
}

#[tokio::test]
async fn pull_combined_status_empty_payload_is_no_ci() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/no-ci/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let cs = crate::api::pull::combined_status(&c, "o", "r", "no-ci")
        .await
        .expect("empty combined status should be no-CI, not a decode error");
    assert_eq!(cs.sha, "no-ci");
    assert_eq!(cs.state, "");
    assert_eq!(cs.total_count, 0);
    assert!(cs.statuses.is_empty());
}

// A minimal repo object that satisfies the non-defaulted fields of
// `api::repo::Repo`. Used by the search/star tests below.
fn minimal_repo_json(name: &str) -> serde_json::Value {
    json!({
        "id": 1,
        "name": name,
        "full_name": format!("o/{name}"),
        "owner": { "id": 7, "login": "o" },
        "html_url": "https://x/o/".to_string() + name,
        "clone_url": "https://x/o/x.git",
        "ssh_url": "git@x:o/x.git",
        "updated_at": "2026-01-01T00:00:00Z",
    })
}

#[tokio::test]
async fn search_issues_sends_query_type_and_clamped_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/issues/search"))
        .and(query_param("q", "bug"))
        .and(query_param("type", "issues"))
        // 999 is clamped to the per-page cap of 50.
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 5, "number": 3, "title": "a bug", "state": "open",
                "user": { "id": 1, "login": "alice" },
                "html_url": "https://x/o/r/issues/3",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let issues = crate::api::search::issues(&c, "bug", "issues", 999)
        .await
        .expect("search");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].title, "a bug");
}

#[tokio::test]
async fn search_repos_unwraps_the_data_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/search"))
        .and(query_param("q", "fj"))
        .and(query_param("limit", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "data": [ minimal_repo_json("fj") ],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repos = crate::api::search::repos(&c, "fj", 10)
        .await
        .expect("repos");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name, "fj");
}

#[tokio::test]
async fn search_users_unwraps_the_data_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/search"))
        .and(query_param("q", "ali"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "data": [ { "id": 1, "login": "alice" } ],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let users = crate::api::search::users(&c, "ali", 30)
        .await
        .expect("users");
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].login, "alice");
}

#[tokio::test]
async fn user_current_decodes_the_authenticated_user() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42, "login": "alice", "is_admin": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let u = crate::api::user::current(&c).await.expect("user");
    assert_eq!(u.login, "alice");
    assert!(u.is_admin);
    // omitted optionals default.
    assert_eq!(u.email, "");
}

#[tokio::test]
async fn release_list_sends_limit_and_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/releases"))
        .and(query_param("limit", "30"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1, "tag_name": "v1.0.0", "target_commitish": "main",
                "draft": false, "prerelease": false, "html_url": "https://x/r/1",
                "created_at": "2026-01-01T00:00:00Z", "published_at": null
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let rels = crate::api::release::list(
        &c,
        "o",
        "r",
        crate::api::release::ListOptions {
            limit: 30,
            ..Default::default()
        },
    )
    .await
    .expect("list");
    assert_eq!(rels.items.len(), 1);
    assert_eq!(rels.items[0].tag_name, "v1.0.0");
}

#[tokio::test]
async fn release_view_404_names_missing_tag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/releases/tags/v9.9.9"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::release::get_by_tag(&c, "o", "r", "v9.9.9")
        .await
        .expect_err("missing release should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("release 'v9.9.9' not found"), "got: {msg}");
}

#[tokio::test]
async fn release_create_posts_only_the_set_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/releases"))
        // name/body/target are None, so they are skipped from the JSON body.
        .and(body_json(json!({
            "tag_name": "v2.0.0",
            "draft": false,
            "prerelease": true,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 9, "tag_name": "v2.0.0", "target_commitish": "main",
            "draft": false, "prerelease": true, "html_url": "https://x/r/9",
            "created_at": "2026-01-01T00:00:00Z", "published_at": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let payload = crate::api::release::CreateRelease {
        tag_name: "v2.0.0",
        name: None,
        body: None,
        target_commitish: None,
        draft: false,
        prerelease: true,
    };
    let r = crate::api::release::create(&c, "o", "r", &payload)
        .await
        .expect("create");
    assert_eq!(r.id, 9);
    assert!(r.prerelease);
}

#[tokio::test]
async fn release_edit_patches_set_fields_only() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/releases/9"))
        .and(body_json(json!({ "name": "Renamed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "tag_name": "v2.0.0", "name": "Renamed",
            "target_commitish": "main", "draft": false, "prerelease": false,
            "html_url": "https://x/r/9", "created_at": "2026-01-01T00:00:00Z",
            "published_at": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let payload = crate::api::release::EditRelease {
        name: Some("Renamed"),
        ..Default::default()
    };
    let r = crate::api::release::edit(&c, "o", "r", 9, &payload)
        .await
        .expect("edit");
    assert_eq!(r.name, "Renamed");
}

#[tokio::test]
async fn release_delete_succeeds_on_204() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/releases/9"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::release::delete(&c, "o", "r", 9)
        .await
        .expect("delete");
}

#[tokio::test]
async fn repo_watch_puts_subscription_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/subscription"))
        .and(body_json(json!({ "subscribed": true, "ignored": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "subscribed": true, "ignored": false, "url": "https://x/sub"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let sub = crate::api::repo::watch(&c, "o", "r").await.expect("watch");
    assert!(sub.subscribed);
}

#[tokio::test]
async fn repo_unwatch_deletes_subscription() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/subscription"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::unwatch(&c, "o", "r")
        .await
        .expect("unwatch");
}

#[tokio::test]
async fn repo_star_and_unstar_hit_user_starred() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/user/starred/o/r"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/starred/o/r"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::star(&c, "o", "r").await.expect("star");
    crate::api::repo::unstar(&c, "o", "r")
        .await
        .expect("unstar");
}

#[tokio::test]
async fn repo_list_starred_clamps_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/starred"))
        .and(query_param("limit", "50"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([minimal_repo_json("starred")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repos = crate::api::repo::list_starred(&c, 200)
        .await
        .expect("starred");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name, "starred");
}

#[tokio::test]
async fn repo_list_for_user_pages_with_limit_and_page() {
    use crate::api::repo::ListOptions;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/repos"))
        .and(query_param("limit", "10"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([minimal_repo_json("a")])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::repo::list_for_user(
        &c,
        ListOptions {
            limit: 10,
            page: 2,
            paginate: false,
            query: None,
        },
    )
    .await
    .expect("list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "a");
}

#[tokio::test]
async fn repo_create_for_current_user_skips_none_fields() {
    use crate::api::repo::CreateRepo;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/repos"))
        // description and default_branch are None -> omitted from the body.
        .and(body_json(json!({
            "name": "newrepo",
            "private": true,
            "auto_init": false,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(minimal_repo_json("newrepo")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateRepo {
        name: "newrepo",
        description: None,
        private: true,
        default_branch: None,
        auto_init: false,
    };
    let r = crate::api::repo::create_for_current_user(&c, &body)
        .await
        .expect("create");
    assert_eq!(r.name, "newrepo");
}

#[tokio::test]
async fn repo_create_for_org_targets_the_org_path() {
    use crate::api::repo::CreateRepo;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs/myorg/repos"))
        .respond_with(ResponseTemplate::new(201).set_body_json(minimal_repo_json("orgrepo")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateRepo {
        name: "orgrepo",
        description: Some("hi"),
        private: false,
        default_branch: None,
        auto_init: true,
    };
    let r = crate::api::repo::create_for_org(&c, "myorg", &body)
        .await
        .expect("create");
    assert_eq!(r.name, "orgrepo");
}

#[tokio::test]
async fn repo_edit_patches_only_set_fields() {
    use crate::api::repo::EditRepo;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r"))
        .and(body_json(json!({ "archived": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_repo_json("r")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = EditRepo {
        archived: Some(true),
        ..Default::default()
    };
    crate::api::repo::edit(&c, "o", "r", &body)
        .await
        .expect("edit");
}

#[tokio::test]
async fn repo_delete_succeeds_on_204() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::delete(&c, "o", "r")
        .await
        .expect("delete");
}

#[tokio::test]
async fn repo_rename_patches_the_name_field() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r"))
        .and(body_json(json!({ "name": "renamed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_repo_json("renamed")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::rename(&c, "o", "r", "renamed")
        .await
        .expect("rename");
}

#[tokio::test]
async fn repo_sync_with_upstream_posts_the_branch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/merge-upstream"))
        .and(body_json(json!({ "branch": "main" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "merged": true })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::sync_with_upstream(&c, "o", "r", "main")
        .await
        .expect("sync");
}

#[tokio::test]
async fn repo_sync_with_upstream_maps_404_to_actionable_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/merge-upstream"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::repo::sync_with_upstream(&c, "o", "r", "main")
        .await
        .expect_err("should error");
    assert!(err.to_string().contains("merge-upstream"));
}

#[tokio::test]
async fn repo_mirror_sync_posts_and_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/mirror-sync"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::mirror_sync(&c, "o", "r")
        .await
        .expect("mirror-sync");
}

// ---- labels ----

#[tokio::test]
async fn label_list_decodes_rows() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "bug", "color": "ee0000", "description": "defects" },
            { "id": 2, "name": "docs", "color": "00ee00" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let labels = crate::api::label::list(&c, "o", "r").await.expect("list");
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0].name, "bug");
    // description defaults when omitted.
    assert_eq!(labels[1].description, "");
}

#[tokio::test]
async fn label_resolve_names_to_ids_returns_ids_and_errors_on_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "bug", "color": "ee0000" },
            { "id": 11, "name": "help wanted", "color": "00ee00" }
        ])))
        .expect(2)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let names = vec!["bug".to_string(), "help wanted".to_string()];
    let ids = crate::api::label::resolve_names_to_ids(&c, "o", "r", &names)
        .await
        .expect("resolve");
    assert_eq!(ids, vec![10, 11]);

    let missing = vec!["missing".to_string()];
    let err = crate::api::label::resolve_names_to_ids(&c, "o", "r", &missing)
        .await
        .expect_err("unknown label should error");
    assert!(err.to_string().contains("no label named 'missing'"));
}

#[tokio::test]
async fn label_create_posts_color_and_skips_none_description() {
    use crate::api::label::CreateLabel;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/labels"))
        .and(body_json(json!({ "name": "bug", "color": "ee0000" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "name": "bug", "color": "ee0000"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateLabel {
        name: "bug",
        color: "ee0000",
        description: None,
        exclusive: None,
    };
    let l = crate::api::label::create(&c, "o", "r", &body)
        .await
        .expect("create");
    assert_eq!(l.id, 1);
}

#[tokio::test]
async fn label_edit_patches_only_set_fields() {
    use crate::api::label::EditLabel;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/labels/1"))
        .and(body_json(json!({ "color": "0000ee" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "bug", "color": "0000ee"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = EditLabel {
        color: Some("0000ee"),
        ..Default::default()
    };
    let l = crate::api::label::edit(&c, "o", "r", 1, &body)
        .await
        .expect("edit");
    assert_eq!(l.color, "0000ee");
}

#[tokio::test]
async fn label_delete_succeeds_on_204() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/labels/1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::label::delete(&c, "o", "r", 1)
        .await
        .expect("delete");
}

#[tokio::test]
async fn issue_labels_add_and_remove_use_issue_label_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/labels"))
        .and(body_json(json!({ "labels": [10, 11] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "bug", "color": "ee0000" },
            { "id": 11, "name": "help wanted", "color": "00ee00" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/labels/10"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let labels = crate::api::label::add_to_issue(&c, "o", "r", 3, vec![10, 11])
        .await
        .expect("add labels");
    assert_eq!(labels.len(), 2);
    crate::api::label::remove_from_issue(&c, "o", "r", 3, 10)
        .await
        .expect("remove label");
}

// ---- milestones ----

fn milestone_json(id: u64, title: &str, state: &str) -> serde_json::Value {
    json!({ "id": id, "title": title, "state": state })
}

#[tokio::test]
async fn milestone_list_sends_state_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/milestones"))
        .and(query_param("state", "open"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([milestone_json(1, "v1", "open")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let ms = crate::api::milestone::list(&c, "o", "r", "open")
        .await
        .expect("list");
    assert_eq!(ms.len(), 1);
    assert_eq!(ms[0].title, "v1");
}

#[tokio::test]
async fn milestone_view_404_names_missing_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/milestones/99"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::milestone::get(&c, "o", "r", 99)
        .await
        .expect_err("missing milestone should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("milestone #99 not found"), "got: {msg}");
}

#[tokio::test]
async fn milestone_create_skips_none_fields() {
    use crate::api::milestone::CreateMilestone;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/milestones"))
        .and(body_json(json!({ "title": "v2" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(milestone_json(2, "v2", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateMilestone {
        title: "v2",
        description: None,
        due_on: None,
    };
    let m = crate::api::milestone::create(&c, "o", "r", &body)
        .await
        .expect("create");
    assert_eq!(m.id, 2);
}

#[tokio::test]
async fn milestone_edit_can_close() {
    use crate::api::milestone::EditMilestone;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/milestones/2"))
        .and(body_json(json!({ "state": "closed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(milestone_json(2, "v2", "closed")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = EditMilestone {
        state: Some("closed"),
        ..Default::default()
    };
    let m = crate::api::milestone::edit(&c, "o", "r", 2, &body)
        .await
        .expect("edit");
    assert_eq!(m.state, "closed");
}

#[tokio::test]
async fn milestone_assign_sets_and_clears_via_patch_issue() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/7"))
        .and(body_json(json!({ "milestone": 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/8"))
        .and(body_json(json!({ "milestone": null })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::milestone::assign(&c, "o", "r", 7, Some(2))
        .await
        .expect("assign");
    crate::api::milestone::assign(&c, "o", "r", 8, None)
        .await
        .expect("detach");
}

#[tokio::test]
async fn milestone_delete_succeeds_on_204() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/milestones/2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::milestone::delete(&c, "o", "r", 2)
        .await
        .expect("delete");
}

// ---- webhooks ----

#[tokio::test]
async fn hook_list_decodes_type_and_config() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/hooks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 3, "type": "gitea", "events": ["push"], "active": true,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "config": { "url": "https://hook.example" }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let hooks = crate::api::hook::list(&c, "o", "r").await.expect("list");
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].type_, "gitea");
    assert_eq!(hooks[0].config["url"], "https://hook.example");
}

#[tokio::test]
async fn hook_create_posts_type_config_events() {
    use crate::api::hook::CreateHook;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/hooks"))
        .and(body_json(json!({
            "type": "gitea",
            "config": { "url": "https://hook.example", "content_type": "json" },
            "events": ["push", "pull_request"],
            "active": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 3, "type": "gitea", "events": ["push", "pull_request"], "active": true,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "config": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateHook {
        type_: "gitea",
        config: json!({ "url": "https://hook.example", "content_type": "json" }),
        events: vec!["push", "pull_request"],
        active: true,
    };
    let h = crate::api::hook::create(&c, "o", "r", &body)
        .await
        .expect("create");
    assert_eq!(h.id, 3);
}

#[tokio::test]
async fn hook_delete_and_test_post_to_their_paths() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/hooks/3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/hooks/3/tests"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::hook::delete(&c, "o", "r", 3)
        .await
        .expect("delete");
    crate::api::hook::test(&c, "o", "r", 3).await.expect("test");
}

// ---- notifications ----

#[tokio::test]
async fn notification_list_sends_all_and_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .and(query_param("all", "true"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1, "unread": true, "pinned": false,
                "updated_at": "2026-01-01T00:00:00Z",
                "url": "https://x/n/1", "html_url": "https://x/n/1",
                "subject": { "title": "a thing", "type": "Issue", "state": "open" },
                "repository": { "full_name": "o/r", "html_url": "https://x/o/r" }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let ns = crate::api::notification::list(&c, true, 99)
        .await
        .expect("list");
    assert_eq!(ns.len(), 1);
    assert_eq!(ns[0].subject.title, "a thing");
}

#[tokio::test]
async fn notification_mark_all_read_puts() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/notifications"))
        .respond_with(ResponseTemplate::new(205))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::notification::mark_all_read(&c)
        .await
        .expect("mark read");
}

// ---- orgs ----

#[tokio::test]
async fn org_list_for_user_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/orgs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "username": "acme", "avatar_url": "https://x/a" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let orgs = crate::api::org::list_for_user(&c).await.expect("orgs");
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0].username, "acme");
}

#[tokio::test]
async fn org_get_and_list_teams() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "username": "acme", "avatar_url": "https://x/a"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/acme/teams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 7, "name": "Owners", "permission": "owner" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let org = crate::api::org::get(&c, "acme").await.expect("get");
    assert_eq!(org.username, "acme");
    let teams = crate::api::org::list_teams(&c, "acme")
        .await
        .expect("teams");
    assert_eq!(teams[0].name, "Owners");
}

// ---- branch protection ----

fn protection_json(branch: &str) -> serde_json::Value {
    json!({ "branch_name": branch, "required_approvals": 2 })
}

#[tokio::test]
async fn protect_list_and_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([protection_json("main")])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(protection_json("main")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let rules = crate::api::protect::list(&c, "o", "r").await.expect("list");
    assert_eq!(rules[0].branch_name, "main");
    let rule = crate::api::protect::get(&c, "o", "r", "main")
        .await
        .expect("get");
    assert_eq!(rule.required_approvals, 2);
}

#[tokio::test]
async fn protect_view_404_names_missing_branch_protection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/feature%2Fx"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::protect::get(&c, "o", "r", "feature/x")
        .await
        .expect_err("missing branch protection should error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("branch protection for 'feature/x' not found"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn protect_create_posts_branch_name() {
    use crate::api::protect::CreateBranchProtection;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/branch_protections"))
        .and(body_json(json!({
            "branch_name": "main",
            "enable_push": false,
            "require_signed_commits": true,
            "block_on_outdated_branch": false,
            "required_approvals": 1,
            "enable_status_check": false,
            "status_check_contexts": []
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(protection_json("main")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let contexts: Vec<String> = Vec::new();
    let body = CreateBranchProtection {
        branch_name: "main",
        enable_push: false,
        require_signed_commits: true,
        block_on_outdated_branch: false,
        required_approvals: 1,
        enable_status_check: false,
        status_check_contexts: &contexts,
    };
    crate::api::protect::create(&c, "o", "r", &body)
        .await
        .expect("create");
}

#[tokio::test]
async fn protect_delete_succeeds_on_204() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::protect::delete(&c, "o", "r", "main")
        .await
        .expect("delete");
}

// ---- templates ----

fn repo_json() -> serde_json::Value {
    json!({
        "id": 42,
        "name": "r",
        "full_name": "o/r",
        "owner": { "id": 1, "login": "o" },
        "html_url": "https://x/o/r",
        "clone_url": "https://x/o/r.git",
        "ssh_url": "git@x:o/r.git",
        "default_branch": "main",
        "updated_at": "2026-05-30T00:00:00Z"
    })
}

fn content_file(path_value: &str, body: &str) -> serde_json::Value {
    use base64::Engine;
    let name = path_value.rsplit('/').next().unwrap_or(path_value);
    json!({
        "name": name,
        "path": path_value,
        "type": "file",
        "encoding": "base64",
        "content": base64::engine::general_purpose::STANDARD.encode(body),
    })
}

#[tokio::test]
async fn template_list_uses_issue_templates_api_when_available() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issue_templates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "name": "Bug report",
                "file_name": ".forgejo/issue_template/bug.md",
                "content": "\n## What happened\n"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let templates =
        crate::api::template::list(&c, "o", "r", crate::api::template::TemplateKind::Issue)
            .await
            .expect("templates");
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "Bug report");
    assert_eq!(templates[0].content, "## What happened");
}

#[tokio::test]
async fn template_list_falls_back_to_pr_template_file_on_default_branch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/repos/o/r/contents/.forgejo/pull_request_template.md",
        ))
        .and(query_param("ref", "main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(content_file(
            ".forgejo/pull_request_template.md",
            "## Summary\n",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let templates =
        crate::api::template::list(&c, "o", "r", crate::api::template::TemplateKind::Pull)
            .await
            .expect("templates");
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "pull_request_template");
    assert_eq!(templates[0].content, "## Summary");
}

// ---- issues ----

fn issue_json(number: u64, title: &str, state: &str) -> serde_json::Value {
    json!({
        "id": number, "number": number, "title": title, "state": state,
        "user": { "id": 1, "login": "alice" },
        "html_url": format!("https://x/o/r/issues/{number}"),
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    })
}

fn comment_json(id: u64, body: &str) -> serde_json::Value {
    json!({
        "id": id, "body": body,
        "user": { "id": 1, "login": "alice" },
        "html_url": format!("https://x/c/{id}"),
        "created_at": "2026-01-01T00:00:00Z"
    })
}

#[tokio::test]
async fn issue_list_sends_milestone_and_mentioned_filters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(query_param("state", "open"))
        .and(query_param("type", "issues"))
        .and(query_param("milestones", "v1"))
        .and(query_param("mentioned_by", "alice"))
        .and(query_param("limit", "30"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue_json(
            3,
            "mentioned in v1",
            "open"
        )])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::issue::list(
        &c,
        "o",
        "r",
        crate::api::issue::ListOptions {
            milestone: Some("v1"),
            mentioned: Some("alice"),
            ..Default::default()
        },
    )
    .await
    .expect("filtered list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 3);
}

#[tokio::test]
async fn issue_get_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let i = crate::api::issue::get(&c, "o", "r", 3).await.expect("get");
    assert_eq!(i.title, "a bug");
}

#[tokio::test]
async fn issue_create_skips_none_fields() {
    use crate::api::issue::CreateIssue;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(body_json(json!({ "title": "new" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(4, "new", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateIssue {
        title: "new",
        body: None,
        assignees: None,
        labels: None,
        milestone: None,
    };
    let i = crate::api::issue::create(&c, "o", "r", &body)
        .await
        .expect("create");
    assert_eq!(i.number, 4);
}

#[tokio::test]
async fn issue_create_posts_resolved_label_ids_and_assignees() {
    use crate::api::issue::CreateIssue;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(body_json(json!({
            "title": "new",
            "assignees": ["alice", "bob"],
            "labels": [10, 11]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(4, "new", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateIssue {
        title: "new",
        body: None,
        assignees: Some(vec!["alice".to_string(), "bob".to_string()]),
        labels: Some(vec![10, 11]),
        milestone: None,
    };
    let i = crate::api::issue::create(&c, "o", "r", &body)
        .await
        .expect("create");
    assert_eq!(i.number, 4);
}

#[tokio::test]
async fn issue_edit_can_close() {
    use crate::api::issue::EditIssue;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .and(body_json(json!({ "state": "closed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "closed")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = EditIssue {
        state: Some("closed"),
        ..Default::default()
    };
    let i = crate::api::issue::edit(&c, "o", "r", 3, &body)
        .await
        .expect("edit");
    assert_eq!(i.state, "closed");
}

#[tokio::test]
async fn issue_assignees_add_and_remove_patch_merged_set() {
    let server = MockServer::start().await;
    // Forgejo has no /issues/{n}/assignees sub-resource: add/remove read the
    // current assignees and PATCH the issue with the merged set.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 3, "number": 3, "title": "a bug", "state": "open",
            "user": { "id": 1, "login": "alice" },
            "assignees": [{ "id": 1, "login": "alice" }, { "id": 2, "login": "bob" }],
            "html_url": "https://x/o/r/issues/3",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        })))
        .expect(2)
        .mount(&server)
        .await;
    // add ["alice","bob"]: both already assigned, so the merged set is unchanged.
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .and(body_json(json!({ "assignees": ["alice", "bob"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    // remove ["bob"]: drops bob, keeps alice.
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .and(body_json(json!({ "assignees": ["alice"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let i = crate::api::issue::add_assignees(
        &c,
        "o",
        "r",
        3,
        vec!["alice".to_string(), "bob".to_string()],
    )
    .await
    .expect("add assignees");
    assert_eq!(i.number, 3);
    let i = crate::api::issue::remove_assignees(&c, "o", "r", 3, vec!["bob".to_string()])
        .await
        .expect("remove assignees");
    assert_eq!(i.number, 3);
}

#[tokio::test]
async fn issue_lock_and_unlock_use_shared_issue_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/issues/3/lock"))
        .and(body_json(json!({ "lock_reason": "too-heated" })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/issues/4/lock"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/lock"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::issue::lock(&c, "o", "r", 3, Some("too-heated"))
        .await
        .expect("lock with reason");
    crate::api::issue::lock(&c, "o", "r", 4, None)
        .await
        .expect("lock without reason");
    crate::api::issue::unlock(&c, "o", "r", 3)
        .await
        .expect("unlock");
}

#[tokio::test]
async fn issue_comment_roundtrip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/comments"))
        .and(body_json(json!({ "body": "hi there" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(comment_json(11, "hi there")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/comments"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([comment_json(11, "hi there")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let made = crate::api::issue::comment(&c, "o", "r", 3, "hi there")
        .await
        .expect("comment");
    assert_eq!(made.id, 11);
    let listed = crate::api::issue::list_comments(&c, "o", "r", 3)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn issue_edit_and_delete_comment_by_id() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/11"))
        .and(body_json(json!({ "body": "edited" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(comment_json(11, "edited")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/comments/11"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let c2 = crate::api::issue::edit_comment(&c, "o", "r", 11, "edited")
        .await
        .expect("edit");
    assert_eq!(c2.body, "edited");
    crate::api::issue::delete_comment(&c, "o", "r", 11)
        .await
        .expect("delete");
}

fn comment_by(id: u64, login: &str) -> serde_json::Value {
    json!({
        "id": id, "body": "x",
        "user": { "id": id, "login": login },
        "html_url": format!("https://x/c/{id}"),
        "created_at": "2026-01-01T00:00:00Z"
    })
}

// `pr comment --edit-last` is "fetch my login, list the PR's comments, pick my
// newest, PATCH it" — PRs reuse the issue comment endpoints. This drives that
// full sequence against the mock and asserts the selection lands on the
// caller's most recent comment, not someone else's later one.
#[tokio::test]
async fn pr_comment_edit_last_selects_callers_newest_comment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "login": "alice" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/7/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            comment_by(11, "alice"),
            comment_by(12, "bob"),
            comment_by(14, "alice"),
            comment_by(15, "bob"),
        ])))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/14"))
        .and(body_json(json!({ "body": "build green" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(comment_json(14, "build green")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let me = crate::api::user::current(&c).await.expect("current user");
    let comments = crate::api::issue::list_comments(&c, "o", "r", 7)
        .await
        .expect("list");
    let target = crate::cli::pr_comment::last_own_comment(&comments, &me.login)
        .expect("alice has a comment");
    assert_eq!(target.id, 14, "newest comment by the caller wins");
    let edited = crate::api::issue::edit_comment(&c, "o", "r", target.id, "build green")
        .await
        .expect("edit");
    assert_eq!(edited.body, "build green");
}

// `--create-if-none` / `--create-or-edit`: when the caller has no comment yet,
// the selection returns None and the command falls through to POST a new one.
#[tokio::test]
async fn pr_comment_create_if_none_posts_when_caller_has_no_comment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "login": "alice" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/7/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([comment_by(12, "bob")])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/7/comments"))
        .and(body_json(json!({ "body": "build running" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(comment_json(20, "build running")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let me = crate::api::user::current(&c).await.expect("current user");
    let comments = crate::api::issue::list_comments(&c, "o", "r", 7)
        .await
        .expect("list");
    assert!(
        crate::cli::pr_comment::last_own_comment(&comments, &me.login).is_none(),
        "alice has no comment to edit"
    );
    let made = crate::api::issue::comment(&c, "o", "r", 7, "build running")
        .await
        .expect("create");
    assert_eq!(made.id, 20);
}

// A bare comment id that does not exist surfaces the API 404 rather than a
// silent success, for both edit and delete. The client wraps 404s in a
// friendly `not found: {resource}` context, so the status code lives in the
// error's source chain; assert against the full chain (`{:#}`) rather than the
// outermost message that `to_string()` returns.
#[tokio::test]
async fn pr_comment_edit_and_delete_missing_id_error() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/comments/999"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "message": "comment does not exist" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/comments/999"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "message": "comment does not exist" })),
        )
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let edit_err = crate::api::issue::edit_comment(&c, "o", "r", 999, "x")
        .await
        .expect_err("missing comment should error");
    assert!(format!("{edit_err:#}").contains("404"));
    let del_err = crate::api::issue::delete_comment(&c, "o", "r", 999)
        .await
        .expect_err("missing comment should error");
    assert!(format!("{del_err:#}").contains("404"));
}

#[tokio::test]
async fn issue_delete_and_pin_use_right_verbs() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    // pin -> POST, unpin -> DELETE on the /pin subpath.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/pin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/pin"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::issue::set_pinned(&c, "o", "r", 3, true)
        .await
        .expect("pin");
    crate::api::issue::set_pinned(&c, "o", "r", 3, false)
        .await
        .expect("unpin");
    crate::api::issue::delete(&c, "o", "r", 3)
        .await
        .expect("delete");
}

#[tokio::test]
async fn issue_delete_comment_missing_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/comments/11"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "not found"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::issue::delete_comment(&c, "o", "r", 11)
        .await
        .expect_err("should fail");
    assert!(err.to_string().to_lowercase().contains("not found"));
}

// ---- actions: artifacts + workflow files ----

#[tokio::test]
async fn workflow_list_artifacts_unwraps_envelope() {
    let server = MockServer::start().await;
    // list_artifacts hits the web route at the host origin, not /api/v1.
    Mock::given(method("GET"))
        .and(path("/o/r/actions/runs/5/artifacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artifacts": [ { "name": "logs", "size": 12, "status": "completed" } ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let arts = crate::api::workflow::list_artifacts(&c, "o", "r", 5)
        .await
        .expect("artifacts");
    assert_eq!(arts.len(), 1);
    assert_eq!(arts[0].name, "logs");
}

#[tokio::test]
async fn workflow_list_artifacts_maps_404() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/o/r/actions/runs/9/artifacts"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let err = crate::api::workflow::list_artifacts(&c, "o", "r", 9)
        .await
        .expect_err("should 404");
    assert!(crate::api::workflow::is_actions_unavailable(&err));
    let msg = err.to_string();
    assert!(msg.contains("cannot read artifacts for run #9"), "{msg}");
    assert!(msg.contains("/o/r/actions/runs/9"), "{msg}");
}

#[tokio::test]
async fn workflow_list_workflows_reads_both_dirs() {
    let server = MockServer::start().await;
    // .forgejo/workflows has a yaml file; .github/workflows is absent (404).
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/.forgejo/workflows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "ci.yml", "path": ".forgejo/workflows/ci.yml", "type": "file" },
            { "name": "README.md", "path": ".forgejo/workflows/README.md", "type": "file" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/.github/workflows"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let wfs = crate::api::workflow::list_workflows(&c, "o", "r")
        .await
        .expect("workflows");
    // Only the .yml is kept; README.md is filtered out.
    assert_eq!(wfs.len(), 1);
    assert_eq!(wfs[0].name, "ci.yml");
}

// ---- pull request: edit / ready / diff / unrequest review ----

fn pull_json(number: u64, state: &str) -> serde_json::Value {
    json!({
        "id": number, "number": number, "title": "A PR", "state": state,
        "user": { "id": 1, "login": "alice" },
        "html_url": format!("https://x/o/r/pulls/{number}"),
        "head": { "ref": "feature", "sha": "aaa" },
        "base": { "ref": "main", "sha": "bbb" },
        "created_at": "2026-05-30T00:00:00Z",
        "updated_at": "2026-05-30T00:01:00Z"
    })
}

#[tokio::test]
async fn pull_edit_can_close() {
    use crate::api::pull::EditPull;
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .and(body_json(json!({ "state": "closed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(12, "closed")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = EditPull {
        state: Some("closed"),
        ..Default::default()
    };
    let pr = crate::api::pull::edit(&c, "o", "r", 12, &body)
        .await
        .expect("edit");
    assert_eq!(pr.state, "closed");
}

#[tokio::test]
async fn pull_label_and_assignee_mutations_use_backing_issue_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "needs-review", "color": "ee0000" },
            { "id": 11, "name": "wip", "color": "00ee00" }
        ])))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/12/labels"))
        .and(body_json(json!({ "labels": [10] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 10, "name": "needs-review", "color": "ee0000" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/12/labels/11"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 12, "number": 12, "title": "A PR", "state": "open",
            "user": { "id": 1, "login": "alice" },
            "assignees": [{ "id": 2, "login": "bob" }],
            "html_url": "https://x/o/r/issues/12",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        })))
        .expect(2)
        .mount(&server)
        .await;
    // add ["alice"]: appended to the existing bob.
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .and(body_json(json!({ "assignees": ["bob", "alice"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(12, "A PR", "open")))
        .expect(1)
        .mount(&server)
        .await;
    // remove ["bob"]: clears the list.
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/issues/12"))
        .and(body_json(json!({ "assignees": [] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(12, "A PR", "open")))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let add_names = vec!["needs-review".to_string()];
    let add_ids = crate::api::label::resolve_names_to_ids(&c, "o", "r", &add_names)
        .await
        .expect("resolve add labels");
    crate::api::label::add_to_issue(&c, "o", "r", 12, add_ids)
        .await
        .expect("add labels to pr");

    let remove_names = vec!["wip".to_string()];
    let remove_ids = crate::api::label::resolve_names_to_ids(&c, "o", "r", &remove_names)
        .await
        .expect("resolve remove labels");
    for label_id in remove_ids {
        crate::api::label::remove_from_issue(&c, "o", "r", 12, label_id)
            .await
            .expect("remove label from pr");
    }

    crate::api::issue::add_assignees(&c, "o", "r", 12, vec!["alice".to_string()])
        .await
        .expect("add assignee to pr");
    crate::api::issue::remove_assignees(&c, "o", "r", 12, vec!["bob".to_string()])
        .await
        .expect("remove assignee from pr");
}

fn draft_pull_json(number: u64, title: &str, draft: bool) -> serde_json::Value {
    let mut v = pull_json(number, "open");
    v["title"] = json!(title);
    v["draft"] = json!(draft);
    v
}

#[tokio::test]
async fn pull_ready_strips_the_wip_prefix_from_the_title() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_pull_json(
            12,
            "wip: Add thing",
            true,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .and(body_json(json!({ "title": "Add thing" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_pull_json(
            12,
            "Add thing",
            false,
        )))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    assert!(crate::api::pull::ready(&c, "o", "r", 12)
        .await
        .expect("ready"));
}

#[tokio::test]
async fn pull_ready_leaves_a_ready_pull_alone() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(12, "open")))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(12, "open")))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    assert!(!crate::api::pull::ready(&c, "o", "r", 12)
        .await
        .expect("ready"));
}

#[tokio::test]
async fn pull_ready_refuses_a_prefix_it_does_not_know() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_pull_json(
            12,
            "Draft: Add thing",
            true,
        )))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(12, "open")))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::ready(&c, "o", "r", 12)
        .await
        .expect_err("unknown prefix");
    assert!(err.to_string().contains("fj pr edit 12 --title"), "{err}");
}

#[tokio::test]
async fn pull_ready_fails_when_the_server_still_reports_a_draft() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_pull_json(
            12,
            "[WIP] Add thing",
            true,
        )))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_pull_json(
            12,
            "Add thing",
            true,
        )))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::ready(&c, "o", "r", 12)
        .await
        .expect_err("still a draft");
    assert!(err.to_string().contains("still a draft"), "{err}");
}

#[tokio::test]
async fn pull_create_draft_prefixes_the_title() {
    use crate::api::pull::CreatePull;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(body_json(json!({
            "title": "WIP: Add thing", "head": "feature", "base": "main"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(draft_pull_json(
            12,
            "WIP: Add thing",
            true,
        )))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let pr = crate::api::pull::create(
        &c,
        "o",
        "r",
        &CreatePull {
            title: "Add thing",
            head: "feature",
            base: "main",
            body: None,
            draft: true,
        },
    )
    .await
    .expect("create draft");
    assert!(pr.draft);
}

#[tokio::test]
async fn pull_diff_text_returns_raw_body() {
    let server = MockServer::start().await;
    let diff = "diff --git a/x b/x\n+new line\n";
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12.diff"))
        .respond_with(ResponseTemplate::new(200).set_body_string(diff))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let got = crate::api::pull::diff_text(&c, "o", "r", 12)
        .await
        .expect("diff");
    assert_eq!(got, diff);
}

#[tokio::test]
async fn pull_unrequest_reviewers_deletes_with_body() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/pulls/12/requested_reviewers"))
        .and(body_json(json!({ "reviewers": ["bob", "carol"] })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::unrequest_reviewers(&c, "o", "r", 12, vec!["bob".into(), "carol".into()])
        .await
        .expect("unrequest");
}

// ---- release assets ----

#[tokio::test]
async fn release_upload_asset_posts_multipart_with_name_query() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/releases/9/assets"))
        .and(query_param("name", "build.zip"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 5, "name": "build.zip", "size": 4,
            "browser_download_url": "https://x/a/build.zip",
            "created_at": "2026-01-01T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let asset = crate::api::release::upload_asset(&c, "o", "r", 9, "build.zip", vec![1, 2, 3, 4])
        .await
        .expect("upload");
    assert_eq!(asset.name, "build.zip");
    assert_eq!(asset.size, 4);
}

#[tokio::test]
async fn release_delete_asset_keys_under_release() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/releases/9/assets/5"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::release::delete_asset(&c, "o", "r", 9, 5)
        .await
        .expect("delete asset");
}

// ---- repo migrate / search ----

#[tokio::test]
async fn repo_migrate_posts_to_migrate_endpoint() {
    use crate::api::repo::Migrate;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/migrate"))
        .and(body_json(json!({
            "clone_addr": "https://github.com/o/x.git",
            "repo_name": "x",
            "repo_owner": "o",
            "mirror": false,
            "private": false,
            "service": "github"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(minimal_repo_json("x")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = Migrate {
        clone_addr: "https://github.com/o/x.git",
        repo_name: "x",
        repo_owner: "o",
        mirror: false,
        private: false,
        description: None,
        auth_username: None,
        auth_password: None,
        auth_token: None,
        service: "github",
        mirror_interval: None,
    };
    let repo = crate::api::repo::migrate(&c, &body).await.expect("migrate");
    assert_eq!(repo.name, "x");
}

#[tokio::test]
async fn repo_search_unwraps_envelope_and_pages() {
    use crate::api::repo::ListOptions;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/search"))
        .and(query_param("q", "fj"))
        .and(query_param("limit", "10"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "data": [ minimal_repo_json("fj") ],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::repo::search(
        &c,
        ListOptions {
            limit: 10,
            page: 1,
            paginate: false,
            query: Some("fj"),
        },
    )
    .await
    .expect("search");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "fj");
}

// --- `--json` paths for the read-only inspect/list surface (rasterstate/fj#112) ---
//
// These assert the exact path the new `--json` branches take: fetch the typed
// struct over the wire, then `serde_json::to_value` it — the same value the CLI
// hands to `output::print_json`. We also run the resulting value through the
// `--json-fields` projector so the gh-style field selection is covered against
// each new shape.

/// Project `value` through `--json-fields spec`, mirroring `output::print_json`
/// when the global selector is set.
fn project_fields(value: &serde_json::Value, spec: &str) -> serde_json::Value {
    let fields = crate::output::json_filter::parse_fields(spec);
    crate::output::json_filter::project(value, &fields)
}

#[tokio::test]
async fn repo_list_json_shape_is_bare_array_and_projects_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1,
                "name": "r1",
                "full_name": "o/r1",
                "owner": { "id": 1, "login": "o" },
                "description": "one",
                "private": false,
                "html_url": "https://forge.example/o/r1",
                "clone_url": "https://forge.example/o/r1.git",
                "ssh_url": "git@forge.example:o/r1.git",
                "updated_at": "2026-01-01T00:00:00Z"
            },
            {
                "id": 2,
                "name": "r2",
                "full_name": "o/r2",
                "owner": { "id": 1, "login": "o" },
                "description": "two",
                "private": true,
                "html_url": "https://forge.example/o/r2",
                "clone_url": "https://forge.example/o/r2.git",
                "ssh_url": "git@forge.example:o/r2.git",
                "updated_at": "2026-01-02T00:00:00Z"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let page = crate::api::repo::list_for_user(
        &c,
        crate::api::repo::ListOptions {
            limit: 30,
            page: 1,
            paginate: false,
            query: None,
        },
    )
    .await
    .expect("repos");

    let v = serde_json::to_value(&page.items).expect("serialize repo array");
    assert!(v.is_array(), "repo list --json should emit a bare array");
    let projected = project_fields(&v, "full_name");
    assert_eq!(
        projected,
        json!([{ "full_name": "o/r1" }, { "full_name": "o/r2" }])
    );
}

#[tokio::test]
async fn pr_checks_json_shape_and_projection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/deadbeef/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": "success",
            "sha": "deadbeef",
            "total_count": 1,
            "statuses": [{
                "status": "success",
                "context": "ci/build",
                "description": "all good",
                "target_url": "https://ci.example/1",
                "updated_at": "2024-01-01T00:00:00Z"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let cs = crate::api::pull::combined_status(&c, "o", "r", "deadbeef")
        .await
        .expect("combined status");
    let v = serde_json::to_value(&cs).expect("serialize");
    assert_eq!(v["state"], "success");
    assert_eq!(v["total_count"], 1);
    assert_eq!(v["statuses"][0]["context"], "ci/build");
    // `fj pr checks --json --json-fields state` is the merge-gate query.
    let projected = project_fields(&v, "state");
    assert_eq!(projected, json!({ "state": "success" }));
}

#[tokio::test]
async fn pr_files_json_shape_and_projection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "filename": "src/lib.rs", "status": "modified", "additions": 3, "deletions": 1 },
            { "filename": "README.md", "status": "added", "additions": 10, "deletions": 0 }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let files = crate::api::pull::files(&c, "o", "r", 7)
        .await
        .expect("files");
    let v = serde_json::to_value(&files).expect("serialize");
    assert_eq!(v[0]["filename"], "src/lib.rs");
    assert_eq!(v[0]["status"], "modified");
    assert_eq!(v[0]["additions"], 3);
    // Projection over an array projects each element independently.
    let projected = project_fields(&v, "filename,status");
    assert_eq!(
        projected,
        json!([
            { "filename": "src/lib.rs", "status": "modified" },
            { "filename": "README.md", "status": "added" }
        ])
    );
}

#[tokio::test]
async fn pr_commits_json_shape_and_projection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "sha": "abc1234def",
            "commit": {
                "message": "Fix the thing\n\nbody",
                "author": { "name": "Ada", "email": "ada@example.com" }
            }
        }])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let commits = crate::api::pull::commits(&c, "o", "r", 7)
        .await
        .expect("commits");
    let v = serde_json::to_value(&commits).expect("serialize");
    assert_eq!(v[0]["sha"], "abc1234def");
    assert_eq!(v[0]["commit"]["author"]["name"], "Ada");
    // Dotted-path projection reaches into the nested commit metadata.
    let projected = project_fields(&v, "sha,commit.author.name");
    assert_eq!(
        projected,
        json!([{ "sha": "abc1234def", "commit.author.name": "Ada" }])
    );
}

#[tokio::test]
async fn secret_list_json_shape_and_projection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/secrets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "TOKEN", "created_at": "2024-01-01T00:00:00Z" },
            { "name": "API_KEY", "created_at": "2024-02-01T00:00:00Z" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let secrets = crate::api::workflow::list_secrets(&c, "o", "r")
        .await
        .expect("secrets");
    let v = serde_json::to_value(&secrets).expect("serialize");
    assert_eq!(v[0]["name"], "TOKEN");
    assert!(v[0]["created_at"].is_string());
    let projected = project_fields(&v, "name");
    assert_eq!(
        projected,
        json!([{ "name": "TOKEN" }, { "name": "API_KEY" }])
    );
}

#[tokio::test]
async fn variable_list_json_shape_and_projection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/variables"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "REGION", "data": "us-west-2", "owner_id": 3, "repo_id": 9 },
            { "name": "TIER", "data": "gold", "owner_id": 3, "repo_id": 9 }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let vars = crate::api::workflow::list_variables(&c, "o", "r")
        .await
        .expect("variables");
    let v = serde_json::to_value(&vars).expect("serialize");
    assert_eq!(v[0]["name"], "REGION");
    // A variable is readable configuration: the value and the server's
    // scope ids are part of the JSON. No timestamp is invented.
    assert_eq!(v[0]["data"], "us-west-2");
    assert_eq!(v[0]["repo_id"], 9);
    assert!(v[0].get("created_at").is_none());
    let projected = project_fields(&v, "name");
    assert_eq!(projected, json!([{ "name": "REGION" }, { "name": "TIER" }]));
}

#[tokio::test]
async fn tag_view_404_names_missing_tag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/tags/feature%2Fx"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::tag::get(&c, "o", "r", "feature/x")
        .await
        .expect_err("missing tag should error");
    let msg = format!("{err:#}");
    assert!(msg.contains("tag 'feature/x' not found"), "got: {msg}");
}

#[tokio::test]
async fn tag_crud_uses_refs_api_paths() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "name": "v1.0.0",
                "message": "release",
                "commit": { "sha": "abcdef1234567890" },
                "tarball_url": "https://example.test/tar.gz",
                "zipball_url": "https://example.test/zip"
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/tags/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "v1.0.0",
            "message": "release",
            "commit": { "sha": "abcdef1234567890" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/tags"))
        .and(body_json(json!({
            "tag_name": "v1.0.1",
            "target": "main",
            "message": "next"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "name": "v1.0.1",
            "message": "next",
            "commit": { "sha": "1234567890abcdef" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/tags/feature%2Fx"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let tags = crate::api::tag::list(&c, "o", "r").await.expect("list");
    assert_eq!(tags[0].name, "v1.0.0");
    assert_eq!(tags[0].commit.sha, "abcdef1234567890");

    let tag = crate::api::tag::get(&c, "o", "r", "v1.0.0")
        .await
        .expect("get");
    assert_eq!(tag.message, "release");

    let created = crate::api::tag::create(
        &c,
        "o",
        "r",
        &crate::api::tag::CreateTag {
            tag_name: "v1.0.1",
            target: Some("main"),
            message: Some("next"),
        },
    )
    .await
    .expect("create");
    assert_eq!(created.name, "v1.0.1");

    crate::api::tag::delete(&c, "o", "r", "feature/x")
        .await
        .expect("delete");
}

#[tokio::test]
async fn branch_create_delete_uses_refs_api_paths() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/branches"))
        .and(body_json(json!({
            "new_branch_name": "feature/x",
            "old_branch_name": "main"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "name": "feature/x",
            "protected": false,
            "commit": { "id": "abcdef1234567890", "message": "init" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/branches/feature%2Fx"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let branch = crate::api::repo::create_branch(
        &c,
        "o",
        "r",
        &crate::api::repo::CreateBranch {
            new_branch_name: "feature/x",
            old_branch_name: Some("main"),
        },
    )
    .await
    .expect("create");
    assert_eq!(branch.name, "feature/x");

    crate::api::repo::delete_branch(&c, "o", "r", "feature/x")
        .await
        .expect("delete");
}

#[tokio::test]
async fn ref_mutation_errors_are_clear() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/branches/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "message": "branch does not exist"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/tags/v1"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "message": "tag is protected"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/tags"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "message": "tag already exists"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let missing = crate::api::repo::delete_branch(&c, "o", "r", "missing")
        .await
        .expect_err("missing branch should fail")
        .to_string();
    assert!(missing.contains("branch 'missing' not found"), "{missing}");

    let conflict = crate::api::tag::delete(&c, "o", "r", "v1")
        .await
        .expect_err("protected tag should fail")
        .to_string();
    assert!(conflict.contains("could not delete tag 'v1'"), "{conflict}");
    assert!(conflict.contains("tag is protected"), "{conflict}");

    let create_conflict = crate::api::tag::create(
        &c,
        "o",
        "r",
        &crate::api::tag::CreateTag {
            tag_name: "v1",
            target: None,
            message: None,
        },
    )
    .await
    .expect_err("duplicate tag should fail");
    let text = format!("{create_conflict:#}");
    assert!(text.contains("409 Conflict"), "{text}");
    assert!(text.contains("tag already exists"), "{text}");
}

// ---- Silent Fjord-Account session refresh on 401 (fix/client-silent-refresh) ----
//
// The shared `Client` now transparently refreshes an expired Fjord bearer on a
// 401 and retries once. These drive that wiring through a mock token flow (the
// `refresh_hook` seam stands in for the keychain-backed `fjord::oidc` refresh)
// so every `Client`-backed command (`fj api`/`pr`/`issue`/…) benefits.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn fjord_bearer_refreshes_once_and_retries_with_rotated_token() {
    let server = MockServer::start().await;
    // The stale bearer is rejected...
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "Bearer stale-token"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"message": "Token expired; sign in again"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    // ...and the rotated bearer from the refresh is accepted.
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "Bearer fresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1, "login": "alice"})))
        .expect(1)
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let hook = {
        let calls = Arc::clone(&calls);
        super::sync_refresh_hook(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some("fresh-token".to_string()))
        })
    };
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).unwrap();
    let c = Client::for_base_url_fjord_refresh(base, "stale-token".into(), hook).unwrap();

    let res: serde_json::Value = c
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect("request succeeds on the post-refresh retry");
    assert_eq!(res["login"], "alice");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the session is refreshed exactly once"
    );
}

#[tokio::test]
async fn fjord_bearer_without_refresh_token_surfaces_401() {
    let server = MockServer::start().await;
    // Only one request is expected: with no usable refresh token there is no
    // retry, so the original 401 surfaces unchanged.
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"message": "Token expired; sign in again"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let hook = {
        let calls = Arc::clone(&calls);
        super::sync_refresh_hook(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).unwrap();
    let c = Client::for_base_url_fjord_refresh(base, "stale-token".into(), hook).unwrap();

    let err = c
        .json::<serde_json::Value, _>(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect_err("the 401 surfaces when there is no refresh token");
    assert!(format!("{err:#}").contains("401"), "{err:#}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "refresh is checked once and, finding nothing, does not retry"
    );
}

#[tokio::test]
async fn pat_host_401_is_never_refreshed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token pat-token"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let base = Url::parse(&format!("{}/api/v1/", server.uri())).unwrap();
    let c =
        Client::for_base_url_with_scheme(base, Some("pat-token".into()), super::AuthScheme::Pat)
            .unwrap();

    let err = c
        .json::<serde_json::Value, _>(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect_err("PAT host 401 surfaces unchanged");
    assert!(format!("{err:#}").contains("401"), "{err:#}");
    // The mock's `.expect(1)` (verified on server drop) proves there was no
    // second, post-refresh request for a PAT host.
}

#[tokio::test]
async fn persistently_401_refreshes_at_most_once() {
    let server = MockServer::start().await;
    // The endpoint stays 401 even with the rotated token: initial request plus
    // exactly one post-refresh retry, then it gives up (no infinite loop).
    Mock::given(method("GET"))
        .and(path("/api/v1/loop"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicUsize::new(0));
    let hook = {
        let calls = Arc::clone(&calls);
        super::sync_refresh_hook(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some("fresh-token".to_string()))
        })
    };
    let base = Url::parse(&format!("{}/api/v1/", server.uri())).unwrap();
    let c = Client::for_base_url_fjord_refresh(base, "stale-token".into(), hook).unwrap();

    let err = c
        .json::<serde_json::Value, _>(Method::GET, "/api/v1/loop", &[], None::<&()>)
        .await
        .expect_err("a persistently-401 endpoint still errors after one refresh");
    assert!(format!("{err:#}").contains("401"), "{err:#}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "refresh is attempted at most once, never in a loop"
    );
}

// ---- notifications: per-thread read, unsubscribe, filtered paging ----

fn notification_thread(id: u64, unread: bool, kind: &str, repo: &str) -> serde_json::Value {
    json!({
        "id": id, "unread": unread, "pinned": false,
        "updated_at": "2026-01-01T00:00:00Z",
        "url": format!("https://x/api/v1/notifications/threads/{id}"),
        "subject": {
            "title": "a thing", "type": kind, "state": "open",
            "html_url": format!("https://x/{repo}/issues/5"),
            "url": format!("https://x/api/v1/repos/{repo}/issues/5")
        },
        "repository": { "full_name": repo }
    })
}

#[tokio::test]
async fn notification_mark_thread_read_accepts_205_with_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(ResponseTemplate::new(205))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::notification::mark_thread_read(&c, 7)
        .await
        .expect("205 with no body is success");
}

#[tokio::test]
async fn notification_mark_thread_read_surfaces_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "gone" })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::notification::mark_thread_read(&c, 7)
        .await
        .expect_err("404 is an error");
    assert!(format!("{err:#}").contains("404"), "{err:#}");
}

#[tokio::test]
async fn notification_get_thread_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, false, "Pull", "o/r")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let t = crate::api::notification::get_thread(&c, 7)
        .await
        .expect("thread");
    assert_eq!(t.id, 7);
    assert!(!t.unread);
    assert_eq!(t.subject.html_url, "https://x/o/r/issues/5");
}

#[tokio::test]
async fn notification_list_filtered_drops_muted_threads_on_every_page() {
    let server = MockServer::start().await;
    // limit 2 => per-page 2. Page 1 is all muted, page 2 is half muted, page
    // 3 is short, which ends the walk.
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .and(query_param("limit", "2"))
        .and(query_param_is_missing("page"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            notification_thread(1, true, "Issue", "Loud/Repo"),
            notification_thread(2, true, "Issue", "loud/repo"),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .and(query_param("limit", "2"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            notification_thread(3, true, "Issue", "o/r"),
            notification_thread(4, true, "Issue", "LOUD/REPO"),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .and(query_param("limit", "2"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 5, "unread": true, "pinned": false,
            "updated_at": "2026-01-01T00:00:00Z",
            "url": "https://x/api/v1/notifications/threads/5",
            "subject": { "title": "no repo", "type": "Issue", "state": "open" },
            "repository": null
        }])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let inbox = crate::api::notification::list_filtered(&c, false, 2, |n| {
        n.repository
            .as_ref()
            .is_none_or(|r| !r.full_name.eq_ignore_ascii_case("loud/repo"))
    })
    .await
    .expect("list");
    let ids: Vec<u64> = inbox.items.iter().map(|n| n.id).collect();
    assert_eq!(ids, [3, 5], "server order kept, repo-less thread passes");
    assert_eq!(inbox.hidden, 3);
    assert!(!inbox.truncated);
}

#[tokio::test]
async fn notification_list_filtered_stops_once_the_limit_is_met() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications"))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            notification_thread(1, true, "Issue", "o/r"),
            notification_thread(2, true, "Issue", "o/r"),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let inbox = crate::api::notification::list_filtered(&c, false, 2, |_| true)
        .await
        .expect("list");
    assert_eq!(inbox.items.len(), 2);
    assert_eq!(inbox.hidden, 0);
}

#[tokio::test]
async fn notification_unsubscribe_deletes_for_current_user_then_marks_read() {
    let server = MockServer::start().await;
    // First read sees the unread thread; the reconciling read after the
    // PATCH sees it read.
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, true, "Issue", "o/r")),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, false, "Issue", "o/r")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "login": "alice" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/5/subscriptions/alice"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(ResponseTemplate::new(205))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let report = crate::api::notification::unsubscribe_thread(&c, 7)
        .await
        .expect("unsubscribe");
    assert_eq!(
        report.outcome,
        crate::api::notification::Unsubscribed::Removed
    );
    assert_eq!(report.user, "alice");
    assert_eq!(report.target.index, 5);
    assert!(!report.thread.unread, "report carries the server's state");
}

#[tokio::test]
async fn notification_unsubscribe_skips_mark_read_for_a_read_thread() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, false, "Pull", "o/r")),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "login": "alice" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/5/subscriptions/alice"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(205))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let report = crate::api::notification::unsubscribe_thread(&c, 7)
        .await
        .expect("unsubscribe");
    assert_eq!(
        report.outcome,
        crate::api::notification::Unsubscribed::AlreadyUnsubscribed
    );
}

#[tokio::test]
async fn notification_unsubscribe_refuses_repository_threads_without_deleting() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(
                7,
                true,
                "Repository",
                "o/r",
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(205))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::notification::unsubscribe_thread(&c, 7)
        .await
        .expect_err("repository thread is not subscribable");
    assert!(
        format!("{err:#}").contains("only issue and pull request"),
        "{err:#}"
    );
}

#[tokio::test]
async fn notification_unsubscribe_refuses_when_current_user_is_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, true, "Issue", "o/r")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "scope" })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(205))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::notification::unsubscribe_thread(&c, 7)
        .await
        .expect_err("no current user, no delete");
    assert!(
        format!("{err:#}").contains("could not resolve the current user"),
        "{err:#}"
    );
}

#[tokio::test]
async fn notification_unsubscribe_reconciles_after_a_failed_delete() {
    let server = MockServer::start().await;
    // Read once before the delete and once after it fails.
    Mock::given(method("GET"))
        .and(path("/api/v1/notifications/threads/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(notification_thread(7, true, "Issue", "o/r")),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 1, "login": "alice" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/5/subscriptions/alice"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "no issue" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(205))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::notification::unsubscribe_thread(&c, 7)
        .await
        .expect_err("delete failed");
    let text = format!("{err:#}");
    assert!(text.contains("still unread on the server"), "{text}");
    assert!(text.contains("404"), "{text}");
}

#[tokio::test]
async fn notification_delete_subscription_explains_a_304() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/5/subscriptions/bob"))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let target = crate::api::notification::SubscriptionTarget {
        owner: "o".into(),
        repo: "r".into(),
        index: 5,
    };
    let err = crate::api::notification::delete_subscription(&c, &target, "bob")
        .await
        .expect_err("304 is a refusal");
    assert!(
        format!("{err:#}").contains("their own subscription"),
        "{err:#}"
    );
}

// --- fj admin: settings, cron, user creation, unadopted repositories ---

#[tokio::test]
async fn admin_settings_section_reports_server_values_or_unavailable() {
    use crate::api::admin::{settings_section, Section};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/settings/repository"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"lfs_disabled": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/settings/ui"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/settings/api"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "nope"})))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    // Only what the server sent: no client default for the other keys.
    assert_eq!(
        settings_section(&c, "/api/v1/settings/repository")
            .await
            .unwrap(),
        Section::Available(json!({"lfs_disabled": true}))
    );
    assert_eq!(
        settings_section(&c, "/api/v1/settings/ui").await.unwrap(),
        Section::Unavailable(404)
    );
    // A denial is an error, never "unavailable".
    let err = settings_section(&c, "/api/v1/settings/api")
        .await
        .expect_err("403 is not a missing endpoint");
    let api = err
        .chain()
        .find_map(|c| c.downcast_ref::<super::ApiError>())
        .expect("typed error");
    assert_eq!(api.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_cron_list_decodes_tasks_and_flags_a_missing_endpoint() {
    use crate::api::admin::cron_list;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/cron"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "name": "update_mirrors", "schedule": "@every 10m",
                "next": "2026-10-08T12:10:00Z", "prev": "2026-10-08T12:00:00Z",
                "exec_times": 42
            },
            {"name": "sparse"}
        ])))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let tasks = cron_list(&c, 100).await.unwrap().expect("endpoint exists");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].name, "update_mirrors");
    assert_eq!(tasks[0].exec_times, Some(42));
    // Absent fields stay absent instead of becoming zero values.
    assert_eq!(tasks[1].exec_times, None);
    assert!(tasks[1].schedule.is_none() && tasks[1].next.is_none());

    let missing = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/cron"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&missing)
        .await;
    let c = client_for(&missing).await;
    assert!(cron_list(&c, 100).await.unwrap().is_none());
}

#[tokio::test]
async fn admin_cron_list_forbidden_is_an_error_not_unavailable() {
    use crate::api::admin::cron_list;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/cron"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "admin only"})))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = cron_list(&c, 100).await.expect_err("403 surfaces");
    assert!(format!("{err:#}").contains("admin only"), "{err:#}");
}

fn new_user_body(password: Option<&str>) -> crate::api::admin::CreateUser<'_> {
    crate::api::admin::CreateUser {
        username: "alice",
        email: "alice@example.com",
        password,
        must_change_password: password.map(|_| true),
        send_notify: false,
        full_name: None,
        login_name: None,
        source_id: None,
        visibility: None,
        restricted: None,
    }
}

#[tokio::test]
async fn admin_create_user_posts_once_and_returns_the_account_without_credentials() {
    use crate::api::admin::create_user;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/users"))
        .and(body_json(json!({
            "username": "alice", "email": "alice@example.com",
            "password": "initial-pw-123", "must_change_password": true,
            "send_notify": false
        })))
        // A misbehaving server echoing the secret must not get it printed.
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "login": "alice", "email": "alice@example.com",
            "password": "initial-pw-123"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let created = create_user(&c, &new_user_body(Some("initial-pw-123")))
        .await
        .unwrap();
    assert_eq!(
        created,
        json!({"id": 7, "login": "alice", "email": "alice@example.com"})
    );
}

#[tokio::test]
async fn admin_create_user_failure_never_carries_the_password() {
    use crate::api::admin::create_user;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/users"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({"message": "user already exists"})),
        )
        // POST is never retried, so exactly one attempt.
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = create_user(&c, &new_user_body(Some("initial-pw-123")))
        .await
        .expect_err("422 surfaces");
    let shown = format!("{err:#} {err:?}");
    assert!(shown.contains("user already exists"), "{shown}");
    assert!(!shown.contains("initial-pw-123"), "{shown}");
}

#[tokio::test]
async fn admin_unadopted_list_sends_pattern_and_paging() {
    use crate::api::admin::{unadopted_list, UnadoptedQuery};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/unadopted"))
        .and(query_param("pattern", "al*"))
        .and(query_param("page", "2"))
        .and(query_param("limit", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(["alice/legacy"])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repos = unadopted_list(
        &c,
        &UnadoptedQuery {
            pattern: Some("al*"),
            page: Some(2),
            limit: Some(10),
        },
    )
    .await
    .unwrap();
    assert_eq!(repos, vec!["alice/legacy"]);
}

#[tokio::test]
async fn admin_unadopted_list_omits_an_unset_pattern() {
    use crate::api::admin::{unadopted_list, UnadoptedQuery};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/unadopted"))
        .and(query_param_is_missing("pattern"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repos = unadopted_list(&c, &UnadoptedQuery::default())
        .await
        .unwrap();
    assert!(repos.is_empty());
}

#[tokio::test]
async fn admin_unadopted_adopt_and_delete_hit_their_endpoints() {
    use crate::api::admin::{unadopted_adopt, unadopted_delete};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/unadopted/alice/legacy"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/admin/unadopted/alice/old"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    unadopted_adopt(&c, "alice", "legacy").await.unwrap();
    unadopted_delete(&c, "alice", "old").await.unwrap();
}

#[tokio::test]
async fn admin_unadopted_adopt_is_an_error_until_the_server_confirms() {
    use crate::api::admin::unadopted_adopt;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/unadopted/alice/ghost"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/admin/unadopted/alice/locked"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "admin only"})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    for (repo, status) in [
        ("ghost", StatusCode::NOT_FOUND),
        ("locked", StatusCode::FORBIDDEN),
    ] {
        let err = unadopted_adopt(&c, "alice", repo)
            .await
            .expect_err("not adopted");
        let api = err
            .chain()
            .find_map(|c| c.downcast_ref::<super::ApiError>())
            .expect("typed error");
        assert_eq!(api.status, status);
    }
}

// --- fj profile: settings, emails, avatar ---

#[tokio::test]
async fn user_settings_patch_sends_only_changed_fields() {
    use crate::api::user::{edit_settings, settings, Settings};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "Alice", "description": "", "hide_email": true,
            "theme": "forgejo-auto"
        })))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/user/settings"))
        .and(body_json(
            json!({"location": "Oslo", "hide_activity": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "Alice", "location": "Oslo", "hide_activity": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let current = settings(&c).await.unwrap();
    assert_eq!(current.full_name.as_deref(), Some("Alice"));
    assert_eq!(current.description.as_deref(), Some(""));
    assert_eq!(current.hide_email, Some(true));
    // Not sent by the server, so not known.
    assert_eq!(current.hide_activity, None);

    let body = Settings {
        location: Some("Oslo".into()),
        hide_activity: Some(false),
        ..Settings::default()
    };
    let confirmed = edit_settings(&c, &body).await.unwrap();
    assert_eq!(confirmed.location.as_deref(), Some("Oslo"));
    assert_eq!(confirmed.hide_activity, Some(false));
}

#[tokio::test]
async fn user_settings_policy_refusal_keeps_the_server_message() {
    use crate::api::user::{edit_settings, Settings};
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/user/settings"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"message": "changing full name is disabled"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = edit_settings(
        &c,
        &Settings {
            full_name: Some("X".into()),
            ..Settings::default()
        },
    )
    .await
    .expect_err("refused");
    let api = err
        .chain()
        .find_map(|c| c.downcast_ref::<super::ApiError>())
        .expect("typed error");
    assert_eq!(api.message, "changing full name is disabled");
}

#[tokio::test]
async fn user_emails_list_add_and_delete() {
    use crate::api::user::{add_emails, delete_emails, emails};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/emails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"email": "a@example.com", "verified": true, "primary": true},
            {"email": "b@example.com", "verified": false, "primary": false},
            {"email": "c@example.com"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/emails"))
        .and(body_json(json!({"emails": ["new@example.com"]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!([
            {"email": "new@example.com", "verified": false, "primary": false}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/emails"))
        .and(body_json(json!({"emails": ["b@example.com"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let listed = emails(&c).await.unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(
        (listed[0].verified, listed[0].primary),
        (Some(true), Some(true))
    );
    assert_eq!(listed[1].verified, Some(false));
    // The server said nothing, so nothing is assumed.
    assert_eq!((listed[2].verified, listed[2].primary), (None, None));

    let added = add_emails(&c, &["new@example.com".to_string()])
        .await
        .unwrap();
    assert_eq!(added[0].verified, Some(false));

    delete_emails(&c, &["b@example.com".to_string()])
        .await
        .unwrap();
}

#[tokio::test]
async fn user_email_add_rejection_keeps_the_server_message() {
    use crate::api::user::add_emails;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/emails"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "message": "Email address a@blocked.test is not allowed by the domain allowlist"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = add_emails(&c, &["a@blocked.test".to_string()])
        .await
        .expect_err("rejected");
    assert!(format!("{err:#}").contains("domain allowlist"), "{err:#}");
}

#[tokio::test]
async fn user_avatar_update_posts_base64_and_delete_removes_it() {
    use crate::api::user::{delete_avatar, update_avatar};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/avatar"))
        .and(body_json(json!({"image": "iVBORw0KGgo="})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/user/avatar"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    update_avatar(&c, "iVBORw0KGgo=").await.unwrap();
    delete_avatar(&c).await.unwrap();
}

// ---- issues: sort, milestone on create, reactions, dependencies, time ----

#[tokio::test]
async fn issue_list_sends_sort_only_when_asked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(query_param("sort", "oldest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            issue_json(1, "first", "open"),
            issue_json(9, "later", "open")
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(query_param_is_missing("sort"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([issue_json(9, "later", "open")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let sorted = crate::api::issue::list(
        &c,
        "o",
        "r",
        crate::api::issue::ListOptions {
            sort: Some("oldest"),
            ..Default::default()
        },
    )
    .await
    .expect("sorted list");
    // The order is the forge's: nothing re-sorts it on the way through.
    let numbers: Vec<u64> = sorted.items.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [1, 9]);

    let default = crate::api::issue::list(&c, "o", "r", Default::default())
        .await
        .expect("default list");
    assert_eq!(default.items.len(), 1);
}

#[tokio::test]
async fn issue_create_sends_milestone_in_the_create_request() {
    use crate::api::issue::CreateIssue;
    let server = MockServer::start().await;
    let mut created = issue_json(4, "new", "open");
    created["milestone"] = json!({ "id": 2, "title": "v2" });
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues"))
        .and(body_json(json!({ "title": "new", "milestone": 2 })))
        .respond_with(ResponseTemplate::new(201).set_body_json(created))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateIssue {
        title: "new",
        body: None,
        assignees: None,
        labels: None,
        milestone: Some(2),
    };
    let i = crate::api::issue::create(&c, "o", "r", &body)
        .await
        .expect("create");
    let confirmed = i.milestone.expect("the forge reports the milestone");
    assert_eq!((confirmed.id, confirmed.title.as_str()), (2, "v2"));
}

#[tokio::test]
async fn milestone_resolve_matches_a_title_then_an_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/milestones"))
        .and(query_param("state", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            milestone_json(1, "v1", "open"),
            milestone_json(2, "v2", "closed")
        ])))
        .mount(&server)
        .await;
    // An id that is not on the listed page is looked up directly.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/milestones/77"))
        .respond_with(ResponseTemplate::new(200).set_body_json(milestone_json(77, "old", "closed")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let by_title = crate::api::milestone::resolve(&c, "o", "r", "v2")
        .await
        .expect("by title");
    assert_eq!(by_title.id, 2);
    let by_id = crate::api::milestone::resolve(&c, "o", "r", "1")
        .await
        .expect("by id");
    assert_eq!(by_id.title, "v1");
    let direct = crate::api::milestone::resolve(&c, "o", "r", "77")
        .await
        .expect("by id off the page");
    assert_eq!(direct.title, "old");
    let err = crate::api::milestone::resolve(&c, "o", "r", "nope")
        .await
        .expect_err("unknown title");
    assert!(
        err.to_string().contains("no milestone named 'nope'"),
        "{err}"
    );
}

fn reaction_json(content: &str, login: &str) -> serde_json::Value {
    json!({
        "content": content,
        "user": { "id": 1, "login": login },
        "created_at": "2026-01-01T00:00:00Z"
    })
}

#[tokio::test]
async fn reactions_list_and_add_on_an_issue_and_on_a_comment() {
    use crate::api::issue_reaction::{self, Target};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/reactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            reaction_json("+1", "alice"),
            reaction_json("rocket", "bob")
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/comments/55/reactions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([reaction_json("eyes", "bob")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/reactions"))
        .and(body_json(json!({ "content": "heart" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(reaction_json("heart", "alice")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/comments/55/reactions"))
        .and(body_json(json!({ "content": "-1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(reaction_json("-1", "alice")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let on_issue = issue_reaction::list(&c, "o", "r", Target::Issue(3))
        .await
        .expect("issue reactions");
    assert_eq!(on_issue.len(), 2);
    assert_eq!(on_issue[1].user.as_ref().unwrap().login, "bob");
    let on_comment = issue_reaction::list(&c, "o", "r", Target::Comment(55))
        .await
        .expect("comment reactions");
    assert_eq!(on_comment[0].content, "eyes");
    let added = issue_reaction::add(&c, "o", "r", Target::Issue(3), "heart")
        .await
        .expect("add on issue");
    assert_eq!(added.content, "heart");
    let added = issue_reaction::add(&c, "o", "r", Target::Comment(55), "-1")
        .await
        .expect("add on comment");
    assert_eq!(added.content, "-1");
}

#[tokio::test]
async fn reaction_delete_carries_a_body_naming_the_reaction() {
    use crate::api::issue_reaction::{self, Target};
    let server = MockServer::start().await;
    // Only a DELETE whose body names the reaction matches; a bodiless DELETE
    // would fall through to wiremock's 404 and fail the calls below.
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/reactions"))
        .and(body_json(json!({ "content": "+1" })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/comments/55/reactions"))
        .and(body_json(json!({ "content": "rocket" })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    issue_reaction::remove(&c, "o", "r", Target::Issue(3), "+1")
        .await
        .expect("remove on issue");
    issue_reaction::remove(&c, "o", "r", Target::Comment(55), "rocket")
        .await
        .expect("remove on comment");
}

#[tokio::test]
async fn reaction_forbidden_surfaces_the_status() {
    use crate::api::issue_reaction::{self, Target};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/reactions"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "locked" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = issue_reaction::add(&c, "o", "r", Target::Issue(3), "heart")
        .await
        .expect_err("forbidden");
    assert_eq!(
        crate::api::issue_tracker::status_of(&err),
        Some(StatusCode::FORBIDDEN)
    );
}

#[tokio::test]
async fn dependencies_list_each_direction_from_its_own_endpoint() {
    use crate::api::issue_dependency::{self, Direction};
    let server = MockServer::start().await;
    let mut cross = issue_json(8, "upstream fix", "closed");
    cross["repository"] = json!({ "owner": "other", "name": "lib", "full_name": "other/lib" });
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/dependencies"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([issue_json(1, "groundwork", "open"), cross])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/blocks"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([issue_json(5, "release", "open")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let depends_on = issue_dependency::list(&c, "o", "r", 3, Direction::DependsOn, None)
        .await
        .expect("depends on");
    assert_eq!(
        depends_on.iter().map(|d| d.number).collect::<Vec<_>>(),
        [1, 8]
    );
    let repo = depends_on[1].repository.as_ref().expect("repository");
    assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("other", "lib"));
    let blocks = issue_dependency::list(&c, "o", "r", 3, Direction::Blocks, None)
        .await
        .expect("blocks");
    assert_eq!(blocks.iter().map(|d| d.number).collect::<Vec<_>>(), [5]);
}

#[tokio::test]
async fn dependency_add_and_remove_keep_the_direction_and_name_the_other_item() {
    use crate::api::issue_dependency::{self, Direction, IssueMeta};
    let server = MockServer::start().await;
    let other = json!({ "owner": "o", "repo": "r", "index": 7 });
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/dependencies"))
        .and(body_json(other.clone()))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/blocks"))
        .and(body_json(other.clone()))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    // The DELETE carries the same body; without it nothing names the link.
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/dependencies"))
        .and(body_json(other.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/blocks"))
        .and(body_json(other))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let meta = IssueMeta {
        owner: "o".into(),
        repo: "r".into(),
        index: 7,
    };
    for direction in [Direction::DependsOn, Direction::Blocks] {
        issue_dependency::add(&c, "o", "r", 3, direction, &meta)
            .await
            .expect("add");
        issue_dependency::remove(&c, "o", "r", 3, direction, &meta)
            .await
            .expect("remove");
    }
}

#[tokio::test]
async fn issue_list_404_names_an_external_tracker() {
    use crate::api::issue_tracker;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "has_issues": true,
            "external_tracker": { "external_tracker_url": "https://t.example/o/r" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::issue::list(&c, "o", "r", Default::default())
        .await
        .err()
        .expect("404");
    let err = issue_tracker::explain_not_found(&c, "o", "r", err).await;
    assert_eq!(
        err.to_string(),
        "o/r keeps its issues in an external tracker: https://t.example/o/r"
    );
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_NOT_FOUND);
}

#[tokio::test]
async fn issue_list_404_on_a_missing_repo_is_left_alone() {
    use crate::api::issue_tracker;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "not found"})))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::issue::list(&c, "o", "r", Default::default())
        .await
        .err()
        .expect("404");
    let err = issue_tracker::explain_not_found(&c, "o", "r", err).await;
    assert_eq!(err.to_string(), "not found: o/r");
}

#[tokio::test]
async fn issue_tracker_reads_repo_switches_and_probes_a_work_item() {
    use crate::api::issue_tracker;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "r", "archived": true,
            "permissions": { "admin": false, "push": false, "pull": true },
            "internal_tracker": {
                "enable_time_tracker": true,
                "allow_only_contributors_to_track_time": true,
                "enable_issue_dependencies": false
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/404"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "not found" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/403"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let tracker = issue_tracker::get(&c, "o", "r").await.expect("repo facts");
    assert!(tracker.archived);
    assert_eq!(tracker.dependencies_enabled(), Some(false));
    assert_eq!(tracker.time_tracking_enabled(), Some(true));
    assert!(tracker.time_limited_to_contributors());
    assert!(issue_tracker::work_item_exists(&c, "o", "r", 3)
        .await
        .expect("exists"));
    assert!(!issue_tracker::work_item_exists(&c, "o", "r", 404)
        .await
        .expect("missing"));
    // Anything but a 404 is not an answer to "does it exist".
    assert!(issue_tracker::work_item_exists(&c, "o", "r", 403)
        .await
        .is_err());
}

fn tracked_time_json(id: u64, user: &str, seconds: u64) -> serde_json::Value {
    json!({
        "id": id, "time": seconds, "user_name": user, "user_id": 1,
        "created": "2026-01-01T00:00:00Z",
        "issue": issue_json(3, "a bug", "open")
    })
}

#[tokio::test]
async fn tracked_time_list_add_and_delete() {
    use crate::api::issue_time;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/times"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            tracked_time_json(1, "alice", 3600),
            tracked_time_json(2, "bob", 1800)
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/times"))
        .and(body_json(json!({ "time": 5400 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(tracked_time_json(3, "alice", 5400)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/times/2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let entries = issue_time::list(&c, "o", "r", 3).await.expect("list");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].user_name, "bob");
    assert_eq!(entries.iter().map(|e| e.time).sum::<u64>(), 5400);
    let added = issue_time::add(&c, "o", "r", 3, 5400).await.expect("add");
    assert_eq!((added.id, added.time), (3, 5400));
    issue_time::delete(&c, "o", "r", 3, 2)
        .await
        .expect("delete");
}

#[tokio::test]
async fn tracked_time_failures_keep_their_status() {
    use crate::api::issue_time;
    use crate::api::issue_tracker::status_of;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/times"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "message": "time tracking disabled" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/times/2"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "forbidden" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = issue_time::add(&c, "o", "r", 3, 60)
        .await
        .expect_err("disabled");
    assert_eq!(status_of(&err), Some(StatusCode::BAD_REQUEST));
    let err = issue_time::delete(&c, "o", "r", 3, 2)
        .await
        .expect_err("denied");
    assert_eq!(status_of(&err), Some(StatusCode::FORBIDDEN));
}

// --- Organizations, teams, collaborators, transfers, single topics ---

#[tokio::test]
async fn org_create_posts_fields_and_decodes_the_org() {
    use crate::api::org::CreateOrg;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs"))
        .and(body_json(json!({
            "username": "acme",
            "full_name": "Acme Inc",
            "visibility": "private"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 7, "username": "acme", "full_name": "Acme Inc",
            "description": "", "visibility": "private", "avatar_url": ""
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let body = CreateOrg {
        username: "acme",
        full_name: Some("Acme Inc"),
        visibility: Some("private"),
        ..Default::default()
    };
    let org = crate::api::org::create(&c, &body).await.expect("create");
    assert_eq!(org.username, "acme");
    assert_eq!(org.visibility, "private");
}

#[tokio::test]
async fn org_create_tells_permission_denial_from_name_conflict() {
    use crate::api::org::CreateOrg;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs"))
        .and(body_json(json!({ "username": "denied" })))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "message": "organization creation is disabled" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs"))
        .and(body_json(json!({ "username": "taken" })))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_json(json!({ "message": "user already exists [name: taken]" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let denied = CreateOrg {
        username: "denied",
        ..Default::default()
    };
    let err = crate::api::org::create(&c, &denied).await.expect_err("403");
    let msg = format!("{err:#}");
    assert!(msg.starts_with("permission denied"), "{msg}");
    assert!(!msg.contains("name conflict"), "{msg}");

    let taken = CreateOrg {
        username: "taken",
        ..Default::default()
    };
    let err = crate::api::org::create(&c, &taken).await.expect_err("422");
    let msg = format!("{err:#}");
    assert!(
        msg.starts_with("name conflict: 'taken' is already taken"),
        "{msg}"
    );
    assert!(!msg.contains("permission denied"), "{msg}");
}

#[tokio::test]
async fn org_team_create_posts_units_and_decodes_the_team() {
    use crate::api::org::{resolve_team_units, CreateTeam, TeamPermission};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs/acme/teams"))
        .and(body_json(json!({
            "name": "docs",
            "permission": "write",
            "includes_all_repositories": false,
            "can_create_org_repo": false,
            "units": ["repo.code", "repo.wiki"]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 12, "name": "docs", "description": "", "permission": "write",
            "includes_all_repositories": false, "can_create_org_repo": false,
            "units": ["repo.code", "repo.wiki"],
            "units_map": { "repo.code": "write", "repo.wiki": "write" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let (units, units_map) = resolve_team_units(
        TeamPermission::Write,
        &["repo.code".to_string(), "repo.wiki".to_string()],
        &[],
    )
    .unwrap();
    let body = CreateTeam {
        name: "docs".into(),
        description: None,
        permission: TeamPermission::Write.as_api_value(),
        includes_all_repositories: false,
        can_create_org_repo: false,
        units,
        units_map,
    };
    let team = crate::api::org::create_team(&c, "acme", &body)
        .await
        .expect("create team");
    assert_eq!(team.id, 12);
    assert_eq!(team.permission, "write");
    assert_eq!(
        team.units_map.get("repo.wiki").map(String::as_str),
        Some("write")
    );
}

#[tokio::test]
async fn org_find_team_matches_name_then_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/acme/teams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "Owners", "permission": "owner", "units": null, "units_map": null },
            { "id": 12, "name": "docs", "permission": "write" }
        ])))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let by_name = crate::api::org::find_team(&c, "acme", "DOCS")
        .await
        .expect("by name");
    assert_eq!(by_name.id, 12);
    let by_id = crate::api::org::find_team(&c, "acme", "1")
        .await
        .expect("by id");
    assert_eq!(by_id.name, "Owners");
    let err = crate::api::org::find_team(&c, "acme", "ghosts")
        .await
        .expect_err("missing");
    assert!(
        err.to_string().contains("no team 'ghosts' in org acme"),
        "{err}"
    );
}

#[tokio::test]
async fn org_team_members_list_add_remove_and_membership() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "login": "alice" },
            { "id": 2, "login": "bob" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams/12/members/carol"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/teams/12/members/bob"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/members/carol"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 3, "login": "carol" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/members/bob"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let members = crate::api::org::list_team_members(&c, 12, 30)
        .await
        .expect("members");
    let logins: Vec<&str> = members.items.iter().map(|u| u.login.as_str()).collect();
    assert_eq!(logins, vec!["alice", "bob"]);

    crate::api::org::add_team_member(&c, 12, "carol")
        .await
        .expect("add");
    crate::api::org::remove_team_member(&c, 12, "bob")
        .await
        .expect("remove");
    assert!(crate::api::org::is_team_member(&c, 12, "carol")
        .await
        .expect("carol"));
    assert!(!crate::api::org::is_team_member(&c, 12, "bob")
        .await
        .expect("bob"));
}

#[tokio::test]
async fn org_team_member_change_surfaces_permission_denial() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams/12/members/carol"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "message": "Must be an organization owner" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::org::add_team_member(&c, 12, "carol")
        .await
        .expect_err("403");
    let err = crate::api::org::with_forbidden_context(err, "permission denied: test".into());
    let msg = format!("{err:#}");
    assert!(msg.starts_with("permission denied: test"), "{msg}");
    assert!(msg.contains("Must be an organization owner"), "{msg}");
}

#[tokio::test]
async fn org_team_repos_list_add_remove_and_assignment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "full_name": "acme/web", "private": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams/12/repos/acme/api"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/teams/12/repos/acme/web"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/repos/acme/api"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": 2, "full_name": "acme/api" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/12/repos/acme/web"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let repos = crate::api::org::list_team_repos(&c, 12, 30)
        .await
        .expect("repos");
    assert_eq!(repos.items.len(), 1);
    assert_eq!(repos.items[0].full_name, "acme/web");
    assert!(repos.items[0].private);

    crate::api::org::add_team_repo(&c, 12, "acme", "api")
        .await
        .expect("add");
    crate::api::org::remove_team_repo(&c, 12, "acme", "web")
        .await
        .expect("remove");
    assert!(crate::api::org::team_has_repo(&c, 12, "acme", "api")
        .await
        .expect("api"));
    assert!(!crate::api::org::team_has_repo(&c, 12, "acme", "web")
        .await
        .expect("web"));
}

#[tokio::test]
async fn org_user_permissions_decodes_flags() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/alice/orgs/acme/permissions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "is_owner": false, "is_admin": false, "can_write": true,
            "can_read": true, "can_create_repository": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let p = crate::api::org::user_permissions(&c, "alice", "acme")
        .await
        .expect("permissions");
    assert!(!p.is_owner);
    assert!(p.can_create_repository);
}

#[tokio::test]
async fn repo_collaborator_list_resolves_permission_per_row() {
    // fj#276: the list sends `permissions: null`; the real level comes from
    // one `.../permission` lookup per collaborator.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "login": "alice", "permissions": null },
            { "id": 2, "login": "bob", "permissions": null },
            { "id": 3, "login": "carol", "permissions": null }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/alice/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permission": "admin", "role_name": "admin",
            "user": { "id": 1, "login": "alice" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/bob/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permission": "read", "role_name": "read",
            "user": { "id": 2, "login": "bob" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/carol/permission"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "message": "Only admins can query all permissions" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let mut rows = crate::api::repo::list_collaborators_page(&c, "o", "r", Some(30))
        .await
        .expect("list")
        .items;
    // Before the lookups nothing is known, and nothing claims "none".
    assert!(rows.iter().all(|r| r.permission_level() == "unknown"));

    let undisclosed = crate::api::repo::resolve_collaborator_permissions(&c, "o", "r", &mut rows)
        .await
        .expect("resolve");
    assert_eq!(undisclosed, 1);
    let levels: Vec<&str> = rows.iter().map(|r| r.permission_level()).collect();
    assert_eq!(levels, vec!["admin", "read", "unknown"]);
}

#[tokio::test]
async fn repo_collaborator_permission_lookup_failure_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/alice/permission"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "gone" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let mut rows: Vec<crate::api::repo::Collaborator> =
        serde_json::from_value(json!([{ "id": 1, "login": "alice" }])).unwrap();
    assert!(
        crate::api::repo::resolve_collaborator_permissions(&c, "o", "r", &mut rows)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn repo_collaborator_membership_and_confirmed_permission() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/alice"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/ghost"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/collaborators/alice/permission"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permission": "write", "role_name": "write",
            "user": { "id": 1, "login": "alice" }
        })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    assert!(crate::api::repo::is_collaborator(&c, "o", "r", "alice")
        .await
        .expect("alice"));
    assert!(!crate::api::repo::is_collaborator(&c, "o", "r", "ghost")
        .await
        .expect("ghost"));
    let access = crate::api::repo::collaborator_permission(&c, "o", "r", "alice")
        .await
        .expect("permission");
    assert_eq!(access.permission, "write");
    assert_eq!(access.user.expect("user").login, "alice");
}

#[tokio::test]
async fn repo_collaborator_remove_forbidden_names_the_access_needed() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/collaborators/alice"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "nope" })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::repo::remove_collaborator(&c, "o", "r", "alice")
        .await
        .expect_err("403");
    let msg = err.to_string();
    assert!(msg.starts_with("permission denied"), "{msg}");
    assert!(msg.contains("admin access"), "{msg}");
}

#[tokio::test]
async fn repo_transfer_request_reports_pending_from_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/bob/web/transfer"))
        .and(body_json(json!({ "new_owner": "acme", "team_ids": [3] })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "name": "web", "full_name": "bob/web",
            "owner": { "id": 1, "login": "bob" },
            "repo_transfer": {
                "doer": { "id": 1, "login": "bob" },
                "recipient": { "id": 9, "login": "acme" },
                "teams": [{ "id": 3, "name": "docs" }]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let after = crate::api::repo::request_transfer(&c, "bob", "web", "acme", &[3])
        .await
        .expect("transfer");
    assert_eq!(after.owner.login, "bob");
    let pending = after.repo_transfer.expect("pending");
    assert_eq!(pending.recipient.expect("recipient").login, "acme");
    assert_eq!(pending.teams[0].name, "docs");
}

#[tokio::test]
async fn repo_transfer_request_reports_moved_from_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/bob/web/transfer"))
        .and(body_json(json!({ "new_owner": "acme" })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "name": "web", "full_name": "acme/web",
            "owner": { "id": 9, "login": "acme" },
            "repo_transfer": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let after = crate::api::repo::request_transfer(&c, "bob", "web", "acme", &[])
        .await
        .expect("transfer");
    assert_eq!(after.full_name, "acme/web");
    assert!(after.repo_transfer.is_none());
}

#[tokio::test]
async fn repo_transfer_request_forbidden_is_a_typed_denial() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/bob/web/transfer"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "forbidden" })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::repo::request_transfer(&c, "bob", "web", "acme", &[])
        .await
        .expect_err("403");
    let api = err
        .downcast_ref::<crate::client::ApiError>()
        .expect("typed error");
    assert_eq!(api.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn repo_transfer_accept_and_reject_decode_the_repository() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/bob/web/transfer/accept"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "name": "web", "full_name": "acme/web",
            "owner": { "id": 9, "login": "acme" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/bob/lib/transfer/reject"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "lib", "full_name": "bob/lib",
            "owner": { "id": 1, "login": "bob" },
            "repo_transfer": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let accepted = crate::api::repo::accept_transfer(&c, "bob", "web")
        .await
        .expect("accept");
    assert_eq!(accepted.owner.login, "acme");
    let rejected = crate::api::repo::reject_transfer(&c, "bob", "lib")
        .await
        .expect("reject");
    assert_eq!(rejected.owner.login, "bob");
    assert!(rejected.repo_transfer.is_none());
}

#[tokio::test]
async fn repo_transfer_state_reads_the_pending_transfer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/bob/web"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "web", "full_name": "bob/web",
            "owner": { "id": 1, "login": "bob" },
            "repo_transfer": {
                "doer": { "id": 1, "login": "bob" },
                "recipient": { "id": 2, "login": "carol" },
                "teams": null
            }
        })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let repo = crate::api::repo::transfer_state(&c, "bob", "web")
        .await
        .expect("state");
    let t = repo.repo_transfer.expect("pending");
    assert_eq!(t.doer.expect("doer").login, "bob");
    assert_eq!(t.recipient.expect("recipient").login, "carol");
}

#[tokio::test]
async fn repo_pending_transfers_keeps_only_marked_repositories() {
    let server = MockServer::start().await;
    let listing = json!([
        { "name": "web", "full_name": "bob/web", "owner": { "id": 1, "login": "bob" },
          "repo_transfer": {
              "doer": { "id": 1, "login": "bob" },
              "recipient": { "id": 2, "login": "carol" },
              "teams": []
          } },
        { "name": "lib", "full_name": "bob/lib", "owner": { "id": 1, "login": "bob" },
          "repo_transfer": null },
        { "name": "api", "full_name": "bob/api", "owner": { "id": 1, "login": "bob" } }
    ]);
    Mock::given(method("GET"))
        .and(path("/api/v1/user/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listing.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/bob/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listing))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let mine = crate::api::repo::list_pending_transfers(&c, None)
        .await
        .expect("mine");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].full_name, "bob/web");
    let theirs = crate::api::repo::list_pending_transfers(&c, Some("bob"))
        .await
        .expect("theirs");
    assert_eq!(theirs.len(), 1);
}

#[tokio::test]
async fn repo_lookup_owner_tells_org_from_user_from_nobody() {
    use crate::api::repo::OwnerKind;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/Acme"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 9, "name": "acme", "username": "acme", "full_name": "Acme Inc"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/carol"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/carol"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 2, "login": "carol", "full_name": "Carol Example"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/ghost"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/ghost"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let org = crate::api::repo::lookup_owner(&c, "Acme")
        .await
        .expect("org")
        .expect("found");
    assert_eq!(org.kind, OwnerKind::Organization);
    // The forge's spelling wins over what was typed.
    assert_eq!(org.login, "acme");
    assert_eq!(org.full_name, "Acme Inc");

    let user = crate::api::repo::lookup_owner(&c, "carol")
        .await
        .expect("user")
        .expect("found");
    assert_eq!(user.kind, OwnerKind::User);
    assert_eq!(user.login, "carol");

    assert!(crate::api::repo::lookup_owner(&c, "ghost")
        .await
        .expect("ghost")
        .is_none());
}

#[tokio::test]
async fn repo_topic_add_and_remove_hit_the_single_topic_route() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/topics/rust"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/topics/cli"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::repo::add_topic(&c, "o", "r", "rust")
        .await
        .expect("add");
    crate::api::repo::remove_topic(&c, "o", "r", "cli")
        .await
        .expect("remove");
}

#[tokio::test]
async fn repo_topic_change_surfaces_the_forge_message() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/topics/bad"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "message": "Topic name is invalid", "invalidTopics": ["bad"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/o/r/topics"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({ "message": "write access needed" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::repo::add_topic(&c, "o", "r", "bad")
        .await
        .expect_err("422");
    assert!(
        format!("{err:#}").contains("Topic name is invalid"),
        "{err:#}"
    );
    let err = crate::api::repo::set_topics(&c, "o", "r", &["x".into()])
        .await
        .expect_err("403");
    assert!(
        format!("{err:#}").contains("write access needed"),
        "{err:#}"
    );
}

// ---- pull request line comments and merge preflight reads ----

fn review_json(id: u64, state: &str, comments_count: u64) -> serde_json::Value {
    json!({
        "id": id, "state": state, "body": "",
        "user": { "id": id, "login": format!("u{id}") },
        "submitted_at": "2026-05-30T00:00:00Z",
        "html_url": format!("https://x/o/r/pulls/12#review-{id}"),
        "official": true, "dismissed": false, "stale": false,
        "comments_count": comments_count
    })
}

#[tokio::test]
async fn pull_review_batches_line_comments_with_the_verdict_in_one_request() {
    use crate::api::pull::{DiffSide, LineComment, ReviewEvent};
    let server = MockServer::start().await;
    // One POST carries the verdict, the body and both comments. An old-side
    // anchor sets only `old_position`, a new-side one only `new_position`,
    // both as file line numbers; no range field is sent.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(body_json(json!({
            "event": "REQUEST_CHANGES",
            "body": "see inline",
            "comments": [
                { "path": "src/a:b.rs", "body": "why removed?", "old_position": 11, "new_position": 0 },
                { "path": "src/c.rs", "body": "rename this", "old_position": 0, "new_position": 42 }
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(review_json(9, "REQUEST_CHANGES", 2)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let comments = [
        LineComment {
            path: "src/a:b.rs".into(),
            side: DiffSide::Old,
            line: 11,
            body: "why removed?".into(),
        },
        LineComment {
            path: "src/c.rs".into(),
            side: DiffSide::New,
            line: 42,
            body: "rename this".into(),
        },
    ];
    let review = crate::api::pull::submit_review_with_comments(
        &c,
        "o",
        "r",
        12,
        ReviewEvent::RequestChanges,
        Some("see inline"),
        &comments,
    )
    .await
    .expect("review");
    assert_eq!(review.state, "REQUEST_CHANGES");
    assert_eq!(review.comments_count, 2);
}

#[tokio::test]
async fn pull_review_posts_a_single_line_comment_without_a_verdict() {
    use crate::api::pull::{DiffSide, LineComment, ReviewEvent};
    let server = MockServer::start().await;
    // A lone comment is a COMMENT review with no body: no verdict travels.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(body_json(json!({
            "event": "COMMENT",
            "comments": [
                { "path": "README.md", "body": "typo", "old_position": 0, "new_position": 3 }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(review_json(10, "COMMENT", 1)))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let review = crate::api::pull::submit_review_with_comments(
        &c,
        "o",
        "r",
        12,
        ReviewEvent::Comment,
        None,
        &[LineComment {
            path: "README.md".into(),
            side: DiffSide::New,
            line: 3,
            body: "typo".into(),
        }],
    )
    .await
    .expect("review");
    assert_eq!(review.state, "COMMENT");
    assert_eq!(review.comments_count, 1);
}

#[tokio::test]
async fn pull_review_with_line_comments_is_not_retried_and_surfaces_422() {
    use crate::api::pull::{DiffSide, LineComment, ReviewEvent};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({ "message": "invalid position" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::submit_review_with_comments(
        &c,
        "o",
        "r",
        12,
        ReviewEvent::Comment,
        None,
        &[LineComment {
            path: "a".into(),
            side: DiffSide::New,
            line: 1,
            body: "x".into(),
        }],
    )
    .await
    .expect_err("422");
    let api = err.downcast_ref::<super::ApiError>().expect("api error");
    assert_eq!(api.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn pull_list_reviews_all_walks_pages_until_an_empty_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            review_json(1, "APPROVED", 0),
            review_json(2, "COMMENT", 0)
        ])))
        .expect(1)
        .mount(&server)
        .await;
    // A short first page is not trusted as the end: page 2 is still read.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([review_json(3, "APPROVED", 0)])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let scan = crate::api::pull::list_reviews_all(&c, "o", "r", 12)
        .await
        .expect("scan");
    assert!(scan.complete);
    assert_eq!(scan.reviews.len(), 3);
    assert!(scan.reviews[0].official);
}

#[tokio::test]
async fn pull_list_reviews_all_is_incomplete_when_the_server_ignores_paging() {
    let server = MockServer::start().await;
    // Every page answers with the same review: the walk must stop and must
    // not claim it saw everything.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([review_json(1, "APPROVED", 0)])),
        )
        .expect(2)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let scan = crate::api::pull::list_reviews_all(&c, "o", "r", 12)
        .await
        .expect("scan");
    assert!(!scan.complete);
    assert_eq!(scan.reviews.len(), 1);
}

#[tokio::test]
async fn pull_list_reviews_all_propagates_a_permission_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/12/reviews"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::pull::list_reviews_all(&c, "o", "r", 12)
        .await
        .expect_err("403");
    let api = err.downcast_ref::<super::ApiError>().expect("api error");
    assert_eq!(api.status, StatusCode::FORBIDDEN);
}

fn status_page(contexts: &[(&str, &str)]) -> serde_json::Value {
    json!({
        "state": "pending",
        "sha": "abc",
        "total_count": contexts.len(),
        "statuses": contexts.iter().map(|(c, s)| json!({
            "status": s, "context": c, "description": "", "target_url": ""
        })).collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn pull_combined_status_all_folds_every_page_of_statuses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc/status"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "50"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(status_page(&[("a", "success"), ("b", "pending")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc/status"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(status_page(&[("required-ci", "success")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Forgejo answers a page past the end with an empty object.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc/status"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let scan = crate::api::pull::combined_status_all(&c, "o", "r", "abc")
        .await
        .expect("scan");
    assert!(scan.complete);
    assert_eq!(scan.combined.state, "pending");
    assert_eq!(scan.combined.total_count, 3);
    let contexts: Vec<&str> = scan
        .combined
        .statuses
        .iter()
        .map(|s| s.context.as_str())
        .collect();
    assert_eq!(contexts, ["a", "b", "required-ci"]);
}

#[tokio::test]
async fn pull_combined_status_all_with_no_ci_is_a_complete_empty_scan() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let scan = crate::api::pull::combined_status_all(&c, "o", "r", "abc")
        .await
        .expect("scan");
    assert!(scan.complete);
    assert!(scan.combined.statuses.is_empty());
}

#[tokio::test]
async fn pull_combined_status_all_is_incomplete_when_paging_is_ignored() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_page(&[("a", "success")])))
        .expect(2)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let scan = crate::api::pull::combined_status_all(&c, "o", "r", "abc")
        .await
        .expect("scan");
    assert!(!scan.complete);
    assert_eq!(scan.combined.statuses.len(), 1);
}

#[tokio::test]
async fn protect_effective_reads_the_rule_a_non_admin_can_see() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/release%2F1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "release/1.0",
            "protected": true,
            "effective_branch_protection_name": "release/*",
            "required_approvals": 2,
            "enable_status_check": true,
            "status_check_contexts": ["CI / *"],
            "user_can_merge": false,
            "user_can_push": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let p = crate::api::protect::effective(&c, "o", "r", "release/1.0")
        .await
        .expect("effective");
    assert_eq!(p.protected, Some(true));
    assert_eq!(
        p.effective_branch_protection_name.as_deref(),
        Some("release/*")
    );
    assert_eq!(p.required_approvals, Some(2));
    assert_eq!(p.enable_status_check, Some(true));
    assert_eq!(p.status_check_contexts, Some(vec!["CI / *".to_string()]));
    assert_eq!(p.user_can_merge, Some(false));
}

#[tokio::test]
async fn protect_effective_keeps_omitted_and_null_fields_unknown() {
    let server = MockServer::start().await;
    // A server that reports the branch but none of the rule, and a null list.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "main",
            "status_check_contexts": null
        })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let p = crate::api::protect::effective(&c, "o", "r", "main")
        .await
        .expect("effective");
    assert_eq!(p.protected, None);
    assert_eq!(p.required_approvals, None);
    assert_eq!(p.status_check_contexts, None);
    assert_eq!(p.user_can_merge, None);
}

#[tokio::test]
async fn protect_effective_tells_missing_from_forbidden() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/gone"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "nf" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/secret"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = crate::api::protect::effective(&c, "o", "r", "gone")
        .await
        .expect_err("404");
    assert!(
        format!("{err:#}").contains("branch 'gone' not found"),
        "{err:#}"
    );
    let err = crate::api::protect::effective(&c, "o", "r", "secret")
        .await
        .expect_err("403");
    let api = err
        .chain()
        .find_map(|c| c.downcast_ref::<super::ApiError>())
        .expect("api error");
    assert_eq!(api.status, StatusCode::FORBIDDEN);
}

/// A non-admin's view: the branch endpoint answers, the admin-only rule
/// endpoint does not. Returns the preflight and the post-refusal diagnosis.
async fn run_preflight(
    server: &MockServer,
    number: u64,
) -> (crate::cli::pr_preflight::Preflight, Option<String>) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branch_protections/main"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    let c = client_for(server).await;
    let pr = crate::api::pull::get(&c, "o", "r", number)
        .await
        .expect("get pull");
    let preflight = crate::cli::pr_preflight::gather(&c, "o", "r", &pr, None).await;
    let reason = crate::cli::pr_merge_check::diagnose_merge_failure(
        &c,
        "o",
        "r",
        &pr,
        "merge",
        None,
        Some(&preflight),
    )
    .await;
    (preflight, reason)
}

async fn mount_pull_and_rule(server: &MockServer, rule: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(7, true, false, "open")),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(rule))
        .mount(server)
        .await;
}

#[tokio::test]
async fn merge_preflight_blocks_on_a_required_check_found_on_a_later_page() {
    use crate::cli::pr_preflight::{CheckOutcome, Gate};
    let server = MockServer::start().await;
    mount_pull_and_rule(
        &server,
        json!({
            "name": "main", "protected": true,
            "effective_branch_protection_name": "main",
            "required_approvals": 0, "enable_status_check": true,
            "status_check_contexts": ["ci", "never-reports"],
            "user_can_merge": true, "user_can_push": true
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(status_page(&[("optional", "failure")])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_page(&[("ci", "success")])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .and(query_param("page", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let (p, reason) = run_preflight(&server, 7).await;
    // `ci` was on page 2 and is green; the optional failure does not block;
    // the required check that never reported does.
    assert_eq!(p.is_required("optional"), Some(false));
    assert_eq!(p.required_checks[0].outcome, CheckOutcome::Success);
    assert_eq!(p.required_checks[1].outcome, CheckOutcome::Missing);
    assert!(matches!(p.checks, Gate::Blocked(_)));
    assert_eq!(p.approvals, Gate::NotRequired);
    let pre = p.pre_merge_blocker(false).expect("blocked before merging");
    assert!(pre.contains("\"never-reports\" has not reported"), "{pre}");
    // The same explanation is available after a refusal, without admin rights.
    assert!(reason.expect("diagnosed").contains("never-reports"));
}

#[tokio::test]
async fn merge_preflight_leaves_unreadable_evidence_unknown() {
    use crate::cli::pr_preflight::Gate;
    let server = MockServer::start().await;
    mount_pull_and_rule(
        &server,
        json!({
            "name": "main", "protected": true,
            "effective_branch_protection_name": "main",
            "required_approvals": 1, "enable_status_check": true,
            "status_check_contexts": ["ci"],
            "user_can_merge": true, "user_can_push": true
        }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits/abc1234/status"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    let (p, reason) = run_preflight(&server, 7).await;
    assert!(matches!(&p.checks, Gate::Unknown(w) if w.contains("permission denied")));
    assert!(matches!(&p.approvals, Gate::Unknown(w) if w.contains("permission denied")));
    // Unknown is neither a pass nor a block: the server gets to decide.
    assert!(p.pre_merge_blocker(false).is_none());
    assert_eq!(p.to_json()["verdict"], "unknown");
    assert!(reason.is_none());
    assert!(p.refusal_note().expect("note").contains("could not check"));
}

#[tokio::test]
async fn merge_preflight_with_an_unreadable_rule_is_unknown_and_reads_nothing_else() {
    use crate::cli::pr_preflight::Protection;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(merge_diag_pull_json(7, true, false, "open")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/branches/main"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&server)
        .await;
    // No status or review mocks: reading them would 404 and is not attempted.
    let (p, reason) = run_preflight(&server, 7).await;
    assert!(matches!(&p.protection, Protection::Unknown(w) if w.contains("permission denied")));
    assert_eq!(p.is_required("ci"), None);
    assert!(p.pre_merge_blocker(false).is_none());
    assert!(reason.is_none());
}

#[tokio::test]
async fn merge_preflight_counts_only_official_approvals() {
    use crate::cli::pr_preflight::Gate;
    let server = MockServer::start().await;
    mount_pull_and_rule(
        &server,
        json!({
            "name": "main", "protected": true,
            "effective_branch_protection_name": "main",
            "required_approvals": 2, "enable_status_check": false,
            "status_check_contexts": [],
            "user_can_merge": true, "user_can_push": true
        }),
    )
    .await;
    let mut unofficial = review_json(2, "APPROVED", 0);
    unofficial["official"] = json!(false);
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([review_json(1, "APPROVED", 0), unofficial])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let (p, reason) = run_preflight(&server, 7).await;
    assert_eq!(p.checks, Gate::NotRequired);
    assert_eq!(
        p.approvals,
        Gate::Blocked("PR #7 needs 2 approving review(s) but has 1.".into())
    );
    assert_eq!(
        reason.as_deref(),
        Some("PR #7 needs 2 approving review(s) but has 1.")
    );
}

// ---- repo commits: signature verification, refs ----

#[tokio::test]
async fn commit_list_starts_at_ref_and_keeps_verification_on() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/commits"))
        .and(query_param("sha", "abc123"))
        .and(query_param("stat", "false"))
        .and(query_param("files", "false"))
        .and(query_param("limit", "3"))
        // Never opt out: the server default (true) is what carries the verdict.
        .and(query_param_is_missing("verification"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "sha": "1111111111111111111111111111111111111111",
                "commit": {
                    "message": "signed work",
                    "author": { "name": "Ada", "email": "ada@example.com", "date": "2026-01-01T00:00:00Z" },
                    "verification": {
                        "verified": true,
                        "reason": "ada / ABCDEF",
                        "signature": "-----BEGIN PGP SIGNATURE-----",
                        "signer": { "name": "Ada", "email": "ada@example.com", "username": "ada" }
                    }
                }
            },
            {
                "sha": "2222222222222222222222222222222222222222",
                "commit": {
                    "message": "plain work",
                    "verification": { "verified": false, "reason": "gpg.error.not_signed_commit", "signature": "", "signer": null }
                }
            },
            {
                "sha": "3333333333333333333333333333333333333333",
                "commit": { "message": "old forge" }
            }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let commits = crate::api::commit::list(
        &c,
        "o",
        "r",
        &crate::api::commit::ListOptions {
            start: Some("abc123"),
            path: None,
            limit: 3,
        },
    )
    .await
    .expect("commit list");
    assert_eq!(commits.len(), 3);
    let v = commits[0].commit.verification.as_ref().expect("reported");
    assert!(v.verified);
    assert_eq!(v.signer.as_ref().unwrap().username, "ada");
    assert!(!commits[1].commit.verification.as_ref().unwrap().verified);
    // No `verification` object decodes to None, not to "unsigned".
    assert!(commits[2].commit.verification.is_none());
}

#[tokio::test]
async fn commit_get_reads_one_commit_and_names_a_missing_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/git/commits/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": "1111111111111111111111111111111111111111",
            "html_url": "https://forge.example/o/r/commit/1111111",
            "commit": { "message": "release", "verification": { "verified": false, "reason": "gpg.error.no_gpg_keys_found", "signature": "sig" } },
            "parents": [{ "sha": "0000000000000000000000000000000000000000" }],
            "stats": { "total": 3, "additions": 2, "deletions": 1 },
            "files": [{ "filename": "a.txt", "status": "modified" }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/git/commits/nope"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let commit = crate::api::commit::get(&c, "o", "r", "v1.0.0")
        .await
        .expect("commit");
    assert_eq!(commit.stats.as_ref().unwrap().additions, 2);
    assert_eq!(commit.files.as_ref().unwrap()[0].filename, "a.txt");
    let v = commit.commit.verification.unwrap();
    assert!(!v.verified);
    assert_eq!(v.signature.as_deref(), Some("sig"));

    let err = crate::api::commit::get(&c, "o", "r", "nope")
        .await
        .expect_err("404");
    assert!(
        format!("{err:#}").contains("commit 'nope' not found"),
        "{err:#}"
    );
}

// ---- contents at a ref (tag browsing) ----

#[tokio::test]
async fn contents_reads_file_and_directory_at_a_tag_ref() {
    use crate::api::contents::Contents;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/docs/a%20b.md"))
        .and(query_param("ref", "v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "a b.md", "path": "docs/a b.md", "type": "file", "size": 5,
            "sha": "abc", "encoding": "base64", "content": "aGVs\nbG8=",
            "last_commit_sha": "1111111"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents"))
        .and(query_param("ref", "v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "docs", "path": "docs", "type": "dir", "size": 0, "sha": "d1" },
            { "name": "README.md", "path": "README.md", "type": "file", "size": 12, "sha": "f1" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/contents/gone.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let file = crate::api::contents::get(&c, "o", "r", "docs/a b.md", Some("v1.0.0"))
        .await
        .expect("file");
    let Contents::Entry(entry) = file else {
        panic!("expected a single entry");
    };
    assert_eq!(
        crate::api::contents::file_bytes(&entry).unwrap().as_deref(),
        Some(b"hello".as_slice())
    );

    let dir = crate::api::contents::get(&c, "o", "r", "", Some("v1.0.0"))
        .await
        .expect("dir");
    let Contents::Dir(entries) = dir else {
        panic!("expected a directory listing");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "dir");

    let err = crate::api::contents::get(&c, "o", "r", "gone.txt", Some("v1.0.0"))
        .await
        .expect_err("404");
    assert!(
        format!("{err:#}").contains("path 'gone.txt' not found at 'v1.0.0'"),
        "{err:#}"
    );
}

// ---- organization labels and exclusive groups ----

#[tokio::test]
async fn org_label_crud_uses_org_label_endpoints() {
    use crate::api::label::{CreateLabel, EditLabel};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/team/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 7, "name": "priority/high", "color": "ee0000", "exclusive": true, "is_archived": false }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs/team/labels"))
        .and(body_json(
            json!({ "name": "priority/low", "color": "00ee00", "exclusive": true }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 8, "name": "priority/low", "color": "00ee00", "exclusive": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/orgs/team/labels/8"))
        .and(body_json(json!({ "exclusive": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 8, "name": "priority/low", "color": "00ee00", "exclusive": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orgs/team/labels/8"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    // The repo label collection is never consulted for an org operation.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/team/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let labels = crate::api::label::list_org(&c, "team").await.expect("list");
    assert_eq!(labels[0].exclusive_scope(), Some("priority"));
    let created = crate::api::label::create_org(
        &c,
        "team",
        &CreateLabel {
            name: "priority/low",
            color: "00ee00",
            description: None,
            exclusive: Some(true),
        },
    )
    .await
    .expect("create");
    assert!(created.exclusive);
    let edited = crate::api::label::edit_org(
        &c,
        "team",
        created.id,
        &EditLabel {
            exclusive: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("edit");
    assert!(!edited.exclusive);
    crate::api::label::delete_org(&c, "team", created.id)
        .await
        .expect("delete");
}

#[tokio::test]
async fn org_label_calls_name_a_missing_org_and_keep_403_distinct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/ghost/labels"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/orgs/team/labels/8"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let missing = crate::api::label::list_org(&c, "ghost")
        .await
        .expect_err("404");
    assert!(
        format!("{missing:#}").contains("organization 'ghost' or its label was not found"),
        "{missing:#}"
    );
    let denied = crate::api::label::delete_org(&c, "team", 8)
        .await
        .expect_err("403");
    let denied = format!("{denied:#}");
    assert!(denied.contains("forbidden"), "{denied}");
    assert!(!denied.contains("was not found"), "{denied}");
}

#[tokio::test]
async fn label_names_resolve_to_org_labels_only_when_the_repo_has_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/team/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "bug", "color": "ee0000" },
            { "id": 2, "name": "shared", "color": "ee0000" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/team/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 50, "name": "priority/high", "color": "ee0000", "exclusive": true },
            { "id": 51, "name": "shared", "color": "00ee00" }
        ])))
        // One fetch covers every unmatched name; none when all are repo labels.
        .expect(1)
        .mount(&server)
        .await;
    // A user-owned repo: the org endpoint 404s, which means "no org labels".
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/r/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/orgs/alice/labels"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;

    let repo_only = vec!["bug".to_string()];
    let ids = crate::api::label::resolve_names_to_ids(&c, "team", "r", &repo_only)
        .await
        .expect("repo label");
    assert_eq!(ids, vec![1]);

    let mixed = vec![
        "priority/high".to_string(),
        "shared".to_string(),
        "bug".to_string(),
    ];
    let labels = crate::api::label::resolve_names(&c, "team", "r", &mixed)
        .await
        .expect("mixed");
    // The repo's own `shared` shadows the org's.
    assert_eq!(
        labels.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![50, 2, 1]
    );
    assert_eq!(labels[0].exclusive_scope(), Some("priority"));

    let err = crate::api::label::resolve_names(&c, "alice", "r", &repo_only)
        .await
        .expect_err("unknown on a user repo");
    assert!(err.to_string().contains("no label named 'bug'"));
}

#[tokio::test]
async fn assigning_an_exclusive_label_reports_the_forge_confirmed_replacement() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "bug", "color": "ee0000" },
            { "id": 10, "name": "priority/low", "color": "00ee00", "exclusive": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    // Only the requested id is sent: fj does not remove the sibling itself.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/labels"))
        .and(body_json(json!({ "labels": [11] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "bug", "color": "ee0000" },
            { "id": 11, "name": "priority/high", "color": "ee0000", "exclusive": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/labels/10"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let requested: Vec<crate::api::label::Label> = serde_json::from_value(json!([
        { "id": 11, "name": "priority/high", "color": "ee0000", "exclusive": true }
    ]))
    .unwrap();
    let assignment = crate::api::label::assign(&c, "o", "r", 3, requested)
        .await
        .expect("assign");
    assert_eq!(assignment.before.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        assignment
            .after
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>(),
        vec!["bug", "priority/high"]
    );
}

#[tokio::test]
async fn assigning_ordinary_labels_skips_the_before_read() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/issues/3/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/labels"))
        .and(body_json(json!({ "labels": [1] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 1, "name": "bug", "color": "ee0000" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let requested: Vec<crate::api::label::Label> =
        serde_json::from_value(json!([{ "id": 1, "name": "bug", "color": "ee0000" }])).unwrap();
    let assignment = crate::api::label::assign(&c, "o", "r", 3, requested)
        .await
        .expect("assign");
    assert!(assignment.before.is_none());
    assert_eq!(assignment.after.len(), 1);
}

// ── Forge flavor from route evidence ─────────────────────────────────────────

#[tokio::test]
async fn server_info_identifies_forgejo_from_its_own_route_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/forgejo/v1/version"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "version": "16.0.4+gitea-1.22.0" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "x" })))
        .expect(0)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let info = c.server_info().await;
    assert_eq!(info.flavor, Flavor::Forgejo);
    assert_eq!(info.raw, "16.0.4+gitea-1.22.0");
    assert_eq!(info.supports(Capability::JobLogs), Support::Supported);
    // Cached for the process: the second ask makes no request.
    assert_eq!(c.server_info().await.flavor, Flavor::Forgejo);
}

#[tokio::test]
async fn server_info_identifies_gitea_and_never_applies_the_forgejo_floor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/forgejo/v1/version"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "1.27.3" })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let info = c.server_info().await;
    assert_eq!(info.flavor, Flavor::Gitea);
    assert_eq!(info.raw, "1.27.3");
    assert!(crate::client::capability::floor_warning(&info).is_none());
    assert_eq!(info.supports(Capability::RunRerun), Support::Supported);
    assert_eq!(info.supports(Capability::RunCancel), Support::Unsupported);
}

#[tokio::test]
async fn server_info_keeps_an_unidentifiable_host_distinct() {
    // Neither version route answers.
    let silent = MockServer::start().await;
    let info = client_for(&silent).await.server_info().await;
    assert_eq!(info.flavor, Flavor::Unidentified);
    assert_eq!(info.supports(Capability::RunJobs), Support::Unknown);
    assert!(crate::client::capability::floor_warning(&info).is_none());

    // The Forgejo route is blocked rather than absent: a 403 is not evidence
    // of Gitea, even though the shared route answers with a 1.x version.
    let blocked = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/forgejo/v1/version"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&blocked)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": "1.20.0" })))
        .mount(&blocked)
        .await;
    let info = client_for(&blocked).await.server_info().await;
    assert_eq!(info.flavor, Flavor::Unidentified);
    assert_eq!(info.raw, "1.20.0");
    assert!(crate::client::capability::floor_warning(&info).is_none());
}

// ── Token-authenticated job logs: bounded byte ranges ────────────────────────

async fn mount_log_range(server: &MockServer, range: &str, content_range: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/7/logs"))
        .and(header("range", range))
        .and(header("authorization", "token test-token"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Range", content_range)
                .set_body_string(body),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn job_log_follows_byte_range_continuations_to_the_end() {
    let server = MockServer::start().await;
    mount_log_range(&server, "bytes=0-9", "bytes 0-9/25", "0123456789").await;
    mount_log_range(&server, "bytes=10-19", "bytes 10-19/25", "abcdefghij").await;
    mount_log_range(&server, "bytes=20-29", "bytes 20-24/25", "KLMNO").await;
    let c = client_for(&server).await;
    let log = crate::api::workflow::read_job_log(&c, "o", "r", 7, None, 0, 10, 1000)
        .await
        .expect("read")
        .expect("the route exists");
    assert_eq!(log.bytes, b"0123456789abcdefghijKLMNO");
    assert!(!log.truncated);
}

#[tokio::test]
async fn job_log_read_is_bounded_and_resumable() {
    let server = MockServer::start().await;
    mount_log_range(&server, "bytes=0-9", "bytes 0-9/*", "0123456789").await;
    mount_log_range(&server, "bytes=10-19", "bytes 10-19/*", "abcdefghij").await;
    let c = client_for(&server).await;
    // A 15-byte ceiling stops after the second chunk and says so.
    let log = crate::api::workflow::read_job_log(&c, "o", "r", 7, None, 0, 10, 15)
        .await
        .expect("read")
        .expect("the route exists");
    assert_eq!(log.bytes.len(), 20);
    assert!(log.truncated);

    // A later read picks up at an offset, with the attempt in the query.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/7/logs"))
        .and(header("range", "bytes=20-29"))
        .and(query_param("attempt", "2"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Range", "bytes 20-22/23")
                .set_body_string("xyz"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let log = crate::api::workflow::read_job_log(&c, "o", "r", 7, Some(2), 20, 10, 1000)
        .await
        .expect("read")
        .expect("the route exists");
    assert_eq!(log.bytes, b"xyz");
    assert!(!log.truncated);
}

#[tokio::test]
async fn job_log_handles_ignored_range_past_the_end_and_missing_route() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/1/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("whole log\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/2/logs"))
        .respond_with(ResponseTemplate::new(416))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/3/logs"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    // A server that ignores Range answers 200 with everything; a resumed read
    // keeps only what it has not seen.
    let log = crate::api::workflow::read_job_log(&c, "o", "r", 1, None, 6, 10, 1000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(log.bytes, b"log\n");
    // Nothing new yet.
    let log = crate::api::workflow::read_job_log(&c, "o", "r", 2, None, 40, 10, 1000)
        .await
        .unwrap()
        .unwrap();
    assert!(log.bytes.is_empty());
    // No such route, or no log recorded.
    assert!(
        crate::api::workflow::read_job_log(&c, "o", "r", 3, None, 0, 10, 1000)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn run_logs_on_forgejo_16_read_a_bounded_range_with_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 1200, "run_number": 12, "status": "success" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/1200/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 7, "name": "build", "status": "success", "attempt": 1 }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/jobs/7/logs"))
        .and(header("range", "bytes=0-1048575"))
        .and(header("authorization", "token test-token"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Range", "bytes 0-7/8")
                .set_body_string("one\ntwo\n"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "16.0.4").await;
    let logs = crate::api::workflow::fetch_run_logs_without_loki(&c, "o", "r", 12)
        .await
        .expect("logs");
    assert_eq!(logs.source, "forgejo-api");
    let lines: Vec<&str> = logs.jobs[0]
        .lines
        .iter()
        .map(|l| l.message.as_str())
        .collect();
    assert_eq!(lines, vec!["one", "two"]);
}

#[tokio::test]
async fn run_logs_on_an_older_server_report_the_limitation_without_a_request() {
    let server = MockServer::start().await;
    refuse_everything(&server).await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let err = crate::api::workflow::fetch_run_logs_without_loki(&c, "o", "r", 12)
        .await
        .expect_err("15.x has no token log route");
    assert!(crate::api::workflow::is_actions_unavailable(&err));
    let msg = err.to_string();
    assert!(msg.contains("Forgejo 15.0.2"), "got: {msg}");
    assert!(
        msg.contains(&format!("{}/o/r/actions/runs/12", server.uri())),
        "got: {msg}"
    );
}

#[tokio::test]
async fn run_logs_on_an_older_server_use_a_configured_loki() {
    let server = MockServer::start().await;
    let loki = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/tasks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tasks": [
            {
                "id": 123,
                "name": "build",
                "status": "failure",
                "started_at": "2026-07-03T00:00:00Z",
                "stopped_at": "2026-07-03T00:05:00Z"
            }
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    // The job and log routes are known absent, so they are never asked.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(no_authorization)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {
                "resultType": "streams",
                "result": [{
                    "stream": { "container": "/FORGEJO-ACTIONS-TASK-123-WORKFLOW-abcd" },
                    "values": [["1793577000000000000", "from loki\n"]]
                }]
            }
        })))
        .expect(1)
        .mount(&loki)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let logs = crate::api::workflow::fetch_run_logs_with_loki_base(
        &c,
        "o",
        "r",
        56,
        None,
        None,
        false,
        &loki.uri(),
    )
    .await
    .expect("logs");
    assert_eq!(logs.source, "loki");
    assert_eq!(logs.jobs[0].task_id, Some(123));
    assert_eq!(logs.jobs[0].lines[0].message, "from loki");
}

// ── Web routes never see the forge token ─────────────────────────────────────

#[tokio::test]
async fn web_run_page_is_requested_without_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/7/jobs/0/attempt/1"))
        .and(wiremock::matchers::header_exists("authorization"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/7/jobs/0/attempt/1"))
        .and(no_authorization)
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "state": {
                "run": { "title": "t", "status": "success", "done": true,
                         "jobs": [{ "name": "check", "status": "success", "duration": "2s" }] },
                "currentJob": {}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // A server with no jobs API: the overview falls back to the web route.
    let c = pinned_client(&server, Flavor::Forgejo, "15.0.2").await;
    let overview = crate::api::workflow::fetch_run_overview(&c, "o", "r", 7)
        .await
        .expect("overview");
    assert_eq!(overview.title, "t");
    assert_eq!(overview.jobs[0].duration, "2s");
    assert_eq!(overview.url, format!("{}/o/r/actions/runs/7", server.uri()));
}

#[tokio::test]
async fn run_overview_prefers_the_api_and_never_touches_the_web_route() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{
                "id": 1200, "run_number": 12, "name": "CI", "status": "failure",
                "head_branch": "main", "head_sha": "0123456789abcdef"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/1200/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 7, "name": "build", "status": "failure", "attempt": 1 }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "16.0.4").await;
    let overview = crate::api::workflow::fetch_run_overview(&c, "o", "r", 12)
        .await
        .expect("overview");
    assert_eq!(overview.title, "CI");
    assert_eq!(overview.status, "failure");
    assert_eq!(overview.branch, "main");
    assert_eq!(overview.jobs[0].name, "build");
}

#[tokio::test]
async fn artifacts_use_the_api_routes_where_the_server_has_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{ "id": 1200, "run_number": 12, "status": "success" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/1200/artifacts"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 4, "name": "dist", "size_in_bytes": 10, "expired": false }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/artifacts/4/zip"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let c = pinned_client(&server, Flavor::Forgejo, "16.0.4").await;
    let arts = crate::api::workflow::list_artifacts(&c, "o", "r", 12)
        .await
        .expect("list");
    assert_eq!((arts[0].id, arts[0].size), (4, 10));
    assert_eq!(arts[0].status, "completed");
    let bytes = crate::api::workflow::download_artifact(&c, "o", "r", 12, &arts[0])
        .await
        .expect("download");
    assert_eq!(&bytes[..2], b"PK");
}

// ── Reverse proxy with its own HTTP Basic credential ─────────────────────────

const PROXY_PASSWORD: &str = "hunter2-proxy";

fn proxy_credential(pat_transport: PatTransport) -> ProxyCredential {
    ProxyCredential {
        username: "ops".into(),
        password: PROXY_PASSWORD.into(),
        pat_transport,
    }
}

fn proxy_basic() -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("ops:{PROXY_PASSWORD}"))
    )
}

async fn proxied_client(server: &MockServer, pat_transport: PatTransport) -> Client {
    client_for(server)
        .await
        .with_proxy(proxy_credential(pat_transport))
        .expect("loopback is a safe transport")
}

async fn mount_basic_challenge(server: &MockServer, expected: u64) {
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("WWW-Authenticate", r#"Basic realm="gate""#),
        )
        .expect(expected)
        .mount(server)
        .await;
}

#[tokio::test]
async fn proxy_basic_challenge_then_success_carries_both_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", proxy_basic().as_str()))
        .and(header("x-forge-authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "alice" })))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    // Exactly one challenge: the first request goes out without the proxy
    // credential, and the second request of the process sends it up front.
    mount_basic_challenge(&server, 1).await;

    let c = proxied_client(&server, PatTransport::Header).await;
    for _ in 0..2 {
        let me: serde_json::Value = c
            .json(Method::GET, "/api/v1/user", &[], None::<&()>)
            .await
            .expect("both credentials accepted");
        assert_eq!(me["login"], "alice");
    }
}

#[tokio::test]
async fn proxy_407_challenge_leaves_authorization_to_the_forge() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("proxy-authorization", proxy_basic().as_str()))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "alice" })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(
            ResponseTemplate::new(407).insert_header("Proxy-Authenticate", r#"Basic realm="gate""#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = proxied_client(&server, PatTransport::Header).await;
    let me: serde_json::Value = c
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect("ok");
    assert_eq!(me["login"], "alice");
}

#[tokio::test]
async fn proxy_query_transport_moves_the_token_into_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", proxy_basic().as_str()))
        .and(query_param("token", "test-token"))
        .and(|req: &wiremock::Request| !req.headers.contains_key("x-forge-authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "alice" })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_basic_challenge(&server, 1).await;
    let c = proxied_client(&server, PatTransport::Query).await;
    let me: serde_json::Value = c
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
        .expect("ok");
    assert_eq!(me["login"], "alice");
}

#[tokio::test]
async fn proxy_wrong_credential_names_the_proxy_and_leaks_nothing() {
    let server = MockServer::start().await;
    // The proxy challenges again after the credential was sent.
    mount_basic_challenge(&server, 2).await;
    let c = proxied_client(&server, PatTransport::Header).await;
    let err = c
        .request(Method::GET, "/api/v1/user", &[], None)
        .await
        .expect_err("a rejected proxy credential is an error, not a 401 response");
    let msg = format!("{err:#}");
    assert!(msg.contains("rejected the proxy credential"), "got: {msg}");
    assert!(
        msg.contains("Your forge token was not the problem"),
        "got: {msg}"
    );
    assert!(msg.contains("fj auth proxy"), "got: {msg}");
    assert!(!msg.contains(PROXY_PASSWORD), "got: {msg}");
    assert!(!msg.contains("test-token"), "got: {msg}");
}

#[tokio::test]
async fn proxy_challenge_without_a_configured_credential_says_what_to_set() {
    let server = MockServer::start().await;
    mount_basic_challenge(&server, 1).await;
    let c = client_for(&server).await;
    let err = c
        .request(Method::GET, "/api/v1/user", &[], None)
        .await
        .expect_err("the challenge is a distinct error");
    let msg = err.to_string();
    assert!(msg.contains("reverse proxy"), "got: {msg}");
    assert!(msg.contains("not your forge token"), "got: {msg}");
    assert!(msg.contains("fj auth proxy --host"), "got: {msg}");
    assert!(msg.contains("FJ_PROXY_PASSWORD"), "got: {msg}");
}

#[tokio::test]
async fn forge_401_behind_an_accepting_proxy_is_not_blamed_on_the_proxy() {
    let server = MockServer::start().await;
    // The proxy lets the request through; the forge then rejects the token.
    // A forge 401 carries no Basic challenge.
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", proxy_basic().as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "message": "bad token" })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_basic_challenge(&server, 1).await;
    let c = proxied_client(&server, PatTransport::Header).await;
    let err = c
        .request(Method::GET, "/api/v1/user", &[], None)
        .await
        .expect_err("forge rejection");
    let msg = err.to_string();
    assert!(msg.contains("accepted the proxy credential"), "got: {msg}");
    assert!(msg.contains("X-Forge-Authorization"), "got: {msg}");
    assert!(!msg.contains("rejected the proxy credential"), "got: {msg}");

    // Without a proxy in the picture, a plain forge 401 is still just a 401.
    let plain = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&plain)
        .await;
    let res = client_for(&plain)
        .await
        .request(Method::GET, "/api/v1/user", &[], None)
        .await
        .expect("a forge 401 is a response");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn debug_output_never_contains_either_secret() {
    // `--debug` prints these lines for a request. No header is printed, and a
    // token riding in the query (the opt-in transport) is redacted in both the
    // URL and the query pairs.
    let url = Url::parse("https://ops:hunter2-proxy@git.example/api/v1/user?token=test-token&a=b")
        .unwrap();
    let query = vec![
        ("token".to_string(), "test-token".to_string()),
        ("access_token".to_string(), "test-token".to_string()),
        ("page".to_string(), "2".to_string()),
    ];
    let text = super::request_log_lines(&Method::GET, &url, &query, None).join("\n");
    assert!(!text.contains("test-token"), "{text}");
    assert!(!text.contains(PROXY_PASSWORD), "{text}");
    assert!(text.contains("page=2"), "{text}");
    assert!(text.contains("a=b"), "{text}");

    let debugged = format!("{:?}", proxy_credential(PatTransport::Query));
    assert!(!debugged.contains(PROXY_PASSWORD), "{debugged}");
}

// ---- runner registration token ----

#[tokio::test]
async fn runner_registration_token_uses_the_route_for_each_scope() {
    use crate::api::runner::{registration_token, Scope};
    let server = MockServer::start().await;
    for (route, token) in [
        (
            "/api/v1/repos/o/r/actions/runners/registration-token",
            "repo-token",
        ),
        (
            "/api/v1/orgs/team/actions/runners/registration-token",
            "org-token",
        ),
        (
            "/api/v1/user/actions/runners/registration-token",
            "user-token",
        ),
        ("/api/v1/admin/runners/registration-token", "instance-token"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": token })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = client_for(&server).await;
    let repo = Scope::Repo {
        owner: "o".into(),
        name: "r".into(),
    };
    for (scope, want) in [
        (repo, "repo-token"),
        (Scope::Org("team".into()), "org-token"),
        (Scope::User, "user-token"),
        (Scope::Instance, "instance-token"),
    ] {
        let got = registration_token(&c, &scope).await.expect("token");
        assert_eq!(got, want);
    }
}

#[tokio::test]
async fn runner_registration_token_reports_a_denial_and_an_empty_reply() {
    use crate::api::runner::{registration_token, Scope};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/runners/registration-token"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "forbidden"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/actions/runners/registration-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "token": "" })))
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let err = registration_token(&c, &Scope::Instance)
        .await
        .expect_err("403");
    assert_eq!(crate::error_exit_code(&err), crate::EXIT_AUTH_FORBIDDEN);
    let err = registration_token(&c, &Scope::User)
        .await
        .expect_err("empty");
    assert!(err.to_string().contains("without a registration token"));
}
