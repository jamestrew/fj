//! Which forge is on the other end, and what it can serve.
//!
//! Forgejo and Gitea share `/api/v1` but number their releases on two
//! incomparable axes: Gitea ships 1.x, and Forgejo shipped its own 1.x before
//! 7.0. A single linear comparison therefore reads a current Gitea 1.27 as an
//! ancient Forgejo. Everything here is keyed on `(flavor, version)` so a
//! Forgejo number is never used to decide a Gitea capability, or the reverse.
//!
//! Flavor comes from route evidence, not from the version string:
//! `/api/forgejo/v1/version` answers 200 on Forgejo and 404 on Gitea, while
//! `/api/v1/version` answers on both. A host where neither observation is
//! conclusive stays [`Flavor::Unidentified`], which is a third outcome and not
//! a synonym for either forge.
//!
//! The gate table mirrors the one the Fjord apps ship
//! (`ForgejoCapability.swift` / `ForgejoCapability.kt`): each entry is the
//! first release whose published API description contains the route.

use std::fmt;

use reqwest::{Method, StatusCode};
use url::Url;

use super::Client;

/// The oldest Forgejo major fj is tested against.
pub const FORGEJO_FLOOR_MAJOR: u32 = 7;

/// Which forge answered. `Unidentified` is deliberately distinct from both
/// known forges: no Forgejo floor is applied to it and no Gitea gate either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Forgejo,
    Gitea,
    Unidentified,
}

impl Flavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Flavor::Forgejo => "forgejo",
            Flavor::Gitea => "gitea",
            Flavor::Unidentified => "unidentified",
        }
    }

    /// Parse the value persisted in hosts.toml. Anything unrecognized is
    /// `Unidentified` so a hand-edited or future value cannot be mistaken for
    /// a known forge.
    pub fn parse(s: &str) -> Flavor {
        match s.trim().to_ascii_lowercase().as_str() {
            "forgejo" => Flavor::Forgejo,
            "gitea" => Flavor::Gitea,
            _ => Flavor::Unidentified,
        }
    }

    /// Display name for messages.
    pub fn label(self) -> &'static str {
        match self {
            Flavor::Forgejo => "Forgejo",
            Flavor::Gitea => "Gitea",
            Flavor::Unidentified => "unidentified forge",
        }
    }
}

/// A release number on ONE forge's axis. Only ever compared against a floor
/// for the same flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32) -> Self {
        Version {
            major,
            minor,
            patch: 0,
        }
    }

    /// Parse the leading `major[.minor[.patch]]` of a reported version such as
    /// `16.0.4+gitea-1.22.0`, `1.27.3` or `v1.21`. Returns `None` when there is
    /// no leading number, so an unreadable version is never read as "old".
    pub fn parse(raw: &str) -> Option<Version> {
        let s = raw.trim().trim_start_matches(['v', 'V']);
        let head = s
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .next()
            .unwrap_or("");
        let mut parts = head.split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What the flavor probe established about a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub flavor: Flavor,
    /// Parsed release on the flavor's own axis. `None` when the server
    /// reported nothing readable.
    pub version: Option<Version>,
    /// The version string exactly as reported (may be empty).
    pub raw: String,
}

impl ServerInfo {
    pub fn unidentified() -> Self {
        ServerInfo {
            flavor: Flavor::Unidentified,
            version: None,
            raw: String::new(),
        }
    }

    pub fn new(flavor: Flavor, raw: &str) -> Self {
        ServerInfo {
            flavor,
            version: Version::parse(raw),
            raw: raw.to_string(),
        }
    }

    /// Whether this server can serve `cap`.
    pub fn supports(&self, cap: Capability) -> Support {
        support(self.flavor, self.version, cap)
    }

    /// "Forgejo 16.0.4", "Gitea 1.27.3", "Gitea (version unknown)", or
    /// "unidentified forge".
    pub fn describe(&self) -> String {
        match (self.flavor, self.version) {
            (Flavor::Unidentified, _) => Flavor::Unidentified.label().to_string(),
            (flavor, Some(v)) => format!("{} {v}", flavor.label()),
            (flavor, None) => format!("{} (version unknown)", flavor.label()),
        }
    }
}

