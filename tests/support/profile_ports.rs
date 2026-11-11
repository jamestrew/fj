use super::{
    repo_clone,
    support::{call, success},
};
use std::path::Path;
use wiremock::MockServer;
pub async fn check(dir: &Path, gateway: &MockServer) {
    let file = dir.join("hosts.toml");
    let mut config: toml::Value = std::fs::read_to_string(&file).unwrap().parse().unwrap();
    for (account, port) in [("a@test", 3001), ("b@test", 3002)] {
        config["accounts"][gateway.uri()]["profiles"][account]["fjord"]
            ["default_instance_public_url"] =
            toml::Value::String(format!("https://forge.example:{port}/base"));
    }
    std::fs::write(&file, toml::to_string(&config).unwrap()).unwrap();
    repo_clone::remote("https://forge.example:3001/o/r.git");
    repo_clone::wrong_instance(dir, gateway, "instance").await;
    for command in ["issue", "pr", "release"] {
        success(call(dir, &["--account", "a@test", command, "list", "--web"]).await);
    }
    let log = std::env::var_os("FJ_BROWSER_LOG").unwrap();
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("https://forge.example:3001/base/o/r/issues"));
    config["accounts"][gateway.uri()]["profiles"]["a@test"]["fjord"]
        ["default_instance_public_url"] = toml::Value::String("https://forge.example/base".into());
    std::fs::write(file, toml::to_string(&config).unwrap()).unwrap();
    repo_clone::remote("https://forge.example:443/o/r.git");
    success(call(dir, &["--account", "a@test", "issue", "list", "--web"]).await);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().last(),
        Some("https://forge.example/base/o/r/issues")
    );
    // A pinned slug with an unparseable (path-mounted) remote must refuse, not
    // fall back to the default host, for mutations and browser shortcuts.
    repo_clone::remote("https://forge.example/base/o/r.git");
    repo_clone::wrong_instance(dir, gateway, "cannot infer the instance").await;
    // A bad higher-priority remote must not hand the slug to a valid fallback.
    let origin = "ssh://git@first-a.example:2222/o/r.git";
    repo_clone::remote(origin);
    let add = [
        "remote",
        "add",
        "upstream",
        "https://forge.example/base/o/r.git",
    ];
    assert!(std::process::Command::new("git")
        .args(add)
        .status()
        .unwrap()
        .success());
    repo_clone::wrong_instance(dir, gateway, "cannot infer the instance").await;
    // An unparseable lower-priority backup remote is never consulted.
    for args in [
        [
            "remote",
            "set-url",
            "upstream",
            "https://forge.example:443/o/r.git",
        ],
        ["remote", "add", "backup", "/mnt/backup/r.git"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    success(call(dir, &["--account", "a@test", "issue", "list", "--web"]).await);
}
