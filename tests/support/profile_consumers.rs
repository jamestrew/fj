use super::support::{call, success, token};
use serde_json::{json, Value};
use std::path::Path;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};
pub async fn command_json(dir: &Path, args: &[&str]) -> Value {
    let out = call(dir, args).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
pub async fn routes(server: &MockServer, account: &str, instance: &str) {
    let authorization = format!("Bearer {}", token(account, 0));
    Mock::given(method("GET"))
        .and(path("/api/v1/me/forge-instances"))
        .and(header("authorization", authorization.as_str()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"instances":[{
                "id":instance,"displayName":instance,"slug":instance,"state":"ready",
                "publicUrl":format!("https://{instance}.example/base")
            }]})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET")).and(path(format!("/api/v1/forge-gateway/{instance}/api/v1/notifications")))
        .and(header("authorization", authorization.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id":1,"unread":true,"pinned":false,"updated_at":"2026-11-10T00:00:00Z","url":"",
            "subject":{"title":instance,"type":"Issue","state":"open"},"repository":{"full_name":"o/r"}
        }]))).mount(server).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/forge-gateway/{instance}/api/v1/repos/o/r"
        )))
        .and(header("authorization", authorization.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":1,"name":"r","full_name":"o/r","html_url":"https://forge.example/o/r",
            "default_branch":"main","private":true,"fork":false,"owner":{"id":1,"login":"owner"},
            "clone_url":"https://forge.example/o/r.git","ssh_url":"git@forge.example:o/r.git","updated_at":"2026-11-10T00:00:00Z"
        })))
        .mount(server)
        .await;
}
#[cfg(unix)]
pub fn browser(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    for program in ["open", "xdg-open"] {
        let file = bin.join(program);
        std::fs::write(
            &file,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$FJ_BROWSER_LOG\"\n",
        )
        .unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = dir.join("browser.log");
    std::env::set_var("FJ_BROWSER_LOG", &log);
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    log
}

pub async fn mutes(dir: &Path, first: &MockServer, second: &MockServer, a: &str, b: &str) {
    success(call(dir, &["status", "mute", "o/r"]).await);
    assert_eq!(
        command_json(dir, &["status", "--json"]).await,
        serde_json::json!([])
    );
    assert_eq!(
        command_json(
            dir,
            &[
                "--host",
                &first.uri(),
                "--account",
                a,
                "status",
                "muted",
                "--json"
            ]
        )
        .await,
        serde_json::json!(["o/r"])
    );
    for (server, account) in [(&first, b), (&second, a)] {
        assert_eq!(
            command_json(
                dir,
                &[
                    "--host",
                    &server.uri(),
                    "--account",
                    account,
                    "status",
                    "muted",
                    "--json"
                ]
            )
            .await,
            serde_json::json!([])
        );
        assert_eq!(
            command_json(
                dir,
                &[
                    "--host",
                    &server.uri(),
                    "--account",
                    account,
                    "status",
                    "--json"
                ]
            )
            .await
            .as_array()
            .unwrap()
            .len(),
            1
        );
    }
    success(
        call(
            dir,
            &[
                "--host",
                &first.uri(),
                "--account",
                b,
                "status",
                "unmute",
                "o/r",
            ],
        )
        .await,
    );
    assert_eq!(
        command_json(dir, &["status", "--json"]).await,
        serde_json::json!([])
    );
    success(call(dir, &["status", "unmute", "o/r"]).await);
    assert_eq!(
        command_json(dir, &["status", "--json"])
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let persisted = std::fs::read_to_string(dir.join("muted_repos.toml")).unwrap();
    assert!(persisted.contains("#legacy"));
    assert!(persisted.contains("old/repo"));
    assert!(!persisted.contains(a));
    assert!(!persisted.contains(b));
}
