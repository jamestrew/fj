//! Integration tests for the HTTP client against a local wiremock server.
//! Lives inline (not under `tests/`) because our crate is a binary, not a
//! library — `tests/` can't `use crate::client::...`.

#![cfg(test)]

use reqwest::Method;
use serde_json::json;
use url::Url;
use wiremock::matchers::{body_json, header, method, path, query_param};
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

    let c = Client::for_base_url_with_scheme(base, "jwt-token".into(), AuthScheme::FjordBearer)
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

// ── Run logs (web routes, not /api/v1) ───────────────────────────────────────
// These exercise the deliberately-non-API log readers in api::workflow. The
// key things under test: the web URL is anchored at the host ORIGIN (no
// `/api/v1/` prefix), the `/attempt/{n}` segment is present, an empty cursor
// list yields step metadata, and a populated list yields lines.

#[tokio::test]
async fn run_logs_fetch_job_logs_two_phase() {
    let server = MockServer::start().await;
    // Phase 1: empty `logCursors` → state only, two steps, no lines.
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/12/jobs/0/attempt/1"))
        .and(body_json(json!({ "logCursors": [] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "logs": { "stepsLog": [] },
            "state": {
                "run": { "done": true, "status": "failure", "jobs": [{"name":"build","status":"failure","duration":"1m"}] },
                "currentJob": {
                    "title": "build",
                    "steps": [
                        {"summary":"checkout","status":"success","duration":"1s"},
                        {"summary":"compile","status":"failure","duration":"9s"}
                    ],
                    "allAttempts": [{"number":1,"status":"failure"}]
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Phase 2: cursors for steps 0..2 → lines per step.
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/12/jobs/0/attempt/1"))
        .and(body_json(json!({ "logCursors": [
            {"step":0,"cursor":0,"expanded":true},
            {"step":1,"cursor":0,"expanded":true}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "logs": { "stepsLog": [
                {"step":0,"cursor":2,"lines":[{"index":1,"message":"cloned","timestamp":1.0}]},
                {"step":1,"cursor":3,"lines":[{"index":1,"message":"error[E0382]","timestamp":2.0}]}
            ]},
            "state": {
                "run": { "done": true, "status": "failure", "jobs": [] },
                "currentJob": { "title": "build", "steps": [
                    {"summary":"checkout","status":"success","duration":"1s"},
                    {"summary":"compile","status":"failure","duration":"9s"}
                ], "allAttempts": [] }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client_for(&server).await;
    let view = crate::api::workflow::fetch_job_logs(&c, "o", "r", 12, 0, 1)
        .await
        .expect("logs");
    assert_eq!(view.logs.steps_log.len(), 2);
    assert_eq!(view.logs.steps_log[1].lines[0].message, "error[E0382]");
    assert_eq!(view.state.current_job.steps[1].status, "failure");
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
    assert!(msg.contains("rejected API token auth"), "got: {msg}");
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
    assert!(!msg.contains("rejected API token auth"), "got: {msg}");
}

#[tokio::test]
async fn run_summary_reads_run_level_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/o/r/actions/runs/56/jobs/0/attempt/1"))
        .and(body_json(json!({ "logCursors": [] })))
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
    assert_eq!(run.link, "/o/r/actions/runs/56");
    assert_eq!(run.commit.short_sha, "abc1234567");
    assert_eq!(run.commit.branch.name, "main");
    assert_eq!(run.commit.pusher.display_name, "stephen");
    assert_eq!(run.jobs.len(), 1);
    assert_eq!(run.jobs[0].status, "success");
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
    let c = client_for(&server).await;
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
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04zip".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    let bytes = crate::api::workflow::download_artifact(&c, "o", "r", 5, "binary")
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
async fn list_secrets_parses_envelope() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/secrets"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "secrets": [
                { "name": "DEPLOY_KEY", "created_at": "2024-01-02T03:04:05Z" },
                { "name": "TOKEN", "created_at": "2024-02-03T04:05:06Z" }
            ]
        })))
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
async fn list_variables_parses_envelope() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/variables"
        )))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "variables": [
                { "name": "REGION", "data": "us-east-1" },
                { "name": "STAGE", "data": "prod" }
            ]
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let vars = crate::api::workflow::list_variables(&client, owner, name)
        .await
        .unwrap();
    assert_eq!(vars.len(), 2);
    assert_eq!(vars[0].name, "REGION");
    assert_eq!(vars[0].data, "us-east-1");
    assert_eq!(vars[1].name, "STAGE");
}

#[tokio::test]
async fn set_variable_puts_value_body() {
    let server = MockServer::start().await;
    let owner = "o";
    let name = "r";
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{name}/actions/variables/REGION"
        )))
        .and(body_json(json!({ "value": "us-west-2" })))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    crate::api::workflow::set_variable(&client, owner, name, "REGION", "us-west-2")
        .await
        .unwrap();
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
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "artifacts": [
                { "name": "build", "size": 1024, "status": "completed" },
                { "name": "coverage", "size": 2048, "status": "expired" }
            ]
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
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
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 1, "number": 12, "title": "Add feature",
                "state": "open", "user": { "id": 1, "login": "alice" },
                "html_url": "https://x/o/r/pulls/12",
                "head": { "ref": "feature", "sha": "aaa" },
                "base": { "ref": "main", "sha": "bbb" },
                "created_at": "2026-05-30T00:00:00Z",
                "updated_at": "2026-05-30T00:01:00Z"
            }
        ])))
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
async fn pull_create_posts_title_head_base() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/pulls"))
        .and(body_json(json!({
            "title": "Add feature", "head": "feature", "base": "main",
            "body": "details", "draft": false
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
        },
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
            delete_branch: true,
            ..Default::default()
        },
    )
    .await
    .expect("merge");
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
    let rels = crate::api::release::list(&c, "o", "r", 30)
        .await
        .expect("list");
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].tag_name, "v1.0.0");
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
async fn issue_assignees_add_and_remove_use_issue_assignees_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/o/r/issues/3/assignees"))
        .and(body_json(json!({ "assignees": ["alice", "bob"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(3, "a bug", "open")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/o/r/issues/3/assignees"))
        .and(body_json(json!({ "assignees": ["bob"] })))
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
    let c = client_for(&server).await;
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
    let c = client_for(&server).await;
    let err = crate::api::workflow::list_artifacts(&c, "o", "r", 9)
        .await
        .expect_err("should 404");
    assert!(err.to_string().contains("not found"));
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
async fn pull_ready_patches_draft_false() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/o/r/pulls/12"))
        .and(body_json(json!({ "draft": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(12, "open")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_for(&server).await;
    crate::api::pull::ready(&c, "o", "r", 12)
        .await
        .expect("ready");
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
            query: Some("fj"),
        },
    )
    .await
    .expect("search");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "fj");
}
