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
