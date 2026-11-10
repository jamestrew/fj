//! Canonical host/port identity used for inferred repository destinations.
pub fn authority(url: &url::Url) -> Option<String> {
    let host = url.host()?.to_string();
    // SSH ports select a transport listener, not a web/API destination.
    if !matches!(url.scheme(), "http" | "https") {
        return Some(host);
    }
    let port = url.port_or_known_default().filter(|p| *p != 443);
    Some(match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn default_and_nondefault_ports_are_distinct() {
        for (remote, expected) in [
            ("https://forge.example:3001/o/r.git", "forge.example:3001"),
            ("https://forge.example:3002/o/r.git", "forge.example:3002"),
            ("https://forge.example:443/o/r.git", "forge.example"),
            ("https://forge.example/o/r.git", "forge.example"),
            ("http://forge.example/o/r.git", "forge.example:80"),
            ("ssh://git@forge.example:2222/o/r.git", "forge.example"),
            ("https://[::1]:3001/o/r.git", "[::1]:3001"),
        ] {
            assert_eq!(super::super::parse_remote(remote).unwrap().host, expected);
        }
    }
}
