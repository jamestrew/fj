//! Token storage. Tokens live in the OS keychain under service `fj`, keyed by
//! hostname. We deliberately do not write tokens to disk.

use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE: &str = "fj";

fn entry(host: &str) -> Result<Entry> {
    Entry::new(SERVICE, host).with_context(|| format!("opening keychain entry for {host}"))
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
