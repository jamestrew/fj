use super::support::call;
use std::path::Path;
use wiremock::{Mock, MockServer, ResponseTemplate};
pub async fn clone(dir: &Path) -> MockServer {
    let trap = MockServer::start().await;
    Mock::given(|_: &wiremock::Request| true)
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&trap)
        .await;
    let file = dir.join("hosts.toml");
    let mut hosts: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
    hosts["hosts"].as_table_mut().unwrap().insert(
        "pat.example".into(),
        toml::Value::try_from(serde_json::json!({
            "git_protocol":"https", "web_url":trap.uri()
        }))
        .unwrap(),
    );
    std::fs::write(file, toml::to_string(&hosts).unwrap()).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["remote", "add", "origin", "git@first-a.example:o/r.git"],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    trap
}
pub async fn invalid_selection(dir: &Path) {
    let file = dir.join(".git/fj/session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    session["pull_request"] = serde_json::json!({"number":7,"title":"Useful saved PR","state":"open","url":"https://forge.example/pr/7"});
    let saved = serde_json::to_vec(&session).unwrap();
    std::fs::write(&file, &saved).unwrap();
    for command in ["context", "resume"] {
        let out = call(
            dir,
            &[
                "--account",
                "missing@test",
                "work",
                command,
                "-R",
                "o/r",
                "--json",
            ],
        )
        .await;
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("profile"));
        assert_eq!(std::fs::read(&file).unwrap(), saved);
    }
}

pub async fn wrong_instance(dir: &Path, server: &MockServer) {
    assert!(std::process::Command::new("git")
        .args(["config", "--local", "fj.resolved", "o/r"])
        .status()
        .unwrap()
        .success());
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(server)
        .await;
    let file = dir.join(".git/fj/session.json");
    let before = std::fs::read(&file).unwrap();
    for command in [
        vec!["issue", "create", "--title", "wrong", "--body", "wrong"],
        vec![
            "pr", "create", "--head", "feature", "--base", "main", "--title", "wrong", "--body",
            "wrong",
        ],
        vec!["release", "create", "v0", "--body", "wrong"],
        vec!["issue", "list", "--web"],
        vec!["pr", "list", "--web"],
        vec!["release", "list", "--web"],
        vec!["work", "context", "--json"],
        vec!["work", "resume", "--json"],
    ] {
        let mut args = vec!["--account", "b@test"];
        args.extend(command);
        let out = call(dir, &args).await;
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("instance"));
    }
    assert_eq!(std::fs::read(file).unwrap(), before);
}
