use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Signed: Forgejo uses negative sentinel ids for system actors
    /// (-1 Ghost/deleted, -2 the `forgejo-actions` bot), which appear as
    /// reviewers, assignees, posters, and authors.
    pub id: i64,
    pub login: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub is_admin: bool,
}

pub async fn current(client: &Client) -> Result<User> {
    client
        .json(Method::GET, "/api/v1/user", &[], None::<&()>)
        .await
}

/// The signed-in account's profile settings, as `/user/settings` reports
/// them. Fields are optional so a forge that omits one shows as unknown,
/// never as an empty string or `false` the server did not send. The same
/// shape is the `PATCH` body: only `Some` fields are serialized, so fields
/// the user did not change are not sent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pronouns: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_email: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_activity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_pronouns: Option<bool>,
}

pub async fn settings(client: &Client) -> Result<Settings> {
    client
        .json(Method::GET, "/api/v1/user/settings", &[], None::<&()>)
        .await
}

pub async fn edit_settings(client: &Client, body: &Settings) -> Result<Settings> {
    client
        .json(Method::PATCH, "/api/v1/user/settings", &[], Some(body))
        .await
}

/// One account email address. `verified` and `primary` stay `None` when the
/// server does not report them, so silence is never read as "verified".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Email {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
}

#[derive(Serialize)]
struct EmailsBody<'a> {
    emails: &'a [String],
}

pub async fn emails(client: &Client) -> Result<Vec<Email>> {
    Ok(client
        .get_page::<Email>("/api/v1/user/emails", &[])
        .await?
        .items)
}

/// Add addresses. Returns the addresses the server reports back.
pub async fn add_emails(client: &Client, addresses: &[String]) -> Result<Vec<Email>> {
    client
        .json(
            Method::POST,
            "/api/v1/user/emails",
            &[],
            Some(&EmailsBody { emails: addresses }),
        )
        .await
}

pub async fn delete_emails(client: &Client, addresses: &[String]) -> Result<()> {
    client
        .send(
            Method::DELETE,
            "/api/v1/user/emails",
            &[],
            Some(&EmailsBody { emails: addresses }),
        )
        .await?;
    Ok(())
}

/// Replace the avatar. `image_base64` is the base64 of the raw image bytes.
pub async fn update_avatar(client: &Client, image_base64: &str) -> Result<()> {
    client
        .send(
            Method::POST,
            "/api/v1/user/avatar",
            &[],
            Some(&serde_json::json!({ "image": image_base64 })),
        )
        .await?;
    Ok(())
}

pub async fn delete_avatar(client: &Client) -> Result<()> {
    client
        .send(Method::DELETE, "/api/v1/user/avatar", &[], None::<&()>)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::User;

    #[test]
    fn deserializes_negative_system_actor_id() {
        // Forgejo returns id -2 for the `forgejo-actions` bot (and -1 for
        // Ghost). These appear in requested_reviewers/assignees/poster, so the
        // User id must decode as signed or `fj pr list`/`view` fails wholesale.
        let user: User = serde_json::from_str(r#"{"id":-2,"login":"forgejo-actions"}"#).unwrap();
        assert_eq!(user.id, -2);
        assert_eq!(user.login, "forgejo-actions");
    }
}
