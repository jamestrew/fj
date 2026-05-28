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

fn entry(host: &str) -> Result<Entry> {
    Entry::new(SERVICE, host).with_context(|| format!("opening keychain entry for {host}"))
}

/// Compose the keychain key for a Fjord Account bearer. `platform_url` is
/// stored verbatim so multiple deployments (prod + local dev) coexist.
pub fn fjord_keychain_key(platform_url: &str) -> String {
    format!("{FJORD_PREFIX}{platform_url}")
}

/// Save a token for `host`. Replaces any existing entry.
pub fn store_token(host: &str, token: &str) -> Result<()> {
    let entry = entry(host)?;
    entry
        .set_password(token)
        .with_context(|| format!("writing token for {host} to keychain"))?;
    Ok(())
}

/// Look up the stored token for `host`. Returns `Ok(None)` when not present
/// rather than an error so callers can prompt for login.
pub fn load_token(host: &str) -> Result<Option<String>> {
    let entry = entry(host)?;
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading token for {host} from keychain")),
    }
}

pub fn delete_token(host: &str) -> Result<()> {
    let entry = entry(host)?;
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

    #[test]
    fn fjord_key_is_namespaced() {
        let k = fjord_keychain_key("https://forgejo-site.pages.dev");
        assert!(k.starts_with(FJORD_PREFIX));
        assert!(k.contains("forgejo-site.pages.dev"));
        // Bare hostname keys and fjord keys can't collide.
        assert_ne!(k, "forgejo-site.pages.dev");
    }
}
