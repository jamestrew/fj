//! Token storage. Tokens live in the OS keychain under service `fj`, keyed by
//! hostname (PAT mode) or by `fjord-account:<platform-url>` (Fjord Account
//! mode). We deliberately do not write tokens to disk.

use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE: &str = "fj";

/// Prefix on the keychain key that flags a Fjord Account bearer rather than
/// a per-host Forgejo PAT. The two key shapes are deliberately disjoint so a
/// `fj auth login --fjord` against the same hostname (which can't happen in
/// practice because the Fjord "host" is the platform URL) can't clobber a
/// pre-existing PAT for that host, or vice versa.
const FJORD_PREFIX: &str = "fjord-account:";

fn entry(host: &str) -> keyring::Result<Entry> {
    Entry::new(SERVICE, host)
}

/// Compose the keychain key for a Fjord Account bearer. `platform_url` is
/// stored verbatim so multiple deployments (prod + local dev) coexist.
pub fn fjord_keychain_key(platform_url: &str) -> String {
    format!("{FJORD_PREFIX}{platform_url}")
}

/// Save a token for `host`. Replaces any existing entry.
pub fn store_token(host: &str, token: &str) -> Result<()> {
    let entry = entry(host).with_context(|| format!("opening keychain entry for {host}"))?;
    entry
        .set_password(token)
        .with_context(|| format!("writing token for {host} to keychain"))?;
    Ok(())
}

/// Look up the stored token for `host`. Returns `Ok(None)` when not present
/// rather than an error so callers can prompt for login.
pub fn load_token(host: &str) -> Result<Option<String>> {
    load_token_from_entry(host, entry(host))
}

fn load_token_from_entry(host: &str, entry: keyring::Result<Entry>) -> Result<Option<String>> {
    let entry = match entry {
        Ok(entry) => entry,
        Err(err) => return handle_load_keyring_error(host, "opening keychain entry", err),
    };
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(err) => handle_load_keyring_error(host, "reading token from keychain", err),
    }
}

fn handle_load_keyring_error(
    host: &str,
    operation: &str,
    err: keyring::Error,
) -> Result<Option<String>> {
    match err {
        keyring::Error::NoEntry => Ok(None),
        keyring::Error::NoStorageAccess(cause) => {
            log_keychain_unavailable(host, operation, cause.as_ref());
            Ok(None)
        }
        keyring::Error::PlatformFailure(cause) => {
            log_keychain_unavailable(host, operation, cause.as_ref());
            Ok(None)
        }
        keyring::Error::BadEncoding(bytes) => Err(keyring::Error::BadEncoding(bytes))
            .with_context(|| format!("{operation} for {host}")),
        keyring::Error::TooLong(name, len) => Err(keyring::Error::TooLong(name, len))
            .with_context(|| format!("{operation} for {host}")),
        keyring::Error::Invalid(attr, reason) => Err(keyring::Error::Invalid(attr, reason))
            .with_context(|| format!("{operation} for {host}")),
        keyring::Error::Ambiguous(items) => {
            Err(keyring::Error::Ambiguous(items)).with_context(|| format!("{operation} for {host}"))
        }
        err => Err(err).with_context(|| format!("{operation} for {host}")),
    }
}

fn log_keychain_unavailable(
    host: &str,
    operation: &str,
    cause: &(dyn std::error::Error + 'static),
) {
    if crate::client::debug_enabled() {
        eprintln!("keychain unavailable while {operation} for {host}: {cause}");
    }
}

/// Return an ephemeral PAT from FJ_TOKEN when it is set to a non-empty value.
/// This is intentionally process-scoped for scripted/CI use and avoids
/// touching keychain services in headless containers.
pub fn env_token() -> Option<String> {
    std::env::var("FJ_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

pub fn delete_token(host: &str) -> Result<()> {
    let entry = entry(host).with_context(|| format!("opening keychain entry for {host}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing token for {host} from keychain")),
    }
}

/// Convenience: store a Fjord Account bearer for `platform_url`.
pub fn store_fjord_bearer(platform_url: &str, bearer: &str) -> Result<()> {
    store_token(&fjord_keychain_key(platform_url), bearer)
}

/// Convenience: load the Fjord Account bearer for `platform_url`.
pub fn load_fjord_bearer(platform_url: &str) -> Result<Option<String>> {
    load_token(&fjord_keychain_key(platform_url))
}

/// Convenience: delete the Fjord Account bearer for `platform_url`.
pub fn delete_fjord_bearer(platform_url: &str) -> Result<()> {
    delete_token(&fjord_keychain_key(platform_url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct TestKeychainCause(&'static str);

    impl fmt::Display for TestKeychainCause {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for TestKeychainCause {}

    fn test_cause(msg: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(TestKeychainCause(msg))
    }

    #[test]
    fn fjord_key_is_namespaced() {
        let k = fjord_keychain_key("https://forgejo-site.pages.dev");
        assert!(k.starts_with(FJORD_PREFIX));
        assert!(k.contains("forgejo-site.pages.dev"));
        // Bare hostname keys and fjord keys can't collide.
        assert_ne!(k, "forgejo-site.pages.dev");
    }

    #[test]
    fn env_token_trims_and_rejects_empty() {
        std::env::set_var("FJ_TOKEN", "  abc  ");
        assert_eq!(env_token().as_deref(), Some("abc"));
        std::env::set_var("FJ_TOKEN", "   ");
        assert_eq!(env_token(), None);
        std::env::remove_var("FJ_TOKEN");
        assert_eq!(env_token(), None);
    }

    #[test]
    fn load_token_treats_entry_new_storage_access_failure_as_absent() {
        let token = load_token_from_entry(
            "example.test",
            Err(keyring::Error::NoStorageAccess(test_cause(
                "keychain is locked",
            ))),
        )
        .expect("storage access failure should not propagate");

        assert_eq!(token, None);
    }

    #[test]
    fn load_token_treats_platform_failure_as_absent() {
        let token = handle_load_keyring_error(
            "example.test",
            "reading token from keychain",
            keyring::Error::PlatformFailure(test_cause("secret service unavailable")),
        )
        .expect("platform failure should not propagate");

        assert_eq!(token, None);
    }

    #[test]
    fn load_token_propagates_unexpected_keyring_errors() {
        let err = handle_load_keyring_error(
            "example.test",
            "reading token from keychain",
            keyring::Error::Invalid("service".into(), "bad value".into()),
        )
        .expect_err("invalid keyring error should propagate");

        assert!(err.to_string().contains("reading token"));
    }
}