/// An optional slice of the Actions API that not every supported server has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// `GET /repos/{o}/{r}/actions/runs/{run_id}/jobs`.
    RunJobs,
    /// `GET /repos/{o}/{r}/actions/jobs/{job_id}/logs`: one job's plaintext
    /// log for an API token.
    JobLogs,
    /// `GET /repos/{o}/{r}/actions/runs/{run_id}/logs`: a zip of every job's
    /// log. fj reads logs per job, so no command calls this gate today; the
    /// row exists so a run-level download cannot be added without it.
    #[allow(dead_code)]
    RunLogs,
    /// `POST /repos/{o}/{r}/actions/runs/{run_id}/cancel`.
    RunCancel,
    /// The artifact list for a run and the artifact zip by id.
    Artifacts,
    /// `POST .../actions/runs/{run_id}/rerun`, `.../rerun-failed-jobs` and
    /// `.../jobs/{job_id}/rerun`. Gitea only: no Forgejo serves them.
    RunRerun,
}

/// The answer a gate gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// The route is known to exist on this flavor at this version.
    Supported,
    /// The route is known to be absent: do not send the request.
    Unsupported,
    /// The flavor or version could not be established. Callers may attempt
    /// the API route and must classify a 404 as "not offered here".
    Unknown,
}

/// The first release of `flavor` that serves `cap`. `None` means that forge
/// serves it at no version, which is a settled no and not "upgrade and retry".
pub fn minimum_version(flavor: Flavor, cap: Capability) -> Option<Version> {
    match flavor {
        Flavor::Forgejo => match cap {
            Capability::RunJobs
            | Capability::JobLogs
            | Capability::RunLogs
            | Capability::RunCancel
            | Capability::Artifacts => Some(Version::new(16, 0)),
            Capability::RunRerun => None,
        },
        Flavor::Gitea => match cap {
            Capability::RunJobs => Some(Version::new(1, 25)),
            Capability::JobLogs | Capability::Artifacts => Some(Version::new(1, 24)),
            Capability::RunRerun => Some(Version::new(1, 26)),
            // Absent from 1.27.3 (the router 404s) and first present in the
            // 28.0.0 API description.
            Capability::RunLogs | Capability::RunCancel => Some(Version::new(28, 0)),
        },
        Flavor::Unidentified => None,
    }
}

/// Pure gate: does a server of `flavor` at `version` serve `cap`?
pub fn support(flavor: Flavor, version: Option<Version>, cap: Capability) -> Support {
    if flavor == Flavor::Unidentified {
        return Support::Unknown;
    }
    let Some(min) = minimum_version(flavor, cap) else {
        return Support::Unsupported;
    };
    match version {
        None => Support::Unknown,
        Some(v) if v >= min => Support::Supported,
        Some(_) => Support::Unsupported,
    }
}

/// The floor warning, if one applies. Only a Forgejo with a version we could
/// read can be below the Forgejo floor. A Gitea is never "pre-7", and an
/// unidentified host is not accused of being old.
pub fn floor_warning(info: &ServerInfo) -> Option<String> {
    if info.flavor != Flavor::Forgejo {
        return None;
    }
    let v = info.version?;
    if v.major == 0 || v.major >= FORGEJO_FLOOR_MAJOR {
        return None;
    }
    Some(format!(
        "server reports Forgejo version `{}`, which is older than the Forgejo {FORGEJO_FLOOR_MAJOR}.x \
         baseline fj is tested against. Some commands (pr ready, repo mirror-sync, search code, \
         milestone) may 404. See docs/compatibility.md.",
        info.raw
    ))
}

/// A note for a host whose flavor could not be established, so the user is
/// told the truth instead of nothing. Not a warning about age.
pub fn unidentified_note(info: &ServerInfo) -> Option<String> {
    (info.flavor == Flavor::Unidentified).then(|| {
        "could not identify this server as Forgejo or Gitea from its version routes. No \
         version floor is applied; optional Actions commands will try the API route and \
         report plainly if the server does not have it."
            .to_string()
    })
}

