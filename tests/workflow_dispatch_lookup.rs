//! `fj workflow run` finds the run it just dispatched even though Forgejo
//! reports an empty `event` for `workflow_dispatch` runs.
#[path = "support/account_override.rs"]
pub mod support;
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn run(id: u64, event: &str, branch: &str, workflow: &str) -> serde_json::Value {
    json!({
        "id": id,
        "index_in_repo": id,
        "title": "t",
        "prettyref": branch,
        "commit_sha": "abc",
        "status": "running",
        "workflow_id": workflow,
        "event": event,
        "html_url": format!("https://forge.example/o/r/actions/runs/{id}"),
    })
}

async fn dispatch(
    ref_: &str,
    before: Vec<serde_json::Value>,
    after: Vec<serde_json::Value>,
) -> std::process::Output {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let runs = "/api/v1/repos/o/r/actions/runs";
    Mock::given(method("GET"))
        .and(path(runs))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": before})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&first)
        .await;
    Mock::given(method("GET"))
        .and(path(runs))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": after})))
        .with_priority(2)
        .mount(&first)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/repos/o/r/actions/workflows/ci.yml/dispatches",
        ))
        .and(body_json(json!({"ref": ref_, "return_run_info": true})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&first)
        .await;
    support::command(dir.path())
        .args([
            "--host",
            "first.example",
            "workflow",
            "run",
            "ci.yml",
            "-R",
            "o/r",
            "--ref",
            ref_,
            "--json",
        ])
        .output()
        .await
        .unwrap()
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test]
async fn a_dispatched_run_with_an_empty_event_is_found_on_a_feature_branch() {
    let old = run(10, "", "cl/feature", "ci.yml");
    let new = run(11, "", "cl/feature", "ci.yml");
    let o = dispatch("cl/feature", vec![old.clone()], vec![new, old]).await;
    assert!(o.status.success(), "{}", out(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["run_number"], 11, "{v}");
}

#[tokio::test]
async fn a_dispatched_run_reporting_workflow_dispatch_is_still_found() {
    let new = run(12, "workflow_dispatch", "main", "ci.yml");
    let o = dispatch("main", vec![], vec![new]).await;
    assert!(o.status.success(), "{}", out(&o));
}

#[tokio::test]
async fn another_trigger_on_the_same_branch_is_not_taken_for_the_dispatch() {
    // Only a push run appears after the dispatch; the lookup must not adopt
    // it. The command waits out its lookup window, so this one is slow.
    let push = run(13, "push", "main", "ci.yml");
    let o = dispatch("main", vec![], vec![push]).await;
    assert!(!o.status.success());
    assert!(
        out(&o).contains("could not find the triggered run"),
        "{}",
        out(&o)
    );
}

#[tokio::test]
async fn a_run_the_server_names_is_used_without_searching_the_run_list() {
    // The run list never shows the new run (a busy repo pushed it off the
    // page), but the dispatch response names it, so the command succeeds.
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_runs": []})))
        .mount(&first)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/api/v1/repos/o/r/actions/workflows/ci.yml/dispatches",
        ))
        .and(body_json(json!({"ref": "main", "return_run_info": true})))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id": 900, "run_number": 41, "jobs": ["b"]})),
        )
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r/actions/runs/900"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run(41, "", "main", "ci.yml")))
        .expect(1)
        .mount(&first)
        .await;
    let o = support::command(dir.path())
        .args([
            "--host",
            "first.example",
            "workflow",
            "run",
            "ci.yml",
            "-R",
            "o/r",
            "--ref",
            "main",
            "--json",
        ])
        .output()
        .await
        .unwrap();
    assert!(o.status.success(), "{}", out(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["run_number"], 41, "{v}");
}
