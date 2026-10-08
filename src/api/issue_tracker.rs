//! What a repository says about its built-in issue tracker: whether
//! dependencies and time tracking are switched on, and what the signed-in
//! account may do. Shared by the dependency and time-tracking commands so both
//! can tell "feature off" apart from "permission denied" and "not found".

use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde::Deserialize;

use crate::client::{ApiError, Client};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InternalTracker {
    #[serde(default)]
    pub enable_time_tracker: Option<bool>,
    #[serde(default)]
    pub allow_only_contributors_to_track_time: Option<bool>,
    #[serde(default)]
    pub enable_issue_dependencies: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub push: bool,
}

/// The slice of `GET /repos/{owner}/{repo}` the tracking commands need.
/// `internal_tracker` is absent when the repo uses an external tracker or has
/// issues off; the forge then falls back to an instance default we cannot see,
/// so every accessor is tri-state (`None` means "the forge did not say").
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoTracker {
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub internal_tracker: Option<InternalTracker>,
    #[serde(default)]
    pub permissions: Option<Permissions>,
}

impl RepoTracker {
    pub fn dependencies_enabled(&self) -> Option<bool> {
        self.internal_tracker
            .as_ref()
            .and_then(|t| t.enable_issue_dependencies)
    }

    pub fn time_tracking_enabled(&self) -> Option<bool> {
        self.internal_tracker
            .as_ref()
            .and_then(|t| t.enable_time_tracker)
    }

    /// True when only contributors may track time and this account is not one.
    pub fn time_limited_to_contributors(&self) -> bool {
        let only = self
            .internal_tracker
            .as_ref()
            .and_then(|t| t.allow_only_contributors_to_track_time)
            .unwrap_or(false);
        let can_write = self.permissions.as_ref().is_some_and(|p| p.push || p.admin);
        only && !can_write
    }
}

pub async fn get(client: &Client, owner: &str, name: &str) -> Result<RepoTracker> {
    let path = crate::api::repo_path(owner, name);
    client.json(Method::GET, &path, &[], None::<&()>).await
}

/// The HTTP status behind an error, when it came from the forge.
pub fn status_of(err: &anyhow::Error) -> Option<StatusCode> {
    err.downcast_ref::<ApiError>().map(|e| e.status)
}

/// Whether issue or pull request `index` exists and is readable. Used to tell
/// a missing work item apart from the other things a 404 can mean here.
pub async fn work_item_exists(
    client: &Client,
    owner: &str,
    name: &str,
    index: u64,
) -> Result<bool> {
    let path = format!("{}/issues/{index}", crate::api::repo_path(owner, name));
    match client
        .json::<serde_json::Value, _>(Method::GET, &path, &[], None::<&()>)
        .await
    {
        Ok(_) => Ok(true),
        Err(err) if status_of(&err) == Some(StatusCode::NOT_FOUND) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_internal_tracker_is_unknown_not_disabled() {
        let repo: RepoTracker = serde_json::from_str(r#"{"archived":false}"#).unwrap();
        assert_eq!(repo.dependencies_enabled(), None);
        assert_eq!(repo.time_tracking_enabled(), None);
        assert!(!repo.time_limited_to_contributors());
    }

    #[test]
    fn reads_tracker_switches_and_contributor_limit() {
        let repo: RepoTracker = serde_json::from_str(
            r#"{"archived":true,
                "internal_tracker":{"enable_time_tracker":true,
                    "allow_only_contributors_to_track_time":true,
                    "enable_issue_dependencies":false},
                "permissions":{"admin":false,"push":false,"pull":true}}"#,
        )
        .unwrap();
        assert!(repo.archived);
        assert_eq!(repo.dependencies_enabled(), Some(false));
        assert_eq!(repo.time_tracking_enabled(), Some(true));
        assert!(repo.time_limited_to_contributors());

        let writer: RepoTracker = serde_json::from_str(
            r#"{"internal_tracker":{"allow_only_contributors_to_track_time":true},
                "permissions":{"push":true}}"#,
        )
        .unwrap();
        assert!(!writer.time_limited_to_contributors());
    }
}