/// Why a gated feature is not offered, phrased for the forge in question.
pub fn unsupported_reason(info: &ServerInfo, cap: Capability, what: &str) -> String {
    match minimum_version(info.flavor, cap) {
        None => format!("{} has no API route to {what}", info.flavor.label()),
        Some(min) => format!(
            "{} cannot {what} over the API: {} added the route in {}.{}",
            info.describe(),
            info.flavor.label(),
            min.major,
            min.minor
        ),
    }
}

/// What one version route said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAnswer {
    /// 2xx. Carries the reported version when the body was readable.
    Ok(Option<String>),
    /// 404: the route does not exist on this server.
    NotFound,
    /// Anything else (401, 403, 5xx, transport error, not attempted).
    Inconclusive,
}

/// Decide the flavor from the two route observations.
///
/// * Forgejo route answers: Forgejo, version from that route.
/// * Forgejo route 404s AND the shared route answers: Gitea, version from the
///   shared route. Both halves are required: a 404 alone could be a proxy or a
///   path-mounted install, so it proves nothing without the shared route.
/// * Everything else: unidentified. The shared route's version string is kept
///   for display but is never parsed against either forge's floor.
pub fn classify(forgejo_route: &RouteAnswer, shared_route: &RouteAnswer) -> ServerInfo {
    match (forgejo_route, shared_route) {
        (RouteAnswer::Ok(v), _) => ServerInfo::new(Flavor::Forgejo, v.as_deref().unwrap_or("")),
        (RouteAnswer::NotFound, RouteAnswer::Ok(v)) => {
            ServerInfo::new(Flavor::Gitea, v.as_deref().unwrap_or(""))
        }
        (_, RouteAnswer::Ok(v)) => ServerInfo {
            flavor: Flavor::Unidentified,
            version: None,
            raw: v.clone().unwrap_or_default(),
        },
        _ => ServerInfo::unidentified(),
    }
}

/// The Forgejo-only version route for a client whose API base ends in
/// `api/v1/`. `None` when the base has another shape (a Fjord Account gateway
/// for example), in which case the probe is not attempted.
pub fn forgejo_version_url(base: &Url) -> Option<Url> {
    let prefix = base.path().strip_suffix("api/v1/")?;
    let mut url = base.clone();
    url.set_path(&format!("{prefix}api/forgejo/v1/version"));
    url.set_query(None);
    Some(url)
}

fn version_from_body(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

async fn ask(client: &Client, path: &str) -> RouteAnswer {
    let Ok(res) = client.request(Method::GET, path, &[], None).await else {
        return RouteAnswer::Inconclusive;
    };
    let status = res.status();
    if status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return RouteAnswer::Ok(version_from_body(&body));
    }
    if status == StatusCode::NOT_FOUND {
        RouteAnswer::NotFound
    } else {
        RouteAnswer::Inconclusive
    }
}

