//! Token storage. Tokens prefer the OS keychain under service `fj`, keyed by
//! hostname (PAT mode) or by `fjord-account:<platform-url>` (Fjord Account
//! mode). On headless systems without a usable keychain, fj falls back to a
//! 0600 TOML file in the XDG config directory. `FJ_NO_KEYCHAIN=1` skips the
//! keychain outright and uses that file store alone.

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SERVICE: &str = "fj";

/// Prefix on the keychain key that flags a Fjord Account bearer rather than
/// a per-host Forgejo PAT. The two key shapes are deliberately disjoint so a
/// `fj auth login --fjord` against the same hostname (which can't happen in
/// practice because the Fjord "host" is the platform URL) can't clobber a
/// pre-existing PAT for that host, or vice versa.
const FJORD_PREFIX: &str = "fjord-account:";

/// Prefix on the keychain key that flags a Fjord Account **refresh** token
/// (from the OIDC sign-in flow) rather than the short-lived access bearer.
/// Disjoint from every other prefix so the two Fjord tokens for one platform
/// URL coexist without collision.
const FJORD_REFRESH_PREFIX: &str = "fjord-refresh:";

/// Prefix on the keychain key that flags a stored Forgejo **web session
/// cookie** (the `i_like_gitea` value) rather than a PAT. The web frontend
/// watch/rerun/cancel routes authenticate by session cookie only and reject a
/// PAT (fj#103); a session imported with `fj auth session` is stored under
/// this disjoint key so it can never clobber, or be clobbered by, the host's
/// PAT.
const SESSION_PREFIX: &str = "session:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStoreLocation {
    Keychain,
    File(PathBuf),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileTokenStore {
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

fn entry(host: &str) -> keyring::Result<Entry> {
    Entry::new(SERVICE, host)
}

/// Whether to skip the OS keychain and use only the file store.
///
/// Set `FJ_NO_KEYCHAIN=1` for a run that must not touch it. Two cases need
/// that. A macOS keychain item's ACL names the binaries allowed to read it, so
/// a freshly built or differently signed `fj` is not on the list and the lookup
/// blocks in securityd waiting for an approval a headless session can never
/// give: the process hangs rather than failing. And a test that asserts fj did
/// not read the token store can only prove it by making the read impossible.
fn keychain_disabled() -> bool {
    std::env::var_os("FJ_NO_KEYCHAIN").is_some_and(|value| !value.is_empty() && value != "0")
}

fn token_file_path() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join("tokens.toml"))
}

/// Compose the keychain key for a Fjord Account bearer. `platform_url` is
/// stored verbatim so multiple deployments (prod + local dev) coexist.
pub fn fjord_keychain_key(platform_url: &str) -> String {
    format!("{FJORD_PREFIX}{platform_url}")
}

/// Save a token for `host`. Replaces any existing entry.
pub fn store_token(host: &str, token: &str) -> Result<TokenStoreLocation> {
    let file_path = token_file_path()?;
    if keychain_disabled() {
        store_file_token(&file_path, host, token)?;
        return Ok(TokenStoreLocation::File(file_path));
    }
    store_token_with_entry_and_file_path(host, token, entry(host), &file_path)
}

fn store_token_with_entry_and_file_path(
    host: &str,
    token: &str,
    entry: keyring::Result<Entry>,
    file_path: &Path,
) -> Result<TokenStoreLocation> {
    let entry = match entry {
        Ok(entry) => entry,
        Err(err) => {
            log_keychain_unavailable_from_error(host, "opening keychain entry", &err);
            store_file_token(file_path, host, token)?;
            return Ok(TokenStoreLocation::File(file_path.to_path_buf()));
        }
    };
    match entry.set_password(token) {
        Ok(()) => Ok(TokenStoreLocation::Keychain),
        Err(err) => {
            log_keychain_unavailable_from_error(host, "writing token to keychain", &err);
            store_file_token(file_path, host, token)?;
            Ok(TokenStoreLocation::File(file_path.to_path_buf()))
        }
    }
}

/// Look up the persisted token for `host`: keychain, then file.
/// Returns `Ok(None)` when not present rather than an error so callers can
/// prompt for login.
pub fn load_token(host: &str) -> Result<Option<String>> {
    load_persisted_token(host)
}

