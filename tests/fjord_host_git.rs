//! fj#222: fj gives git no credential for a Fjord Account host, so git it runs
//! must fail at once with an explanation, not wait on a prompt.
#[path = "support/account_override.rs"]
pub mod support;
use serde_json::json;
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn repo(clone_url: &str) -> serde_json::Value {
    json!({
        "id": 1, "name": "r", "full_name": "o/r",
        "owner": {"id": 2, "login": "o"},
        "html_url": "http://x/o/r", "clone_url": clone_url, "ssh_url": "git@x:o/r.git",
        "default_branch": "main", "updated_at": "2026-11-12T00:00:00Z"
    })
}

/// A forge that answers git's first request with a Basic-auth challenge.
async fn forge_demanding_credentials() -> MockServer {
    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/o/r.git/info/refs"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("WWW-Authenticate", "Basic realm=\"forge\""),
        )
        .mount(&forge)
        .await;
    forge
}

/// An askpass program that records that it ran, then waits like a GUI prompt.
fn hanging_askpass(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = dir.join("askpass-ran");
    let script = dir.join("askpass.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nsleep 60\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

async fn clone_in(
    dir: &std::path::Path,
    args: &[&str],
    wire_askpass: bool,
) -> (std::process::Output, Duration) {
    let (askpass, _) = hanging_askpass(dir);
    let started = Instant::now();
    let mut command = support::command(dir);
    command
        .env("FJ_CONFIG_DIR", dir.join("fj"))
        // No helper or ambient config may supply a credential; every askpass
        // route is wired to a program that would hang if git ever ran it.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env_remove("GIT_TERMINAL_PROMPT")
        .kill_on_drop(true)
        .args(args);
    if wire_askpass {
        command
            .env("GIT_ASKPASS", &askpass)
            .env("SSH_ASKPASS", &askpass)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.askPass")
            .env("GIT_CONFIG_VALUE_0", &askpass);
    }
    // Enforced: a child stuck in a prompt is killed at the deadline, not awaited.
    let out = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("fj hung waiting for a credential prompt")
        .unwrap();
    (out, started.elapsed())
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[tokio::test]
async fn a_fjord_host_clone_without_a_credential_fails_fast_with_the_explanation() {
    let platform = MockServer::start().await;
    let forge = forge_demanding_credentials().await;
    let dir = tempfile::tempdir().unwrap();
    let p = platform.uri();
    std::fs::create_dir(dir.path().join("fj")).unwrap();
    std::fs::write(
        dir.path().join("fj/hosts.toml"),
        format!(
            "current = \"{p}\"\n[hosts.\"{p}\"]\nauth_kind = \"fjord_account\"\ngit_protocol = \"https\"\n[hosts.\"{p}\".fjord_account]\nplatform_url = \"{p}\"\ndefault_instance_id = \"inst\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("fj/tokens.toml"),
        format!("[tokens]\n'fjord-account:{p}' = 'bearer-token'\n"),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/forge-gateway/inst/api/v1/repos/o/r"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(repo(&format!("{}/o/r.git", forge.uri()))),
        )
        .mount(&platform)
        .await;

    let (out, took) = clone_in(dir.path(), &["repo", "clone", "o/r", "dest"], true).await;
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(
        took < Duration::from_secs(30),
        "never waits on a prompt: {took:?}"
    );
    assert!(
        err.contains("terminal prompts disabled"),
        "git ran with GIT_TERMINAL_PROMPT=0: {err}"
    );
    assert!(err.contains("Fjord Account host"), "{err}");
    assert!(err.contains("fj#222"), "{err}");
    assert!(
        !err.contains("bearer-token"),
        "the bearer never reaches git or the output"
    );
    assert!(!dir.path().join("dest").exists(), "nothing was cloned");
    assert!(
        !dir.path().join("askpass-ran").exists(),
        "no askpass program (GIT_ASKPASS, core.askPass, SSH_ASKPASS) was started"
    );
}

#[tokio::test]
async fn a_pat_host_keeps_gits_own_prompting_and_gets_no_hint() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let forge = forge_demanding_credentials().await;
    let dir = support::config(&first, &second);
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/o/r"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(repo(&format!("{}/o/r.git", forge.uri()))),
        )
        .mount(&first)
        .await;

    let (out, _) = clone_in(
        dir.path(),
        &["--host", "first.example", "repo", "clone", "o/r", "dest"],
        false,
    )
    .await;
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(!err.contains("terminal prompts disabled"), "{err}");
    assert!(!err.contains("fj#222"), "{err}");
}
