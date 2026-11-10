//! Serialize every token-file read/modify/write, regardless of credential kind.
use super::*;

fn lock(path: &Path) -> Result<crate::fjord::lock::RefreshLock> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::fjord::lock::acquire_in(dir, "token-store-write", std::time::Duration::from_secs(30))
}

pub(super) fn store_file_token(path: &Path, host: &str, token: &str) -> Result<()> {
    let _lock = lock(path)?;
    let mut store = load_file_store(path)?;
    store.tokens.insert(host.to_string(), token.to_string());
    write_file_store(path, &store)
}

pub(super) fn delete_file_token(path: &Path, host: &str) -> Result<()> {
    let _lock = lock(path)?;
    let mut store = load_file_store(path)?;
    if store.tokens.remove(host).is_some() {
        write_file_store(path, &store)?;
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn overlapping_token_writers_wait_and_preserve_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tokens.toml");
    let held = lock(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for name in ["legacy", "profile"] {
            let path = &path;
            let tx = &tx;
            scope.spawn(move || {
                store_file_token(path, name, "pat").unwrap();
                tx.send(()).unwrap();
            });
        }
        let blocked = rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err();
        drop(held);
        assert!(blocked);
    });
    assert_eq!(load_file_store(&path).unwrap().tokens.len(), 2);
}
