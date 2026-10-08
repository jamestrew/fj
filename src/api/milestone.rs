use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub state: String,
    #[serde(default)]
    pub open_issues: u64,
    #[serde(default)]
    pub closed_issues: u64,
    #[serde(default)]
    pub due_on: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
}

pub async fn list(client: &Client, owner: &str, name: &str, state: &str) -> Result<Vec<Milestone>> {
    let path = format!("{}/milestones", crate::api::repo_path(owner, name));
    let q = vec![("state".into(), state.into())];
    client.json(Method::GET, &path, &q, None::<&()>).await
}

pub async fn get(client: &Client, owner: &str, name: &str, id: u64) -> Result<Milestone> {
    let path = format!("{}/milestones/{id}", crate::api::repo_path(owner, name));
    client
        .json(Method::GET, &path, &[], None::<&()>)
        .await
        .map_err(|e| crate::client::with_not_found_context(e, format!("milestone #{id} not found")))
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateMilestone<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<&'a str>,
}

pub async fn create(
    client: &Client,
    owner: &str,
    name: &str,
    body: &CreateMilestone<'_>,
) -> Result<Milestone> {
    let path = format!("{}/milestones", crate::api::repo_path(owner, name));
    client.json(Method::POST, &path, &[], Some(body)).await
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct EditMilestone<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<&'a str>,
}

pub async fn edit(
    client: &Client,
    owner: &str,
    name: &str,
    id: u64,
    body: &EditMilestone<'_>,
) -> Result<Milestone> {
    let path = format!("{}/milestones/{id}", crate::api::repo_path(owner, name));
    client.json(Method::PATCH, &path, &[], Some(body)).await
}

pub async fn delete(client: &Client, owner: &str, name: &str, id: u64) -> Result<()> {
    let path = format!("{}/milestones/{id}", crate::api::repo_path(owner, name));
    let res = client.request(Method::DELETE, &path, &[], None).await?;
    res.error_for_status()?;
    Ok(())
}

/// Assign an existing issue/PR to a milestone (or pass `None` to detach).
pub async fn assign(
    client: &Client,
    owner: &str,
    name: &str,
    issue_number: u64,
    milestone_id: Option<u64>,
) -> Result<()> {
    let path = format!(
        "{}/issues/{issue_number}",
        crate::api::repo_path(owner, name)
    );
    let body = match milestone_id {
        Some(id) => serde_json::json!({ "milestone": id }),
        None => serde_json::json!({ "milestone": null }),
    };
    let res = client
        .request(Method::PATCH, &path, &[], Some(&body))
        .await?;
    res.error_for_status()?;
    Ok(())
}

/// Pick the milestone a user named, by title first and by id second (the same
/// order the forge uses for the `milestones` list filter). `Ok(None)` means
/// nothing in `milestones` matched. A title shared by several milestones is an
/// error naming their ids, since guessing would file the issue in the wrong one.
pub fn pick<'a>(milestones: &'a [Milestone], wanted: &str) -> Result<Option<&'a Milestone>> {
    let wanted = wanted.trim();
    let by_title: Vec<&Milestone> = milestones.iter().filter(|m| m.title == wanted).collect();
    match by_title.as_slice() {
        [one] => return Ok(Some(one)),
        [] => {}
        many => {
            let ids = many
                .iter()
                .map(|m| m.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("several milestones are titled '{wanted}' (ids {ids}); pass the id");
        }
    }
    Ok(wanted
        .parse::<u64>()
        .ok()
        .and_then(|id| milestones.iter().find(|m| m.id == id)))
}

/// Resolve a milestone title or id to the milestone the forge holds. An id
/// that is not on the first list page is still looked up directly.
pub async fn resolve(client: &Client, owner: &str, name: &str, wanted: &str) -> Result<Milestone> {
    let milestones = list(client, owner, name, "all").await?;
    if let Some(found) = pick(&milestones, wanted)? {
        return Ok(found.clone());
    }
    if let Ok(id) = wanted.trim().parse::<u64>() {
        return get(client, owner, name, id).await;
    }
    Err(anyhow::anyhow!(
        "no milestone named '{}' in {owner}/{name} (see `fj milestone list`)",
        wanted.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn milestone(id: u64, title: &str) -> Milestone {
        Milestone {
            id,
            title: title.to_string(),
            description: String::new(),
            state: "open".to_string(),
            open_issues: 0,
            closed_issues: 0,
            due_on: None,
            created_at: None,
            closed_at: None,
        }
    }

    #[test]
    fn pick_prefers_title_then_falls_back_to_id() {
        let all = [milestone(7, "v1"), milestone(8, "7"), milestone(9, "v2")];
        assert_eq!(pick(&all, "v1").unwrap().unwrap().id, 7);
        // A milestone literally titled "7" wins over the milestone with id 7.
        assert_eq!(pick(&all, "7").unwrap().unwrap().id, 8);
        assert_eq!(pick(&all, " 9 ").unwrap().unwrap().id, 9);
        assert!(pick(&all, "nope").unwrap().is_none());
        assert!(pick(&all, "42").unwrap().is_none());
    }

    #[test]
    fn pick_refuses_an_ambiguous_title() {
        let all = [milestone(1, "v1"), milestone(2, "v1")];
        let err = pick(&all, "v1").unwrap_err().to_string();
        assert!(err.contains("ids 1, 2"), "{err}");
    }
}
