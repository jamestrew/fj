use super::repo_clone;
use super::support::{call, success};
use std::path::Path;
pub async fn check(dir: &Path, other_gateway: &str) {
    success(
        call(
            dir,
            &["auth", "switch", other_gateway, "--account", "a@test"],
        )
        .await,
    );
    repo_clone::remote("git@first-b.example:o/r.git");
    for command in ["issue", "pr", "release"] {
        success(call(dir, &["--account", "b@test", command, "list", "--web"]).await);
    }
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
    repo_clone::remote("git@pat.example:o/r.git");
    std::env::set_var("FJ_TOKEN_H7061742E6578616D706C65", "dummy-pat");
    for command in ["issue", "pr", "release"] {
        success(call(dir, &["--account", "pat-user", command, "list", "--web"]).await);
    }
    std::env::remove_var("FJ_TOKEN_H7061742E6578616D706C65");
}
