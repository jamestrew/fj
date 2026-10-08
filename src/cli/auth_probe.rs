//! Token verification + server flavor/version probing shared by login and
//! refresh.

use anyhow::Result;

use crate::client::capability::{self, ServerInfo};
use crate::client::{Client, ProxyCredential, ResolvedHost};
use crate::config::hosts::Host;
use crate::output;

/// Establish which forge is on the other end and print what the user needs
/// to know about it: a floor warning for an old Forgejo, a plain note for a
/// host that could not be identified, nothing for a Gitea. Never fails.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn probe_server(client: &Client) -> ServerInfo {
    let info = client.server_info().await;
    for line in probe_messages(&info) {
        eprintln!("{line}");
    }
    info
}

/// The lines `probe_server` prints. Pure: the flavor decides, never the bare
/// version string, so a Gitea 1.x is not told it is below the Forgejo floor.
fn probe_messages(info: &ServerInfo) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(msg) = capability::floor_warning(info) {
        lines.push(format!("{} {msg}", output::bold("warning:")));
    }
    if let Some(msg) = capability::unidentified_note(info) {
        lines.push(format!("{} {msg}", output::bold("note:")));
    }
    lines
}

/// Record what the probe established on the host entry. An unidentified
/// result is stored as such (with whatever version string the shared route
/// gave) so `fj auth status` can say so instead of guessing.
pub(crate) fn record_server(host: &mut Host, info: &ServerInfo) {
    host.flavor = Some(info.flavor.as_str().to_string());
    host.server_version = (!info.raw.is_empty()).then(|| info.raw.clone());
}

/// "Forgejo 16.0.4+gitea-1.22.0", "Gitea 1.27.3", or "unidentified forge
/// (reports 1.21.11)". For the login line and `fj auth status`.
pub(crate) fn server_label(info: &ServerInfo) -> String {
    use crate::client::Flavor;
    match (info.flavor, info.raw.is_empty()) {
        (Flavor::Unidentified, true) => info.flavor.label().to_string(),
        (Flavor::Unidentified, false) => {
            format!("{} (reports {})", info.flavor.label(), info.raw)
        }
        (flavor, true) => format!("{} (version unknown)", flavor.label()),
        (flavor, false) => format!("{} {}", flavor.label(), info.raw),
    }
}

pub(crate) fn build_probe_client(
    host: &str,
    cfg: &Host,
    token: &str,
    proxy: Option<ProxyCredential>,
) -> Result<Client> {
    let resolved = ResolvedHost {
        name: host.to_string(),
        host: cfg.clone(),
        token: Some(token.to_string()),
        proxy,
    };
    Client::new(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::capability::{classify, RouteAnswer};
    use crate::client::Flavor;

    #[test]
    fn old_forgejo_gets_the_floor_warning() {
        let info = ServerInfo::new(Flavor::Forgejo, "6.0.0+gitea-1.20.0");
        let lines = probe_messages(&info);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("older than the Forgejo 7.x baseline"));
    }

    #[test]
    fn current_gitea_is_never_told_it_is_pre_7() {
        // The regression this module exists for: Gitea 1.27.3 has a leading
        // major of 1, which the old check read as an ancient Forgejo.
        let info = classify(
            &RouteAnswer::NotFound,
            &RouteAnswer::Ok(Some("1.27.3".into())),
        );
        assert_eq!(info.flavor, Flavor::Gitea);
        assert!(probe_messages(&info).is_empty());
    }

    #[test]
    fn modern_forgejo_is_silent() {
        let info = ServerInfo::new(Flavor::Forgejo, "16.0.4+gitea-1.22.0");
        assert!(probe_messages(&info).is_empty());
    }

    #[test]
    fn unidentified_host_gets_a_note_not_a_floor_warning() {
        let info = classify(
            &RouteAnswer::Inconclusive,
            &RouteAnswer::Ok(Some("1.21.11".into())),
        );
        let lines = probe_messages(&info);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("could not identify"));
        assert!(!lines[0].contains("older than"));
    }

    #[test]
    fn record_server_persists_flavor_and_raw_version() {
        let mut host = Host::default();
        record_server(&mut host, &ServerInfo::new(Flavor::Gitea, "1.27.3"));
        assert_eq!(host.flavor.as_deref(), Some("gitea"));
        assert_eq!(host.server_version.as_deref(), Some("1.27.3"));

        record_server(&mut host, &ServerInfo::unidentified());
        assert_eq!(host.flavor.as_deref(), Some("unidentified"));
        assert_eq!(host.server_version, None);
    }

    #[test]
    fn server_label_names_the_forge() {
        assert_eq!(
            server_label(&ServerInfo::new(Flavor::Forgejo, "16.0.4+gitea-1.22.0")),
            "Forgejo 16.0.4+gitea-1.22.0"
        );
        assert_eq!(
            server_label(&ServerInfo::new(Flavor::Gitea, "1.27.3")),
            "Gitea 1.27.3"
        );
        assert_eq!(
            server_label(&ServerInfo::new(Flavor::Gitea, "")),
            "Gitea (version unknown)"
        );
        assert_eq!(
            server_label(&ServerInfo::unidentified()),
            "unidentified forge"
        );
    }
}
