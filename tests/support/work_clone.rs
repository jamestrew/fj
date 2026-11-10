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
        "first-a.example".into(),
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
