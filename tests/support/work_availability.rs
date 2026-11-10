use super::{
    consumers::command_json,
    support::{call, success},
};
use std::path::Path;
pub async fn check(dir: &Path, other_gateway: &str) {
    success(
        call(
            dir,
            &["auth", "switch", other_gateway, "--account", "a@test"],
        )
        .await,
    );
    assert!(std::process::Command::new("git")
        .args(["remote", "set-url", "origin", "git@first-b.example:o/r.git"])
        .status()
        .unwrap()
        .success());
    for command in ["issue", "pr", "release"] {
        success(call(dir, &["--account", "b@test", command, "list", "--web"]).await);
    }
    for command in ["context", "resume"] {
        let value = command_json(dir, &["--account", "b@test", "work", command, "--json"]).await;
        let context = if command == "resume" {
            &value["current"]
        } else {
            &value
        };
        assert_eq!(context["api_reachable"], serde_json::json!(true));
        assert_eq!(
            context["account"]["fjord_instance"].as_str(),
            Some("first-b")
        );
    }
    std::fs::remove_file(dir.join("tokens.toml")).unwrap();
    for command in ["context", "resume"] {
        let value = command_json(dir, &["--account", "b@test", "work", command, "--json"]).await;
        let context = if command == "resume" {
            &value["current"]
        } else {
            &value
        };
        assert_eq!(context["api_reachable"], serde_json::json!(false));
        assert_eq!(
            context["account"]["fjord_instance"].as_str(),
            Some("first-b")
        );
    }
    super::work_clone::invalid_selection(dir).await;
    let file = dir.join("hosts.toml");
    let mut config: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
    for (_, host) in config["hosts"].as_table_mut().unwrap().iter_mut() {
        if let Some(fa) = host.get_mut("fjord_account").and_then(|v| v.as_table_mut()) {
            fa.insert(
                "default_instance_public_url".into(),
                toml::Value::String("https://pat.example".into()),
            );
        }
    }
    std::fs::write(file, toml::to_string(&config).unwrap()).unwrap();
    assert!(std::process::Command::new("git")
        .args(["remote", "set-url", "origin", "git@pat.example:o/r.git"])
        .status()
        .unwrap()
        .success());
    std::env::set_var("FJ_TOKEN_H7061742E6578616D706C65", "dummy-pat");
    for command in ["issue", "pr", "release"] {
        success(call(dir, &["--account", "pat-user", command, "list", "--web"]).await);
    }
    std::env::remove_var("FJ_TOKEN_H7061742E6578616D706C65");
    let fresh = tempfile::tempdir().unwrap();
    std::env::set_current_dir(fresh.path()).unwrap();
    let local = command_json(fresh.path(), &["work", "context", "-R", "o/r", "--json"]).await;
    assert_eq!(local["api_reachable"], serde_json::json!(false));
    assert_eq!(local["repo"].as_str(), Some("o/r"));
    std::env::set_current_dir(dir).unwrap();
}
