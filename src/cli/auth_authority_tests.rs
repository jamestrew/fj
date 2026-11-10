use super::normalize_pat_host;
#[cfg(test)]
#[test]
fn stored_authority_matches_discovery() {
    for raw in [
        "http://FORGE.EXAMPLE:80",
        "https://forge.example:443",
        "http://forge.example:443",
        "https://forge.example:3001",
    ] {
        let key = normalize_pat_host(raw).unwrap().0;
        assert_eq!(
            crate::git::remote::parse_remote(&format!("{raw}/o/r.git"))
                .unwrap()
                .host,
            key
        );
    }
}
