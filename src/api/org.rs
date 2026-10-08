use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::client::{ApiError, Client, Page};

use super::serde_util::deserialize_null_to_default;
use super::user::User;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub visibility: String,
    pub avatar_url: String,
}

pub async fn list_for_user(client: &Client) -> Result<Vec<Organization>> {
    client
        .json(Method::GET, "/api/v1/user/orgs", &[], None::<&()>)
        .await
}

pub async fn get(client: &Client, org: &str) -> Result<Organization> {
    let path = format!("/api/v1/orgs/{}", crate::api::path_segment(org));
    client.json(Method::GET, &path, &[], None::<&()>).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OrgVisibility {
    Public,
    Limited,
    Private,
}

impl OrgVisibility {
    pub fn as_api_value(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Limited => "limited",
            Self::Private => "private",
        }
    }
}

/// Body of `POST /orgs` (`CreateOrgOption`). Only `username` is required.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateOrg<'a> {
    pub username: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<&'a str>,
}

/// Forgejo caps user and organization names at 40 characters.
const MAX_ORG_NAME_LEN: usize = 40;

/// Check an organization name against Forgejo's username rules before any
/// request is sent: ASCII letters, digits, `-`, `_` and `.`; starts with a
/// letter or digit; no trailing or doubled separator. Reserved names are left
/// to the server, which is the only side that knows its reserved list.
pub fn validate_org_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("organization name is required"));
    }
    if name.chars().count() > MAX_ORG_NAME_LEN {
        return Err(anyhow!(
            "organization name is longer than {MAX_ORG_NAME_LEN} characters"
        ));
    }
    let is_sep = |c: char| matches!(c, '-' | '_' | '.');
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || is_sep(*c)))
    {
        return Err(anyhow!(
            "organization name may only contain letters, digits, '-', '_' and '.', got {bad:?}"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(anyhow!(
            "organization name must start with a letter or digit"
        ));
    }
    if name.ends_with(is_sep) {
        return Err(anyhow!(
            "organization name must not end with '-', '_' or '.'"
        ));
    }
    let chars: Vec<char> = name.chars().collect();
    if chars.windows(2).any(|w| is_sep(w[0]) && is_sep(w[1])) {
        return Err(anyhow!(
            "organization name must not contain consecutive '-', '_' or '.'"
        ));
    }
    Ok(())
}

/// Validate the optional `CreateOrgOption` fields that have a checkable shape.
pub fn validate_org_fields(email: Option<&str>, website: Option<&str>) -> Result<()> {
    if let Some(email) = email {
        let ok = email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty());
        if !ok || email.contains(char::is_whitespace) {
            return Err(anyhow!("'{email}' is not an email address"));
        }
    }
    if let Some(website) = website {
        if !(website.starts_with("http://") || website.starts_with("https://")) {
            return Err(anyhow!("website must be an http:// or https:// URL"));
        }
    }
    Ok(())
}

/// The headline for a failed `POST /orgs`. Forgejo answers 403 when the
/// account may not create organizations and 422 for a taken, reserved or
/// malformed name; each gets its own wording so the two are not confused.
pub fn explain_org_create_error(status: StatusCode, message: &str, name: &str) -> Option<String> {
    let message = message.trim();
    match status {
        StatusCode::FORBIDDEN => Some(if message.is_empty() {
            "permission denied: this account may not create organizations on this forge".to_string()
        } else {
            format!(
                "permission denied: this account may not create organizations on this forge ({message})"
            )
        }),
        StatusCode::UNPROCESSABLE_ENTITY | StatusCode::CONFLICT => {
            if message.to_lowercase().contains("already exist") {
                Some(format!(
                    "name conflict: '{name}' is already taken on this forge ({message})"
                ))
            } else if message.is_empty() {
                Some(format!("the forge rejected organization '{name}'"))
            } else {
                Some(format!(
                    "the forge rejected organization '{name}': {message}"
                ))
            }
        }
        _ => None,
    }
}

pub async fn create(client: &Client, body: &CreateOrg<'_>) -> Result<Organization> {
    client
        .json(Method::POST, "/api/v1/orgs", &[], Some(body))
        .await
        .map_err(|err| {
            let headline = err
                .downcast_ref::<ApiError>()
                .and_then(|e| explain_org_create_error(e.status, &e.message, body.username));
            match headline {
                Some(headline) => err.context(headline),
                None => err,
            }
        })
}

