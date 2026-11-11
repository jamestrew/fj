use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Proxy setup for a host stored under a legacy mixed-case key configures that
/// entry instead of creating a lowercase twin.
#[tokio::test]
async fn proxy_setup_reuses_the_legacy_host_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("fj")).unwrap();
    let hosts = dir.path().join("fj/hosts.toml");
    std::fs::write(&hosts, "[hosts.'FORGE.EXAMPLE']\nuser = 'operator'\n").unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fj"))
        .env("XDG_CONFIG_HOME", dir.path())
        .env("FJ_CONFIG_DIR", dir.path().join("fj"))
        .env("FJ_NO_KEYCHAIN", "1")
        .current_dir(dir.path())
        .args([
            "auth",
            "proxy",
            "--host",
            "forge.example",
            "--username",
            "u",
        ])
        .arg("--with-password")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret\n")
        .await
        .unwrap();
    let out = child.wait_with_output().await.unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let config: toml::Value = std::fs::read_to_string(&hosts).unwrap().parse().unwrap();
    let table = config["hosts"].as_table().unwrap();
    assert_eq!(table.keys().collect::<Vec<_>>(), ["FORGE.EXAMPLE"]);
    assert_eq!(
        table["FORGE.EXAMPLE"]["proxy"]["username"].as_str(),
        Some("u")
    );
}
