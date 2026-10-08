//! Instance administration: forge-wide settings, scheduled tasks, user
//! creation, and unadopted repositories. Everything under `/admin` needs an
//! instance-administrator token; the server answers 403 otherwise.

use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::{ApiError, Client};

/// The global settings sections Forgejo exposes, as `(name, path)`.
pub const SETTINGS_SECTIONS: &[(&str, &str)] = &[
    ("api", "/api/v1/settings/api"),
    ("repository", "/api/v1/settings/repository"),
    ("ui", "/api/v1/settings/ui"),
    ("attachment", "/api/v1/settings/attachment"),
];

/// One settings section as the server reported it. Kept as raw JSON so a
/// field the server omits is absent, never a client-side default.
#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    Available(serde_json::Value),
    /// The forge does not serve this endpoint (HTTP status attached).
    Unavailable(u16),
}

/// Statuses that mean "this forge does not have the endpoint", as opposed to
/// a permission or transport failure.
pub fn is_unsupported(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    )
}

/// Map an "endpoint missing" failure to `None`; pass every other outcome on.
fn unsupported_status(err: &anyhow::Error) -> Option<u16> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .filter(|api| is_unsupported(api.status))
        .map(|api| api.status.as_u16())
}

pub async fn settings_section(client: &Client, path: &str) -> Result<Section> {
    match client
        .json::<serde_json::Value, _>(Method::GET, path, &[], None::<&()>)
        .await
    {
        Ok(value) => Ok(Section::Available(value)),
        Err(err) => match unsupported_status(&err) {
            Some(status) => Ok(Section::Unavailable(status)),
            None => Err(err),
        },
    }
}

/// A scheduled task. Every field but the name is optional so a forge that
/// omits one shows as unknown instead of a zero value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cron {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_times: Option<i64>,
}

/// List scheduled tasks. `Ok(None)` means the forge has no such endpoint.
pub async fn cron_list(client: &Client, limit: usize) -> Result<Option<Vec<Cron>>> {
    match client
        .get_all_page::<Cron>("/api/v1/admin/cron", &[], Some(limit))
        .await
    {
        Ok(page) => Ok(Some(page.items)),
        Err(err) if unsupported_status(&err).is_some() => Ok(None),
        Err(err) => Err(err),
    }
}

/// Body of `POST /admin/users`. `password` is deliberately not `Debug`
/// printable: the struct has a manual `Debug` that redacts it.
#[derive(Clone, Serialize)]
pub struct CreateUser<'a> {
    pub username: &'a str,
    pub email: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub must_change_password: Option<bool>,
    pub send_notify: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restricted: Option<bool>,
}

impl std::fmt::Debug for CreateUser<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateUser")
            .field("username", &self.username)
            .field("email", &self.email)
            .field("password", &self.password.map(|_| "[redacted]"))
            .field("must_change_password", &self.must_change_password)
            .field("send_notify", &self.send_notify)
            .finish_non_exhaustive()
    }
}

/// Create a user. Returns the server's account record as raw JSON with any
/// credential-shaped key removed, so no caller can print one by accident.
pub async fn create_user(client: &Client, body: &CreateUser<'_>) -> Result<serde_json::Value> {
    let mut created: serde_json::Value = client
        .json(Method::POST, "/api/v1/admin/users", &[], Some(body))
        .await?;
    strip_credentials(&mut created);
    Ok(created)
}

/// Drop credential-shaped keys from a server account record.
pub fn strip_credentials(value: &mut serde_json::Value) {
    if let Some(map) = value.as_object_mut() {
        map.retain(|key, _| {
            let k = key.to_ascii_lowercase();
            !(k.contains("password") || k.contains("passwd") || k.contains("token"))
        });
    }
}

#[derive(Debug, Clone, Default)]
pub struct UnadoptedQuery<'a> {
    pub pattern: Option<&'a str>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

/// List unadopted repositories as `owner/name` strings.
pub async fn unadopted_list(client: &Client, q: &UnadoptedQuery<'_>) -> Result<Vec<String>> {
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(pattern) = q.pattern {
        query.push(("pattern".into(), pattern.to_string()));
    }
    if let Some(page) = q.page {
        query.push(("page".into(), page.to_string()));
    }
    if let Some(limit) = q.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    Ok(client
        .get_page::<String>("/api/v1/admin/unadopted", &query)
        .await?
        .items)
}

fn unadopted_path(owner: &str, repo: &str) -> String {
    format!(
        "/api/v1/admin/unadopted/{}/{}",
        crate::api::path_segment(owner),
        crate::api::path_segment(repo)
    )
}

/// Adopt the on-disk files as a repository. Succeeds only on the server's 204.
pub async fn unadopted_adopt(client: &Client, owner: &str, repo: &str) -> Result<()> {
    client
        .send(Method::POST, &unadopted_path(owner, repo), &[], None::<&()>)
        .await?;
    Ok(())
}

/// Delete the on-disk files. Succeeds only on the server's 204.
pub async fn unadopted_delete(client: &Client, owner: &str, repo: &str) -> Result<()> {
    client
        .send(
            Method::DELETE,
            &unadopted_path(owner, repo),
            &[],
            None::<&()>,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_user_debug_never_shows_the_password() {
        let body = CreateUser {
            username: "alice",
            email: "a@example.com",
            password: Some("hunter2-secret"),
            must_change_password: Some(true),
            send_notify: false,
            full_name: None,
            login_name: None,
            source_id: None,
            visibility: None,
            restricted: None,
        };
        let shown = format!("{body:?}");
        assert!(!shown.contains("hunter2-secret"), "{shown}");
        assert!(shown.contains("[redacted]"), "{shown}");
    }

    #[test]
    fn create_user_omits_unset_optionals_but_always_states_notify() {
        let body = CreateUser {
            username: "alice",
            email: "a@example.com",
            password: None,
            must_change_password: None,
            send_notify: false,
            full_name: None,
            login_name: None,
            source_id: None,
            visibility: None,
            restricted: None,
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "username": "alice", "email": "a@example.com", "send_notify": false
            })
        );
    }

    #[test]
    fn strip_credentials_removes_secret_shaped_keys_only() {
        let mut v = serde_json::json!({
            "id": 7, "login": "alice", "password": "x", "Passwd": "y", "access_token": "z"
        });
        strip_credentials(&mut v);
        assert_eq!(v, serde_json::json!({"id": 7, "login": "alice"}));
    }

    #[test]
    fn unsupported_statuses_are_missing_endpoints_not_denials() {
        assert!(is_unsupported(StatusCode::NOT_FOUND));
        assert!(is_unsupported(StatusCode::METHOD_NOT_ALLOWED));
        assert!(is_unsupported(StatusCode::NOT_IMPLEMENTED));
        assert!(!is_unsupported(StatusCode::FORBIDDEN));
        assert!(!is_unsupported(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn unadopted_path_encodes_each_segment() {
        assert_eq!(
            unadopted_path("al ice", "re/po"),
            "/api/v1/admin/unadopted/al%20ice/re%2Fpo"
        );
    }
}