fn load_persisted_token(host: &str) -> Result<Option<String>> {
    let file_path = token_file_path()?;
    if keychain_disabled() {
        return load_file_token(&file_path, host);
    }
    load_persisted_token_from_entry_and_file_path(host, entry(host), &file_path)
}

fn load_persisted_token_from_entry_and_file_path(
    host: &str,
    entry: keyring::Result<Entry>,
    file_path: &Path,
) -> Result<Option<String>> {
    let entry = match entry {
        Ok(entry) => entry,
        Err(err) => {
            return handle_load_keyring_error(host, "opening keychain entry", err, file_path)
        }
    };
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => load_file_token(file_path, host),
        Err(err) => handle_load_keyring_error(host, "reading token from keychain", err, file_path),
    }
}

fn handle_load_keyring_error(
    host: &str,
    operation: &str,
    err: keyring::Error,
    file_path: &Path,
) -> Result<Option<String>> {
    match err {
        keyring::Error::NoEntry => load_file_token(file_path, host),
        keyring::Error::NoStorageAccess(cause) => {
            log_keychain_unavailable(host, operation, cause.as_ref());
            load_file_token(file_path, host)
        }
        keyring::Error::PlatformFailure(cause) => {
            log_keychain_unavailable(host, operation, cause.as_ref());
            load_file_token(file_path, host)
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

fn is_keychain_unavailable(err: &keyring::Error) -> bool {
    matches!(
        err,
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_)
    )
}

fn log_keychain_unavailable_from_error(host: &str, operation: &str, err: &keyring::Error) {
    match err {
        keyring::Error::NoStorageAccess(cause) | keyring::Error::PlatformFailure(cause) => {
            log_keychain_unavailable(host, operation, cause.as_ref());
        }
        _ => {}
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

/// Return a host-scoped ephemeral PAT from `FJ_TOKEN_<HOST>` when present.
///
/// The host part is a byte-for-byte uppercase hex encoding prefixed with `H`,
/// so `rasterhub.com` maps to `FJ_TOKEN_H7261737465726875622E636F6D`.
/// Unlike the legacy host-agnostic `FJ_TOKEN`, this is safe to honor even when
/// a command targets an explicit `--host`.
pub fn env_token_for_host(host: &str) -> Option<String> {
    std::env::var(host_env_token_var(host))
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

pub fn host_env_token_var(host: &str) -> String {
    format!("FJ_TOKEN_{}", host_env_suffix(host))
}

fn host_env_suffix(host: &str) -> String {
    let mut suffix = String::with_capacity(1 + host.len() * 2);
    suffix.push('H');
    for byte in host.bytes() {
        use std::fmt::Write;
        let _ = write!(suffix, "{byte:02X}");
    }
    suffix
}

pub fn host_env_session_var(host: &str) -> String {
    format!("FJ_SESSION_{}", host_env_suffix(host))
}

pub fn env_session_for_host(host: &str) -> Option<String> {
    std::env::var(host_env_session_var(host))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn session_cookie_for_host(host: &str, allow_generic_env: bool) -> Result<Option<String>> {
    if let Some(cookie) = env_session_for_host(host) {
        return Ok(Some(cookie));
    }
    if allow_generic_env {
        if let Some(cookie) = env_session() {
            return Ok(Some(cookie));
        }
    }
    load_session_cookie(host)
}

pub fn env_session_for_import(host: &str, allow_generic_env: bool) -> Option<String> {
    env_session_for_host(host).or_else(|| {
        if allow_generic_env {
            env_session()
        } else {
            None
        }
    })
}

fn load_file_store(path: &Path) -> Result<FileTokenStore> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(FileTokenStore::default()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn load_file_token(path: &Path, host: &str) -> Result<Option<String>> {
    Ok(load_file_store(path)?.tokens.get(host).cloned())
}

fn store_file_token(path: &Path, host: &str, token: &str) -> Result<()> {
    let mut store = load_file_store(path)?;
    store.tokens.insert(host.to_string(), token.to_string());
    write_file_store(path, &store)
}

fn delete_file_token(path: &Path, host: &str) -> Result<()> {
    let mut store = load_file_store(path)?;
    if store.tokens.remove(host).is_some() {
        write_file_store(path, &store)?;
    }
    Ok(())
}

/// Rewrite the token store atomically.
///
/// Truncate-then-write left the store at zero bytes between `open` and
/// `write_all`, so anything that stopped the process in that window destroyed
/// every token rather than the one being changed. `fj auth login` reaches this
/// through the error path of a keychain call that can hang, which is exactly
/// when a user reaches for Ctrl+C, and the interrupt backstop exits the process
/// from another thread without unwinding this one.
///
/// A temporary file in the same directory, fsynced, then renamed over the
/// target: rename is atomic, so a reader (or a crash, or a force-exit) sees the
/// old contents or the new ones and never an empty file. The window stops
/// existing rather than being made smaller, which is why this is the fix rather
/// than widening the shutdown grace.
///
/// The temp file is created 0600 by `tempfile` and set again explicitly before
/// the rename, so the secret is never briefly world-readable under a permissive
/// umask, and the target inherits the mode through the rename.
fn write_file_store(path: &Path, store: &FileTokenStore) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let text = toml::to_string_pretty(store).context("serializing token store")?;

    // Same directory as the target, because rename is only atomic within one
    // filesystem and a temp dir can be on another.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating a temporary file in {}", parent.display()))?;
    #[cfg(unix)]
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!(
                "setting permissions on the temporary {} file",
                path.display()
            )
        })?;
    tmp.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    // Before the rename, so a crash after it cannot leave the new name pointing
    // at unflushed content.
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("flushing {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    set_file_store_permissions(path)?;
    Ok(())
}

fn set_file_store_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if path.exists() {
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    Ok(())
}

pub fn delete_token(host: &str) -> Result<()> {
    let file_path = token_file_path()?;
    if keychain_disabled() {
        return delete_file_token(&file_path, host);
    }
    match entry(host) {
        Ok(entry) => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(err) if is_keychain_unavailable(&err) => {
                log_keychain_unavailable_from_error(host, "removing token from keychain", &err);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("removing token for {host} from keychain"));
            }
        },
        Err(err) if is_keychain_unavailable(&err) => {
            log_keychain_unavailable_from_error(host, "opening keychain entry", &err);
        }
        Err(err) => return Err(err).with_context(|| format!("opening keychain entry for {host}")),
    }
    delete_file_token(&file_path, host)
}

/// Convenience: store a Fjord Account bearer for `platform_url`.
pub fn store_fjord_bearer(platform_url: &str, bearer: &str) -> Result<TokenStoreLocation> {
    store_token(&fjord_keychain_key(platform_url), bearer)
}

/// Convenience: load the Fjord Account bearer for `platform_url`.
pub fn load_fjord_bearer(platform_url: &str) -> Result<Option<String>> {
    load_persisted_token(&fjord_keychain_key(platform_url))
}

/// Convenience: delete the Fjord Account bearer for `platform_url`.
pub fn delete_fjord_bearer(platform_url: &str) -> Result<()> {
    delete_token(&fjord_keychain_key(platform_url))
}

/// Compose the keychain key for a Fjord Account **refresh** token. Disjoint
/// from the access-bearer key so the short-lived access token and the
/// long-lived refresh token never clobber one another. Only the OIDC sign-in
/// flow stores one; credentials/PAT hosts have no entry here.
pub fn fjord_refresh_keychain_key(platform_url: &str) -> String {
    format!("{FJORD_REFRESH_PREFIX}{platform_url}")
}

/// Store the Fjord Account OIDC refresh token for `platform_url`.
pub fn store_fjord_refresh(platform_url: &str, token: &str) -> Result<TokenStoreLocation> {
    store_token(&fjord_refresh_keychain_key(platform_url), token)
}

/// Load the Fjord Account OIDC refresh token for `platform_url`. `Ok(None)`
/// when absent (a credentials/PAT host, or an OIDC host that never got one).
pub fn load_fjord_refresh(platform_url: &str) -> Result<Option<String>> {
    load_persisted_token(&fjord_refresh_keychain_key(platform_url))
}

/// Delete the Fjord Account OIDC refresh token for `platform_url`.
pub fn delete_fjord_refresh(platform_url: &str) -> Result<()> {
    delete_token(&fjord_refresh_keychain_key(platform_url))
}

/// Compose the keychain key for a host's Forgejo web session cookie. Disjoint
/// from the bare-host PAT key and the Fjord bearer key by construction.
pub fn session_keychain_key(host: &str) -> String {
    format!("{SESSION_PREFIX}{host}")
}

/// Return an ephemeral session cookie from `FJ_SESSION` when set to a non-empty
/// value. Process-scoped, for scripted/CI use, mirroring [`env_token`]; avoids
/// touching keychain services in headless containers.
pub fn env_session() -> Option<String> {
    std::env::var("FJ_SESSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Store the Forgejo web session cookie (`i_like_gitea` value) for `host`.
/// Replaces any existing entry. The value is a raw cookie, never a PAT.
pub fn store_session_cookie(host: &str, cookie: &str) -> Result<TokenStoreLocation> {
    store_token(&session_keychain_key(host), cookie)
}

/// Load the persisted web session cookie for `host`. Environment overrides are
/// resolved by callers that know whether generic process-scoped credentials are
/// safe for the selected host.
pub fn load_session_cookie(host: &str) -> Result<Option<String>> {
    load_persisted_token(&session_keychain_key(host))
}

/// Delete the stored web session cookie for `host`.
pub fn delete_session_cookie(host: &str) -> Result<()> {
    delete_token(&session_keychain_key(host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    /// Regression for the truncate-then-write window: rewriting the token store
    /// must REPLACE the file, not empty it and refill it.
    ///
    /// Observed through a read handle opened before the write. Replacement by
    /// rename leaves that handle on the old inode, so it still sees the old
    /// bytes; truncate-in-place would show the handle the new (or, mid-write,
    /// an empty) file. That is the mechanism that makes the write atomic, so it
    /// is what this asserts, and it fails deterministically if the write goes
    /// back to `OpenOptions::truncate`.
    #[test]
    fn write_file_store_replaces_the_file_instead_of_truncating_it() {
        use std::io::Read;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokens.toml");

        let mut first = FileTokenStore::default();
        first.tokens.insert("a.example".into(), "aaaa".into());
        first.tokens.insert("b.example".into(), "bbbb".into());
        write_file_store(&path, &first).expect("first write");
        let original = fs::read_to_string(&path).expect("read original");
        assert!(original.contains("a.example") && original.contains("b.example"));

        // Opened BEFORE the rewrite, and deliberately kept open across it.
        let mut held = fs::File::open(&path).expect("hold a read handle");

        let mut second = FileTokenStore::default();
        second.tokens.insert("b.example".into(), "bbbb".into());
        write_file_store(&path, &second).expect("second write");

        let mut through_held = String::new();
        held.read_to_string(&mut through_held).expect("read held");
        assert_eq!(
            through_held, original,
            "the pre-existing handle must still see the old store; seeing anything \
             else means the target was rewritten in place, which is the window that \
             can leave it empty"
        );

        let now = fs::read_to_string(&path).expect("read new");
        assert!(now.contains("b.example"), "the new store must be readable");
        assert!(
            !now.contains("a.example"),
            "the new store must not carry the removed host"
        );
        assert!(!now.is_empty(), "the store must never be left empty");

        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the replacement must keep the store private");
        }
    }

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
    fn session_key_is_namespaced_and_disjoint() {
        let host = "rasterhub.com";
        let s = session_keychain_key(host);
        assert!(s.starts_with(SESSION_PREFIX));
        assert!(s.contains(host));
        // A session key can collide with neither the bare-host PAT key nor the
        // Fjord bearer key for the same string.
        assert_ne!(s, host);
        assert_ne!(s, fjord_keychain_key(host));
    }

    #[test]
    fn env_session_trims_and_rejects_empty() {
        std::env::set_var("FJ_SESSION", "  cookieval  ");
        assert_eq!(env_session().as_deref(), Some("cookieval"));
        std::env::set_var("FJ_SESSION", "   ");
        assert_eq!(env_session(), None);
        std::env::remove_var("FJ_SESSION");
        assert_eq!(env_session(), None);
    }

    #[test]
    fn host_scoped_env_session_is_named_from_host() {
        let var = host_env_session_var("commons.fjord.host");
        assert_eq!(var, "FJ_SESSION_H636F6D6D6F6E732E666A6F72642E686F7374");
        std::env::set_var(&var, "  scoped-cookie  ");
        assert_eq!(
            env_session_for_host("commons.fjord.host").as_deref(),
            Some("scoped-cookie")
        );
        std::env::set_var(&var, "   ");
        assert_eq!(env_session_for_host("commons.fjord.host"), None);
        std::env::remove_var(var);
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
    fn host_scoped_env_token_is_named_from_host() {
        let var = host_env_token_var("commons.fjord.host");
        assert_eq!(var, "FJ_TOKEN_H636F6D6D6F6E732E666A6F72642E686F7374");
        std::env::set_var(&var, "  scoped  ");
        assert_eq!(
            env_token_for_host("commons.fjord.host").as_deref(),
            Some("scoped")
        );
        std::env::set_var(&var, "   ");
        assert_eq!(env_token_for_host("commons.fjord.host"), None);
        std::env::remove_var(var);
    }

    #[test]
    fn host_scoped_env_vars_do_not_collapse_punctuation() {
        assert_ne!(
            host_env_token_var("foo-bar.com"),
            host_env_token_var("foo.bar.com")
        );
        assert_ne!(
            host_env_session_var("foo-bar.com"),
            host_env_session_var("foo.bar.com")
        );
    }

    #[test]
    fn load_token_uses_file_when_entry_creation_has_no_storage_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");
        store_file_token(&path, "example.test", "file-token").unwrap();

        let loaded = load_persisted_token_from_entry_and_file_path(
            "example.test",
            Err(keyring::Error::NoStorageAccess(test_cause(
                "keychain is locked",
            ))),
            &path,
        )
        .expect("storage access failure should fall through to file store");

        assert_eq!(loaded.as_deref(), Some("file-token"));
    }

    #[test]
    fn load_token_uses_file_when_platform_failure_blocks_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");
        store_file_token(&path, "example.test", "file-token").unwrap();

        let loaded = handle_load_keyring_error(
            "example.test",
            "reading token from keychain",
            keyring::Error::PlatformFailure(test_cause("secret service unavailable")),
            &path,
        )
        .expect("platform failure should fall through to file store");

        assert_eq!(loaded.as_deref(), Some("file-token"));
    }

    #[test]
    fn store_token_writes_file_when_keychain_unavailable_and_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/tokens.toml");

        let location = store_token_with_entry_and_file_path(
            "example.test",
            "file-token",
            Err(keyring::Error::NoStorageAccess(test_cause(
                "no secret service",
            ))),
            &path,
        )
        .expect("unavailable keychain should write file store");

        assert_eq!(location, TokenStoreLocation::File(path.clone()));
        let loaded = load_persisted_token_from_entry_and_file_path(
            "example.test",
            Err(keyring::Error::PlatformFailure(test_cause("no dbus"))),
            &path,
        )
        .expect("file token should load when keychain remains unavailable");
        assert_eq!(loaded.as_deref(), Some("file-token"));

        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn store_token_writes_file_when_keychain_entry_has_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");

        let location = store_token_with_entry_and_file_path(
            "example.test",
            "file-token",
            Err(keyring::Error::NoEntry),
            &path,
        )
        .expect("no keychain entry should write file store");

        assert_eq!(location, TokenStoreLocation::File(path.clone()));
        assert_eq!(
            load_file_token(&path, "example.test").unwrap().as_deref(),
            Some("file-token")
        );
    }

    #[test]
    fn store_token_writes_file_when_keychain_set_password_has_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");
        let entry = Entry::new_with_credential(Box::<keyring::mock::MockCredential>::default());
        let mock = entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap();
        mock.set_error(keyring::Error::NoEntry);

        let location =
            store_token_with_entry_and_file_path("example.test", "file-token", Ok(entry), &path)
                .expect("set_password NoEntry should write file store");

        assert_eq!(location, TokenStoreLocation::File(path.clone()));
        assert_eq!(
            load_file_token(&path, "example.test").unwrap().as_deref(),
            Some("file-token")
        );
    }

    #[test]
    fn load_token_uses_file_when_entry_creation_has_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");
        store_file_token(&path, "example.test", "file-token").unwrap();

        let loaded = load_persisted_token_from_entry_and_file_path(
            "example.test",
            Err(keyring::Error::NoEntry),
            &path,
        )
        .expect("missing keychain entry should fall through to file store");

        assert_eq!(loaded.as_deref(), Some("file-token"));
    }

    #[test]
    fn load_token_propagates_unexpected_keyring_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.toml");
        let err = handle_load_keyring_error(
            "example.test",
            "reading token from keychain",
            keyring::Error::Invalid("service".into(), "bad value".into()),
            &path,
        )
        .expect_err("invalid keyring error should propagate");

        assert!(err.to_string().contains("reading token"));
    }
}