/// Add `context` as the headline when `err` is a 403 from the forge, so a
/// permission denial names the action and the access it needs.
pub fn with_forbidden_context(err: anyhow::Error, context: String) -> anyhow::Error {
    if err
        .downcast_ref::<ApiError>()
        .is_some_and(|e| e.status == StatusCode::FORBIDDEN)
    {
        err.context(context)
    } else {
        err
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub includes_all_repositories: bool,
    #[serde(default)]
    pub can_create_org_repo: bool,
    #[serde(default, deserialize_with = "deserialize_null_to_default")]
    pub units: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_null_to_default")]
    pub units_map: BTreeMap<String, String>,
}

/// Every team of an org. Follows pagination so a team past the first page is
/// still listed and still resolvable by name.
pub async fn list_teams(client: &Client, org: &str) -> Result<Vec<Team>> {
    let path = format!("/api/v1/orgs/{}/teams", crate::api::path_segment(org));
    Ok(client.get_all_page(&path, &[], None).await?.items)
}

/// Pick the team `wanted` names out of an org's teams: an exact name match
/// (case-insensitive, as Forgejo team names are), else a numeric team id.
pub fn pick_team<'a>(teams: &'a [Team], wanted: &str) -> Option<&'a Team> {
    teams
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            let id: u64 = wanted.parse().ok()?;
            teams.iter().find(|t| t.id == id)
        })
}

pub async fn find_team(client: &Client, org: &str, wanted: &str) -> Result<Team> {
    let teams = list_teams(client, org).await?;
    pick_team(&teams, wanted)
        .cloned()
        .ok_or_else(|| anyhow!("no team '{wanted}' in org {org}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TeamPermission {
    Read,
    Write,
    Admin,
}

impl TeamPermission {
    pub fn as_api_value(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

/// Body of `POST /orgs/{org}/teams` (`CreateTeamOption`). `units` grants each
/// listed unit at the team's `permission`; `units_map` sets a level per unit.
/// Forgejo needs one of the two for a read or write team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateTeam {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub permission: &'static str,
    pub includes_all_repositories: bool,
    pub can_create_org_repo: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub units: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub units_map: BTreeMap<String, String>,
}

/// Units a read or write team gets when neither `--units` nor
/// `--unit-permission` is passed.
pub const DEFAULT_TEAM_UNITS: &[&str] = &[
    "repo.code",
    "repo.issues",
    "repo.pulls",
    "repo.releases",
    "repo.wiki",
    "repo.projects",
    "repo.packages",
    "repo.actions",
];

const UNIT_LEVELS: &[&str] = &["none", "read", "write", "admin"];

fn validate_unit_name(unit: &str) -> Result<()> {
    let ok = unit.strip_prefix("repo.").is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    });
    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "'{unit}' is not a repository unit (expected a name like repo.code or repo.issues)"
        ))
    }
}

/// Resolve the unit flags into the `units` / `units_map` pair Forgejo accepts.
///
/// An admin team has admin access to every unit, so unit flags are refused
/// for it instead of being silently ignored by the server. A read or write
/// team with no unit flags gets [`DEFAULT_TEAM_UNITS`] at the team permission,
/// because Forgejo rejects such a team when both fields are empty.
pub fn resolve_team_units(
    permission: TeamPermission,
    units: &[String],
    unit_permissions: &[String],
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    if !units.is_empty() && !unit_permissions.is_empty() {
        return Err(anyhow!(
            "--units and --unit-permission are mutually exclusive"
        ));
    }
    if permission == TeamPermission::Admin {
        if !units.is_empty() || !unit_permissions.is_empty() {
            return Err(anyhow!(
                "an admin team has admin access to every unit; drop --units and --unit-permission"
            ));
        }
        return Ok((Vec::new(), BTreeMap::new()));
    }
    if !unit_permissions.is_empty() {
        let mut map = BTreeMap::new();
        for entry in unit_permissions {
            let (unit, level) = entry.split_once('=').ok_or_else(|| {
                anyhow!("expected --unit-permission <unit>=<level>, got '{entry}'")
            })?;
            let (unit, level) = (unit.trim(), level.trim());
            validate_unit_name(unit)?;
            if !UNIT_LEVELS.contains(&level) {
                return Err(anyhow!(
                    "'{level}' is not a unit access level (expected one of: {})",
                    UNIT_LEVELS.join(", ")
                ));
            }
            if map.insert(unit.to_string(), level.to_string()).is_some() {
                return Err(anyhow!("unit '{unit}' is given more than once"));
            }
        }
        return Ok((Vec::new(), map));
    }
    let list: Vec<String> = if units.is_empty() {
        DEFAULT_TEAM_UNITS.iter().map(|u| u.to_string()).collect()
    } else {
        let mut list: Vec<String> = Vec::new();
        for unit in units.iter().map(|u| u.trim()).filter(|u| !u.is_empty()) {
            validate_unit_name(unit)?;
            if !list.iter().any(|u| u == unit) {
                list.push(unit.to_string());
            }
        }
        if list.is_empty() {
            return Err(anyhow!("--units needs at least one unit"));
        }
        list
    };
    Ok((list, BTreeMap::new()))
}

