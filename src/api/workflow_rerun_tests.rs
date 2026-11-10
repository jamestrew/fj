use super::*;
use crate::client::ServerInfo;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Client) {
    let server = MockServer::start().await;
    let client = Client::for_base_url(
        Url::parse(&format!("{}/api/v1/", server.uri())).unwrap(),
        "test-token".into(),
    )
    .unwrap()
    .with_server_info(ServerInfo::new(Flavor::Forgejo, "17.0.0+gitea-1.22.0"));
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workflow_runs": [{"id": 900, "run_number": 56}]
        })))
        .mount(&server)
        .await;
    (server, client)
}

async fn jobs(server: &MockServer, snapshots: Vec<Value>) {
    let reads = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/900/jobs"))
        .and(header("authorization", "token test-token"))
        .respond_with(move |_: &wiremock::Request| {
            let index = reads
                .fetch_add(1, Ordering::SeqCst)
                .min(snapshots.len() - 1);
            ResponseTemplate::new(200).set_body_json(snapshots[index].clone())
        })
        .mount(server)
        .await;
}

async fn post(server: &MockServer, suffix: &str, status: u16, message: &str) {
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/repos/o/r/actions/{suffix}")))
        .and(header("authorization", "token test-token"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"message": message})))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn forgejo_rerun_maps_number_to_id_and_uses_token() {
    let (server, client) = setup().await;
    post(&server, "runs/900/rerun", 204, "").await;
    assert_eq!(
        rerun(&client, "o", "r", 56, RerunTarget::Run)
            .await
            .unwrap(),
        RerunOutcome::Started
    );
}

#[tokio::test]
async fn forgejo_rerun_job_preserves_zero_based_index() {
    let (server, client) = setup().await;
    jobs(&server, vec![json!([{"id":11}, {"id":12}])]).await;
    post(&server, "jobs/12/rerun", 204, "").await;
    assert_eq!(
        rerun(&client, "o", "r", 56, RerunTarget::Job(1))
            .await
            .unwrap(),
        RerunOutcome::Started
    );
    assert!(rerun(&client, "o", "r", 56, RerunTarget::Job(2))
        .await
        .unwrap_err()
        .to_string()
        .contains("out of range"));
}

#[tokio::test]
async fn forgejo_rerun_failed_skips_rescheduled_dependent() {
    let (server, client) = setup().await;
    let initial = json!([
        {"id":11,"status":"failure"}, {"id":12,"status":"cancelled"},
        {"id":13,"status":"success"}, {"id":14,"status":"skipped"}
    ]);
    jobs(
        &server,
        vec![
            initial.clone(),
            initial,
            json!([
                {"id":11,"status":"waiting"}, {"id":12,"status":"waiting"},
                {"id":13,"status":"success"}, {"id":14,"status":"skipped"}
            ]),
        ],
    )
    .await;
    post(&server, "jobs/11/rerun", 204, "").await;
    assert_eq!(
        rerun(&client, "o", "r", 56, RerunTarget::Failed)
            .await
            .unwrap(),
        RerunOutcome::Started
    );
    let posts: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
}

#[tokio::test]
async fn forgejo_rerun_failed_continues_after_rejection_and_reports_partial_progress() {
    let (server, client) = setup().await;
    jobs(&server, vec![json!([
        {"id":11,"status":"failure"}, {"id":12,"status":"cancelled"}, {"id":13,"status":"success"}
    ])]).await;
    post(&server, "jobs/11/rerun", 400, "job is running").await;
    post(&server, "jobs/12/rerun", 204, "").await;
    let error = rerun(&client, "o", "r", 56, RerunTarget::Failed)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("job is running"), "{error}");
    assert!(error.contains("HTTP 400"), "{error}");
    assert!(error.contains("1 job rerun(s) started"), "{error}");
}

#[tokio::test]
async fn forgejo_rerun_all_failed_uses_one_run_post() {
    let (server, client) = setup().await;
    jobs(
        &server,
        vec![json!([{"id":11,"status":"failure"}, {"id":12,"status":"cancelled"}])],
    )
    .await;
    post(&server, "runs/900/rerun", 204, "").await;
    assert_eq!(
        rerun(&client, "o", "r", 56, RerunTarget::Failed)
            .await
            .unwrap(),
        RerunOutcome::Started
    );
}

#[tokio::test]
async fn forgejo_rerun_nothing_failed_posts_nothing() {
    let (server, client) = setup().await;
    jobs(
        &server,
        vec![json!([{"id":11,"status":"success"}, {"id":12,"status":"skipped"}])],
    )
    .await;
    assert_eq!(
        rerun(&client, "o", "r", 56, RerunTarget::Failed)
            .await
            .unwrap(),
        RerunOutcome::NothingFailed
    );
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.method == "GET"));
}

#[tokio::test]
async fn forgejo_rerun_surfaces_in_progress_permission_and_missing_errors() {
    for (status, message) in [
        (400, "run is in progress"),
        (403, "write denied"),
        (404, "not found"),
    ] {
        let (server, client) = setup().await;
        post(&server, "runs/900/rerun", status, message).await;
        let error = rerun(&client, "o", "r", 56, RerunTarget::Run)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("HTTP {status}")), "{error}");
        if status != 404 {
            assert!(error.contains(message), "{error}");
        }
        if status == 403 {
            assert!(error.contains("permission denied"), "{error}");
        }
    }
}
