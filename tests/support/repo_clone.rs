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
        vec![
            "remote",
            "add",
            "origin",
            "ssh://git@first-a.example:2222/o/r.git",
        ],
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
    ] {
        let mut args = vec!["--account", "b@test"];
        args.extend(command);
        let out = call(dir, &args).await;
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("instance"));
    }
}

pub fn remote(url: &str) {
    assert!(std::process::Command::new("git")
        .args(["remote", "set-url", "origin", url])
        .status()
        .unwrap()
        .success());
}
