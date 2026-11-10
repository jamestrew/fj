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
pub(super) fn remember(mut hosts: Hosts) -> Hosts {
    hosts.base = Some(Box::new(hosts.clone()));
    hosts
}
fn merge(
    base: Option<&toml::Value>,
    wanted: Option<&toml::Value>,
    current: Option<&toml::Value>,
) -> Result<Option<toml::Value>> {
    if base == wanted {
        return Ok(current.cloned());
    }
    if current == base || current == wanted {
        return Ok(wanted.cloned());
    }
    if let (Some(toml::Value::Table(w)), Some(toml::Value::Table(c))) = (wanted, current) {
        let empty = toml::map::Map::new();
        let b = base.and_then(toml::Value::as_table).unwrap_or(&empty);
        let mut next = c.clone();
        for key in b
            .keys()
            .chain(w.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            match merge(b.get(key), w.get(key), c.get(key))? {
                Some(value) => {
                    next.insert(key.clone(), value);
                }
                None => {
                    next.remove(key);
                }
            }
        }
        return Ok(Some(toml::Value::Table(next)));
    }
    anyhow::bail!("configuration changed concurrently; retry the mutation")
}
pub(super) fn save(hosts: &Hosts) -> Result<()> {
    let _lock = crate::fjord::lock::acquire("auth-profile-publication")?;
    let base = toml::Value::try_from(hosts.base.as_deref().unwrap_or(&Hosts::default()))?;
    let next: Hosts = merge(
        Some(&base),
        Some(&toml::Value::try_from(hosts)?),
        Some(&toml::Value::try_from(Hosts::load()?)?),
    )?
    .unwrap()
    .try_into()?;
    if next.accounts.keys().any(|h| !next.hosts.contains_key(h)) {
        anyhow::bail!("host gained profiles concurrently; retry configuration change");
    }
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
    #[test]
    fn stale_legacy_mutation_preserves_new_host_and_profile_default() {
        let base: toml::Value = "current='a'\n[hosts.a]\n".parse().unwrap();
        let wanted: toml::Value = "current='a'\n[hosts.a]\n[groups]\nteam=['a']\n"
            .parse()
            .unwrap();
        let latest: toml::Value =
        "current='b'\n[hosts.a]\n[hosts.b]\n[accounts.b]\ndefault_account='qa'\n[accounts.b.profiles.qa]\nuser='qa'\ngeneration='1'\n"
            .parse()
            .unwrap();
        let next = merge(Some(&base), Some(&wanted), Some(&latest))
            .unwrap()
            .unwrap();
        assert_eq!(next["current"].as_str(), Some("b"));
        assert!(next["hosts"].get("b").is_some());
        assert_eq!(next["accounts"], latest["accounts"]);
        assert_eq!(next["groups"]["team"][0].as_str(), Some("a"));
    }
}