pub async fn create_team(client: &Client, org: &str, body: &CreateTeam) -> Result<Team> {
    let path = format!("/api/v1/orgs/{}/teams", crate::api::path_segment(org));
    client.json(Method::POST, &path, &[], Some(body)).await
}

/// `OrganizationPermissions` for one user in one org.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrgPermissions {
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub can_write: bool,
    #[serde(default)]
    pub can_read: bool,
    #[serde(default)]
    pub can_create_repository: bool,
}

pub async fn user_permissions(client: &Client, user: &str, org: &str) -> Result<OrgPermissions> {
    let path = format!(
        "/api/v1/users/{}/orgs/{}/permissions",
        crate::api::path_segment(user),
        crate::api::path_segment(org)
    );
    client.json(Method::GET, &path, &[], None::<&()>).await
}

fn team_path(team_id: u64) -> String {
    format!("/api/v1/teams/{team_id}")
}

pub async fn list_team_members(client: &Client, team_id: u64, limit: u32) -> Result<Page<User>> {
    let path = format!("{}/members", team_path(team_id));
    client.get_all_page(&path, &[], Some(limit as usize)).await
}

/// 200 means a member, 404 means not; anything else is an error.
async fn exists(client: &Client, path: &str) -> Result<bool> {
    let res = client.request(Method::GET, path, &[], None).await?;
    match res.status() {
        StatusCode::NOT_FOUND => Ok(false),
        _ => crate::client::ensure_success(res).await.map(|_| true),
    }
}

fn team_member_path(team_id: u64, user: &str) -> String {
    format!(
        "{}/members/{}",
        team_path(team_id),
        crate::api::path_segment(user)
    )
}

pub async fn is_team_member(client: &Client, team_id: u64, user: &str) -> Result<bool> {
    exists(client, &team_member_path(team_id, user)).await
}

pub async fn add_team_member(client: &Client, team_id: u64, user: &str) -> Result<()> {
    client
        .send(
            Method::PUT,
            &team_member_path(team_id, user),
            &[],
            None::<&()>,
        )
        .await?;
    Ok(())
}

pub async fn remove_team_member(client: &Client, team_id: u64, user: &str) -> Result<()> {
    client
        .send(
            Method::DELETE,
            &team_member_path(team_id, user),
            &[],
            None::<&()>,
        )
        .await?;
    Ok(())
}

/// The slice of a `Repository` the team repository listing shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRepo {
    pub full_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub private: bool,
}

pub async fn list_team_repos(client: &Client, team_id: u64, limit: u32) -> Result<Page<TeamRepo>> {
    let path = format!("{}/repos", team_path(team_id));
    client.get_all_page(&path, &[], Some(limit as usize)).await
}

fn team_repo_path(team_id: u64, org: &str, repo: &str) -> String {
    format!(
        "{}/repos/{}/{}",
        team_path(team_id),
        crate::api::path_segment(org),
        crate::api::path_segment(repo)
    )
}

pub async fn team_has_repo(client: &Client, team_id: u64, org: &str, repo: &str) -> Result<bool> {
    exists(client, &team_repo_path(team_id, org, repo)).await
}

pub async fn add_team_repo(client: &Client, team_id: u64, org: &str, repo: &str) -> Result<()> {
    client
        .send(
            Method::PUT,
            &team_repo_path(team_id, org, repo),
            &[],
            None::<&()>,
        )
        .await?;
    Ok(())
}

