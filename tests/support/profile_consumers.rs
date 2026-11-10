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

async fn scoped(
    dir: &Path,
    server: &MockServer,
    account: &str,
    args: &[&str],
) -> std::process::Output {
    let host = server.uri();
    let mut flags = vec!["--host", &host, "--account", account];
    flags.extend_from_slice(args);
    call(dir, &flags).await
}
async fn scoped_json(dir: &Path, server: &MockServer, account: &str, args: &[&str]) -> Value {
    let out = scoped(dir, server, account, args).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
pub async fn mutes(dir: &Path, first: &MockServer, second: &MockServer, a: &str, b: &str) {
    success(call(dir, &["status", "mute", "o/r"]).await);
    assert_eq!(command_json(dir, &["status", "--json"]).await, json!([]));
    let mine = scoped_json(dir, first, a, &["status", "muted", "--json"]).await;
    assert_eq!(mine, json!(["o/r"]));
    for (server, account) in [(first, b), (second, a)] {
        let mutes = scoped_json(dir, server, account, &["status", "muted", "--json"]).await;
        assert_eq!(mutes, json!([]));
        let inbox = scoped_json(dir, server, account, &["status", "--json"]).await;
        assert_eq!(inbox.as_array().unwrap().len(), 1);
    }
    success(scoped(dir, first, b, &["status", "unmute", "o/r"]).await);
    assert_eq!(command_json(dir, &["status", "--json"]).await, json!([]));
    success(call(dir, &["status", "unmute", "o/r"]).await);
    let inbox = command_json(dir, &["status", "--json"]).await;
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    let persisted = std::fs::read_to_string(dir.join("muted_repos.toml")).unwrap();
    assert!(persisted.contains("#legacy"));
    assert!(persisted.contains("old/repo"));
    assert!(!persisted.contains(a));
    assert!(!persisted.contains(b));
}
