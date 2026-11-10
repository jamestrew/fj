//! Atomic publication, serializing profile edits and preserving them in legacy saves.
use super::hosts::Hosts;
use anyhow::{Context, Result};
use std::io::Write;

fn write(path: &std::path::Path, hosts: &Hosts) -> Result<()> {
    write_with(path, hosts, |file, path| {
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    })
}
fn write_with(
    path: &std::path::Path,
    hosts: &Hosts,
    publish: impl FnOnce(tempfile::NamedTempFile, &std::path::Path) -> Result<()>,
) -> Result<()> {
    let parent = path.parent().context("hosts path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(toml::to_string_pretty(hosts)?.as_bytes())?;
    file.as_file().sync_all()?;
    publish(file, path)
}
pub(super) fn save(hosts: &Hosts) -> Result<()> {
    let _lock = crate::fjord::lock::acquire("auth-profile-publication")?;
    let mut next = hosts.clone();
    next.accounts = Hosts::load()?.accounts;
    write(&super::hosts_path()?, &next)
}
pub(super) fn edit(change: impl FnOnce(&mut Hosts) -> Result<()>) -> Result<()> {
    let _lock = crate::fjord::lock::acquire("auth-profile-publication")?;
    let mut hosts = Hosts::load()?;
    change(&mut hosts)?;
    write(&super::hosts_path()?, &hosts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_publication_failure_preserves_old_file_and_removes_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, "old active configuration").unwrap();
        assert!(write_with(&path, &Hosts::default(), |_, _| anyhow::bail!(
            "rename failed"
        ))
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "old active configuration"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        write(&path, &Hosts::default()).unwrap();
        let _: Hosts = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    }
}
