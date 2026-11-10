use super::support::{call, success};
use std::path::Path;
use wiremock::{
    matchers::{header, method, path_regex},
    Mock, MockServer, ResponseTemplate,
};
pub async fn check(dir: &Path, gateway: &MockServer) {
    let pat = tempfile::tempdir().unwrap();
    let direct = MockServer::start().await;
    std::fs::write(
        pat.path().join("hosts.toml"),
        format!(
            "[hosts.'first-a.example']\nweb_url = '{}'\ngit_protocol = 'https'\n",
            direct.uri()
        ),
    )
    .unwrap();
    std::fs::write(
        pat.path().join("tokens.toml"),
        "[tokens]\n'first-a.example' = 'legacy'\n",
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path_regex("/api/v1/repos/o/r/(issues|pulls|releases)$"))
        .and(header("authorization", "token legacy"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(6)
        .mount(&direct)
        .await;
    for (root, host, account) in [
        (dir, gateway.uri(), Some("a@test")),
        (pat.path(), "first-a.example".into(), None),
    ] {
        for explicit in [false, true] {
            let mut flags = Vec::new();
            if explicit {
                flags.extend(["--host", host.as_str()]);
            }
            if let Some(account) = account {
                flags.extend(["--account", account]);
            }
            for command in ["issue", "pr", "release"] {
                let mut args = flags.clone();
                args.extend([command, "list", "--json"]);
                success(call(root, &args).await);
                *args.last_mut().unwrap() = "--web";
                success(call(root, &args).await);
            }
        }
    }
    let log = std::fs::read_to_string(std::env::var_os("FJ_BROWSER_LOG").unwrap()).unwrap();
    assert!(log.lines().last().unwrap().starts_with(&direct.uri()));
}
