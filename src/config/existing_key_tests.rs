use super::{existing_key, Hosts};

fn hosts(keys: &[&str]) -> Hosts {
    let mut hosts = Hosts::default();
    for key in keys {
        hosts.hosts.insert((*key).into(), Default::default());
    }
    hosts
}

#[test]
fn authority_folds_case_but_mount_paths_stay_exact() {
    let legacy = hosts(&["FORGE.EXAMPLE:3000", "Forge.Example/Forge"]);
    let find = |h: &Hosts, k| existing_key(h, k).unwrap();
    assert_eq!(
        find(&legacy, "forge.example:3000").as_deref(),
        Some("FORGE.EXAMPLE:3000")
    );
    assert_eq!(
        find(&legacy, "forge.example/Forge").as_deref(),
        Some("Forge.Example/Forge")
    );
    assert_eq!(find(&legacy, "forge.example/forge"), None);
    // Exact keys win, so case-distinct mounts stay individually reachable.
    let mounts = hosts(&["forge.example/Forge", "forge.example/forge"]);
    for key in ["forge.example/Forge", "forge.example/forge"] {
        assert_eq!(find(&mounts, key).as_deref(), Some(key));
    }
    let twins = hosts(&["FORGE.EXAMPLE", "Forge.example"]);
    assert!(existing_key(&twins, "forge.example").is_err());
}
