//! A saved work session belongs to one host, account and instance.
#[path = "support/pat_profiles.rs"]
pub mod profile_support;
#[path = "support/account_override.rs"]
pub mod support;
use profile_support::{call, login, server};
use wiremock::MockServer;

fn session(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join(".git/fj/session.json")).unwrap()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[tokio::test]
async fn resume_and_continue_refuse_a_session_from_another_account() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(d)
        .status()
        .unwrap()
        .success());
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-b", "qa-b").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;

    let saved = call(d, &["--account", "qa-a", "work", "context"]).await;
    assert!(saved.status.success(), "{}", stderr(&saved));
    let before = session(d);
    assert!(!before.contains("qa-a"), "no login name on disk");

    for sub in [["work", "resume"], ["work", "continue"]] {
        let mut args = vec!["--account", "qa-b"];
        args.extend(sub);
        let out = call(d, &args).await;
        assert!(!out.status.success());
        let err = stderr(&out);
        assert!(err.contains("different account or instance"), "{err}");
        assert!(!err.contains("qa-a") && !err.contains("qa-b"), "{err}");
        assert_eq!(session(d), before, "a refused command writes nothing");
    }
    let dry = call(d, &["--account", "qa-b", "work", "continue", "--dry-run"]).await;
    assert!(!dry.status.success(), "--dry-run checks the identity too");

    let ok = call(d, &["--account", "qa-a", "work", "continue"]).await;
    assert!(ok.status.success(), "{}", stderr(&ok));
    assert_ne!(session(d), before, "the same account advances the cursor");
    let resume = call(d, &["--account", "qa-a", "work", "resume"]).await;
    assert!(resume.status.success(), "{}", stderr(&resume));
}

#[tokio::test]
async fn legacy_sessions_stay_valid_for_the_legacy_identity_only() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(d)
        .status()
        .unwrap()
        .success());
    server(&first, "tok-a", "qa-a").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    let host = ["--host", "second.example"];
    let run = |extra: &[&str]| {
        let mut args: Vec<String> = host.iter().map(ToString::to_string).collect();
        args.extend(extra.iter().map(ToString::to_string));
        args
    };
    let call_with = |args: Vec<String>| async move {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        call(d, &refs).await
    };
    assert!(call_with(run(&["work", "context"])).await.status.success());
    assert!(session(d).contains("\"identity\": null"));
    assert!(call_with(run(&["work", "resume"])).await.status.success());
    assert!(call_with(run(&["work", "continue"])).await.status.success());
    // A profile on the same checkout never reads the legacy session.
    let other = call(
        d,
        &[
            "--host",
            "first.example",
            "--account",
            "qa-a",
            "work",
            "resume",
        ],
    )
    .await;
    assert!(!other.status.success());
    assert!(stderr(&other).contains("different account or instance"));
}