pub async fn remove_team_repo(client: &Client, team_id: u64, org: &str, repo: &str) -> Result<()> {
    client
        .send(
            Method::DELETE,
            &team_repo_path(team_id, org, repo),
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
    fn org_name_accepts_forgejo_shaped_names() {
        for name in ["acme", "Acme-Corp", "a.b_c", "x1"] {
            validate_org_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn org_name_rejects_names_forgejo_would_refuse() {
        for name in ["", "-acme", "acme-", "ac--me", "ac me", "acmé", "a/b"] {
            assert!(
                validate_org_name(name).is_err(),
                "{name:?} should be refused"
            );
        }
        assert!(validate_org_name(&"a".repeat(41)).is_err());
        assert!(validate_org_name(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn org_fields_check_email_and_website_shape() {
        assert!(validate_org_fields(Some("ops@acme.test"), Some("https://acme.test")).is_ok());
        assert!(validate_org_fields(None, None).is_ok());
        assert!(validate_org_fields(Some("not-an-email"), None).is_err());
        assert!(validate_org_fields(Some("@acme.test"), None).is_err());
        assert!(validate_org_fields(None, Some("acme.test")).is_err());
    }

    #[test]
    fn org_create_error_separates_denial_from_conflict() {
        let denied = explain_org_create_error(StatusCode::FORBIDDEN, "", "acme").unwrap();
        assert!(denied.starts_with("permission denied"), "{denied}");
        let conflict = explain_org_create_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "user already exists [name: acme]",
            "acme",
        )
        .unwrap();
        assert!(conflict.starts_with("name conflict: 'acme'"), "{conflict}");
        let invalid =
            explain_org_create_error(StatusCode::UNPROCESSABLE_ENTITY, "name is reserved", "acme")
                .unwrap();
        assert!(
            invalid.contains("rejected") && invalid.contains("name is reserved"),
            "{invalid}"
        );
        assert!(explain_org_create_error(StatusCode::NOT_FOUND, "x", "acme").is_none());
    }

    fn team(id: u64, name: &str) -> Team {
        serde_json::from_value(serde_json::json!({ "id": id, "name": name })).unwrap()
    }

    #[test]
    fn pick_team_prefers_name_then_id() {
        let teams = vec![team(7, "Owners"), team(9, "7"), team(12, "devs")];
        assert_eq!(pick_team(&teams, "owners").unwrap().id, 7);
        // A team literally named "7" wins over the team whose id is 7.
        assert_eq!(pick_team(&teams, "7").unwrap().id, 9);
        assert_eq!(pick_team(&teams, "12").unwrap().name, "devs");
        assert!(pick_team(&teams, "nope").is_none());
    }

    #[test]
    fn team_decodes_null_units() {
        let t: Team = serde_json::from_value(serde_json::json!({
            "id": 1, "name": "devs", "units": null, "units_map": null
        }))
        .unwrap();
        assert!(t.units.is_empty() && t.units_map.is_empty());
    }

    #[test]
    fn team_units_default_for_a_read_team() {
        let (units, map) = resolve_team_units(TeamPermission::Read, &[], &[]).unwrap();
        assert_eq!(units.len(), DEFAULT_TEAM_UNITS.len());
        assert!(map.is_empty());
    }

    #[test]
    fn team_units_explicit_list_is_deduplicated() {
        let (units, _) = resolve_team_units(
            TeamPermission::Write,
            &[
                "repo.code".into(),
                " repo.issues".into(),
                "repo.code".into(),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(units, vec!["repo.code", "repo.issues"]);
    }

    #[test]
    fn team_units_map_parses_levels() {
        let (units, map) = resolve_team_units(
            TeamPermission::Read,
            &[],
            &["repo.code=read".into(), "repo.issues=write".into()],
        )
        .unwrap();
        assert!(units.is_empty());
        assert_eq!(map["repo.code"], "read");
        assert_eq!(map["repo.issues"], "write");
    }

    #[test]
    fn team_units_refuse_bad_input() {
        let read = TeamPermission::Read;
        assert!(resolve_team_units(read, &["code".into()], &[]).is_err());
        assert!(resolve_team_units(read, &[], &["repo.code".into()]).is_err());
        assert!(resolve_team_units(read, &[], &["repo.code=owner".into()]).is_err());
        assert!(resolve_team_units(
            read,
            &[],
            &["repo.code=read".into(), "repo.code=write".into()]
        )
        .is_err());
        assert!(
            resolve_team_units(read, &["repo.code".into()], &["repo.wiki=read".into()]).is_err()
        );
        assert!(resolve_team_units(TeamPermission::Admin, &["repo.code".into()], &[]).is_err());
        let (units, map) = resolve_team_units(TeamPermission::Admin, &[], &[]).unwrap();
        assert!(units.is_empty() && map.is_empty());
    }

    #[test]
    fn create_team_body_omits_empty_unit_fields() {
        let body = CreateTeam {
            name: "devs".into(),
            description: None,
            permission: "admin",
            includes_all_repositories: true,
            can_create_org_repo: false,
            units: Vec::new(),
            units_map: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "name": "devs",
                "permission": "admin",
                "includes_all_repositories": true,
                "can_create_org_repo": false
            })
        );
    }
}
