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

fn git(d: &std::path::Path, args: &[&str]) {
    assert!(std::process::Command::new("git")
        .args(args)
        .current_dir(d)
        .status()
        .unwrap()
        .success());
}

#[tokio::test]
async fn the_clone_host_not_the_default_host_binds_the_session() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    git(d, &["init", "-q"]);
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-b", "qa-b").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;
    // The clone lives on second.example; the configured default is first.example.
    git(
        d,
        &["remote", "add", "origin", "https://second.example/o/r.git"],
    );
    let saved = call(d, &["work", "context"]).await;
    assert!(saved.status.success(), "{}", stderr(&saved));
    assert!(session(d).contains("\"identity\": null"));
    // An account selected for first.example never reads the clone's legacy session.
    let other = call(d, &["--account", "qa-a", "work", "resume"]).await;
    assert!(!other.status.success());
    // The same clone host, resolved the same way, resumes.
    assert!(call(d, &["work", "resume"]).await.status.success());
}

#[tokio::test]
async fn a_sole_host_without_current_still_binds_the_selected_account() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    git(d, &["init", "-q"]);
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-b", "qa-b").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;
    let hosts = std::fs::read_to_string(d.join("fj/hosts.toml")).unwrap();
    let mut doc: toml::Value = toml::from_str(&hosts).unwrap();
    let table = doc.as_table_mut().unwrap();
    table.remove("current");
    table["hosts"]
        .as_table_mut()
        .unwrap()
        .remove("second.example");
    std::fs::write(d.join("fj/hosts.toml"), toml::to_string(&doc).unwrap()).unwrap();
    let saved = call(d, &["--account", "qa-a", "work", "context"]).await;
    assert!(saved.status.success(), "{}", stderr(&saved));
    assert!(!session(d).contains("\"identity\": null"));
    for sub in ["resume", "continue"] {
        let out = call(d, &["--account", "qa-b", "work", sub]).await;
        assert!(!out.status.success(), "{sub}");
        assert!(stderr(&out).contains("different account or instance"));
    }
}

#[tokio::test]
async fn an_explicit_repo_uses_the_default_host_not_the_clone_host() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    git(d, &["init", "-q"]);
    server(&first, "tok-a", "qa-a").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    git(
        d,
        &["remote", "add", "origin", "https://second.example/o/r.git"],
    );
    // -R selects the repository only; the destination stays the configured default.
    let saved = call(d, &["--account", "qa-a", "work", "context", "-R", "o/r"]).await;
    assert!(saved.status.success(), "{}", stderr(&saved));
    assert!(!session(d).contains("\"identity\": null"));
    let same = call(d, &["--account", "qa-a", "work", "resume", "-R", "o/r"]).await;
    assert!(same.status.success(), "{}", stderr(&same));
    // `continue` has no -R and follows how the session was saved.
    for extra in [vec!["--dry-run"], vec![]] {
        let mut args = vec!["--account", "qa-a", "work", "continue"];
        args.extend(extra);
        let out = call(d, &args).await;
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let json = call(
        d,
        &[
            "--account",
            "qa-a",
            "work",
            "context",
            "-R",
            "o/r",
            "--json",
        ],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        v["host"], "first.example",
        "the data host, not the clone host"
    );
}

#[tokio::test]
async fn a_switch_while_gathering_is_not_saved_or_resumed() {
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let dir = support::config(&first, &second);
    let d = dir.path();
    git(d, &["init", "-q"]);
    server(&first, "tok-a", "qa-a").await;
    server(&first, "tok-b", "qa-b").await;
    login(d, "first.example", "qa-a", "tok-a").await;
    login(d, "first.example", "qa-b", "tok-b").await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(1500)))
        .mount(&first)
        .await;
    let switch = |account: &'static str| async move {
        let out = call(
            d,
            &["auth", "switch", "first.example", "--account", account],
        )
        .await;
        assert!(out.status.success(), "{}", stderr(&out));
    };
    let switch_during = |account: &'static str| async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        switch(account).await;
    };
    switch("qa-a").await;

    // A stable run saves.
    let ok = call(d, &["work", "context", "-R", "o/r"]).await;
    assert!(ok.status.success(), "{}", stderr(&ok));
    let saved = session(d);

    // The account changes while `work resume` reads the API: nothing is shown.
    let (resume, ()) = tokio::join!(
        call(d, &["work", "resume", "-R", "o/r"]),
        switch_during("qa-b")
    );
    assert!(!resume.status.success());
    assert!(
        stderr(&resume).contains("changed while gathering"),
        "{}",
        stderr(&resume)
    );
    assert!(resume.stdout.is_empty(), "no snapshot is printed");

    // The account changes while `work context` reads the API: the old session stays.
    switch("qa-a").await;
    let (context, ()) = tokio::join!(
        call(d, &["work", "context", "-R", "o/r"]),
        switch_during("qa-b")
    );
    assert!(!context.status.success());
    assert!(stderr(&context).contains("changed while gathering"));
    assert_eq!(session(d), saved, "the old session is not replaced");
}
