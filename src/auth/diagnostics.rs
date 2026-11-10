//! Storage keys are not safe diagnostic labels: profile keys encode account names.
#[cfg(test)]
use super::*;
fn label(key: &str) -> &str {
    if key.starts_with("profile:") {
        "[redacted profile]"
    } else {
        key
    }
}
pub(super) fn line(key: &str, operation: &str, cause: &dyn std::error::Error) -> String {
    if label(key) != key {
        format!("keychain unavailable while {operation} for {}", label(key))
    } else {
        format!("keychain unavailable while {operation} for {key}: {cause}")
    }
}
pub(super) fn error(key: &str, operation: &str, cause: keyring::Error) -> anyhow::Error {
    if label(key) != key {
        anyhow::anyhow!("{operation} for {}", label(key))
    } else {
        anyhow::Error::new(cause).context(format!("{operation} for {key}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_key_diagnostics_are_redacted() {
        if std::env::var_os("FJ_DIAGNOSTIC_CHILD").is_some() {
            crate::client::set_debug(true);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tokens.toml");
            let key = "profile:686f7374:71612d70726976617465:pat:nonce";
            let failure = || {
                keyring::Error::NoStorageAccess(Box::new(std::io::Error::other(format!(
                    "{key} qa-private qa-pat"
                ))))
            };
            store_token_with_entry_and_file_path(key, "qa-pat", Err(failure()), &path).unwrap();
            assert!(
                load_persisted_token_from_entry_and_file_path(key, Err(failure()), &path)
                    .unwrap()
                    .is_some()
            );
            eprintln!("{:?}", error(key, "reading token", failure()));
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "auth::diagnostics::tests::profile_key_diagnostics_are_redacted",
                "--nocapture",
            ])
            .env("FJ_DIAGNOSTIC_CHILD", "1")
            .output()
            .unwrap();
        assert!(out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("[redacted profile]"));
        for private in ["qa-private", "71612d70726976617465", "qa-pat"] {
            assert!(!stderr.contains(private), "{stderr}");
        }
    }
}