/// Probe the server's flavor and version. At most two GETs, never fails: an
/// unreachable or unreadable server is simply unidentified.
pub async fn probe(client: &Client) -> ServerInfo {
    let forgejo_route = match forgejo_version_url(client.base()) {
        Some(url) => ask(client, url.as_str()).await,
        None => RouteAnswer::Inconclusive,
    };
    if let RouteAnswer::Ok(_) = forgejo_route {
        return classify(&forgejo_route, &RouteAnswer::Inconclusive);
    }
    let shared_route = ask(client, "/api/v1/version").await;
    classify(&forgejo_route, &shared_route)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(flavor: Flavor, raw: &str) -> ServerInfo {
        ServerInfo::new(flavor, raw)
    }

    #[test]
    fn version_parses_forgejo_and_gitea_shapes() {
        assert_eq!(
            Version::parse("16.0.4+gitea-1.22.0"),
            Some(Version {
                major: 16,
                minor: 0,
                patch: 4
            })
        );
        assert_eq!(
            Version::parse("1.27.3"),
            Some(Version {
                major: 1,
                minor: 27,
                patch: 3
            })
        );
        assert_eq!(Version::parse("v1.21"), Some(Version::new(1, 21)));
        assert_eq!(Version::parse("10.5.2-beta"), Version::parse("10.5.2"));
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("totally-unknown"), None);
    }

    #[test]
    fn version_orders_minor_numerically() {
        assert!(Version::new(1, 27) > Version::new(1, 9));
        assert!(Version::parse("1.26.0").unwrap() >= Version::new(1, 26));
        assert!(Version::parse("1.25.9").unwrap() < Version::new(1, 26));
    }

    #[test]
    fn classify_forgejo_route_answering_is_forgejo() {
        let got = classify(
            &RouteAnswer::Ok(Some("16.0.4+gitea-1.22.0".into())),
            &RouteAnswer::Inconclusive,
        );
        assert_eq!(got.flavor, Flavor::Forgejo);
        assert_eq!(got.version, Some(Version::parse("16.0.4").unwrap()));
    }

    #[test]
    fn classify_forgejo_route_with_unreadable_body_is_forgejo_without_version() {
        let got = classify(&RouteAnswer::Ok(None), &RouteAnswer::Inconclusive);
        assert_eq!(got.flavor, Flavor::Forgejo);
        assert_eq!(got.version, None);
    }

    #[test]
    fn classify_404_plus_shared_route_is_gitea() {
        let got = classify(
            &RouteAnswer::NotFound,
            &RouteAnswer::Ok(Some("1.27.3".into())),
        );
        assert_eq!(got.flavor, Flavor::Gitea);
        assert_eq!(got.version, Some(Version::parse("1.27.3").unwrap()));
    }

    #[test]
    fn classify_keeps_unidentified_distinct() {
        // A 404 with no shared route answering proves nothing.
        assert_eq!(
            classify(&RouteAnswer::NotFound, &RouteAnswer::NotFound).flavor,
            Flavor::Unidentified
        );
        assert_eq!(
            classify(&RouteAnswer::NotFound, &RouteAnswer::Inconclusive).flavor,
            Flavor::Unidentified
        );
        // The Forgejo route was blocked (401 from a proxy, say): the shared
        // route answering does not make this a Gitea.
        let got = classify(
            &RouteAnswer::Inconclusive,
            &RouteAnswer::Ok(Some("1.21.11".into())),
        );
        assert_eq!(got.flavor, Flavor::Unidentified);
        assert_eq!(got.version, None, "never parsed against a floor");
        assert_eq!(got.raw, "1.21.11");
    }

    #[test]
    fn floor_warning_only_applies_to_forgejo() {
        assert!(floor_warning(&info(Flavor::Forgejo, "6.0.0+gitea-1.20.0")).is_some());
        assert!(floor_warning(&info(Flavor::Forgejo, "1.21.11")).is_some());
        assert!(floor_warning(&info(Flavor::Forgejo, "7.0.16+gitea-1.21.11")).is_none());
        assert!(floor_warning(&info(Flavor::Forgejo, "16.0.4")).is_none());
        // Unreadable or 0.x (dev build): no accusation.
        assert!(floor_warning(&info(Flavor::Forgejo, "")).is_none());
        assert!(floor_warning(&info(Flavor::Forgejo, "0.99.0")).is_none());
    }

    #[test]
    fn floor_warning_never_fires_for_gitea_1x() {
        assert!(floor_warning(&info(Flavor::Gitea, "1.27.3")).is_none());
        assert!(floor_warning(&info(Flavor::Gitea, "1.20.6")).is_none());
    }

    #[test]
    fn floor_warning_never_fires_for_unidentified() {
        let unidentified = classify(
            &RouteAnswer::Inconclusive,
            &RouteAnswer::Ok(Some("1.21.11".into())),
        );
        assert!(floor_warning(&unidentified).is_none());
        assert!(unidentified_note(&unidentified).is_some());
        assert!(unidentified_note(&info(Flavor::Gitea, "1.27.3")).is_none());
        assert!(unidentified_note(&info(Flavor::Forgejo, "16.0.4")).is_none());
    }

    #[test]
    fn gitea_1_27_3_offers_rerun_and_job_logs_but_not_cancel_or_run_logs() {
        let gitea = info(Flavor::Gitea, "1.27.3");
        assert_eq!(gitea.supports(Capability::RunRerun), Support::Supported);
        assert_eq!(gitea.supports(Capability::JobLogs), Support::Supported);
        assert_eq!(gitea.supports(Capability::RunJobs), Support::Supported);
        assert_eq!(gitea.supports(Capability::Artifacts), Support::Supported);
        assert_eq!(gitea.supports(Capability::RunCancel), Support::Unsupported);
        assert_eq!(gitea.supports(Capability::RunLogs), Support::Unsupported);
    }

    #[test]
    fn gitea_rerun_needs_1_26() {
        assert_eq!(
            info(Flavor::Gitea, "1.25.4").supports(Capability::RunRerun),
            Support::Unsupported
        );
        assert_eq!(
            info(Flavor::Gitea, "1.26.0").supports(Capability::RunRerun),
            Support::Supported
        );
    }

    #[test]
    fn forgejo_numbers_never_decide_a_gitea_capability() {
        // 1.27 is below every Forgejo entry (16) yet Gitea serves job logs.
        assert_eq!(
            info(Flavor::Gitea, "1.27.3").supports(Capability::JobLogs),
            Support::Supported
        );
        // And 16 is above the Gitea rerun entry (1.26) yet no Forgejo reruns.
        assert_eq!(
            info(Flavor::Forgejo, "16.0.4").supports(Capability::RunRerun),
            Support::Unsupported
        );
    }

    #[test]
    fn forgejo_actions_api_starts_at_16() {
        let old = info(Flavor::Forgejo, "15.0.2+gitea-1.22.0");
        let new = info(Flavor::Forgejo, "16.0.4+gitea-1.22.0");
        for cap in [
            Capability::RunJobs,
            Capability::JobLogs,
            Capability::RunLogs,
            Capability::RunCancel,
            Capability::Artifacts,
        ] {
            assert_eq!(old.supports(cap), Support::Unsupported, "{cap:?}");
            assert_eq!(new.supports(cap), Support::Supported, "{cap:?}");
        }
    }

    #[test]
    fn unknown_when_flavor_or_version_is_missing() {
        assert_eq!(
            ServerInfo::unidentified().supports(Capability::RunRerun),
            Support::Unknown
        );
        assert_eq!(
            info(Flavor::Gitea, "").supports(Capability::RunRerun),
            Support::Unknown
        );
        // A settled no does not need a version.
        assert_eq!(
            info(Flavor::Forgejo, "").supports(Capability::RunRerun),
            Support::Unsupported
        );
    }

    #[test]
    fn unsupported_reason_names_the_right_forge() {
        let forgejo = info(Flavor::Forgejo, "16.0.4");
        assert_eq!(
            unsupported_reason(&forgejo, Capability::RunRerun, "rerun a workflow run"),
            "Forgejo has no API route to rerun a workflow run"
        );
        let gitea = info(Flavor::Gitea, "1.27.3");
        let msg = unsupported_reason(&gitea, Capability::RunCancel, "cancel a workflow run");
        assert!(msg.contains("Gitea 1.27.3"), "{msg}");
        assert!(msg.contains("28.0"), "{msg}");
    }

    #[test]
    fn forgejo_version_url_swaps_the_api_segment() {
        let base = Url::parse("https://forge.example/api/v1/").unwrap();
        assert_eq!(
            forgejo_version_url(&base).unwrap().as_str(),
            "https://forge.example/api/forgejo/v1/version"
        );
        let mounted = Url::parse("https://example.com/git/api/v1/").unwrap();
        assert_eq!(
            forgejo_version_url(&mounted).unwrap().as_str(),
            "https://example.com/git/api/forgejo/v1/version"
        );
        let gateway = Url::parse("https://platform.example/gateway/abc/").unwrap();
        assert!(forgejo_version_url(&gateway).is_none());
    }

    #[test]
    fn flavor_round_trips_through_its_persisted_name() {
        for f in [Flavor::Forgejo, Flavor::Gitea, Flavor::Unidentified] {
            assert_eq!(Flavor::parse(f.as_str()), f);
        }
        assert_eq!(Flavor::parse("github"), Flavor::Unidentified);
    }
}
