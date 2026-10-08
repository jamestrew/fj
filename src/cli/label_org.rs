//! Organization labels (`fj label <verb> --org ORG`), plus the pure
//! rendering helpers the repo label commands share.
//!
//! An organization's labels are their own collection on the forge
//! (`/orgs/{org}/labels`), shared by every repository the organization owns.
//! They are listed and managed apart from a repository's labels, and every
//! line printed here names which of the two it is about.

use anyhow::{anyhow, Result};

use crate::api::label::{self, CreateLabel, EditLabel, Label};
use crate::client::Client;
use crate::output;

/// Which collection a label lives in, for output wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner<'a> {
    Repo(&'a str, &'a str),
    Org(&'a str),
}

impl Owner<'_> {
    /// "label" for a repo, "organization label" for an org.
    fn noun(&self) -> &'static str {
        match self {
            Owner::Repo(..) => "label",
            Owner::Org(_) => "organization label",
        }
    }

    fn place(&self) -> String {
        match self {
            Owner::Repo(owner, name) => format!("{owner}/{name}"),
            Owner::Org(org) => format!("organization {org}"),
        }
    }
}

pub(crate) const LIST_HEADERS: [&str; 4] = ["NAME", "COLOR", "EXCLUSIVE GROUP", "DESCRIPTION"];

/// Table rows for a label list. The EXCLUSIVE GROUP cell names the group an
/// exclusive label belongs to, and is blank for an ordinary label.
pub(crate) fn label_rows(labels: &[Label]) -> Vec<Vec<String>> {
    labels
        .iter()
        .map(|l| {
            vec![
                l.name.clone(),
                format!("#{}", l.color),
                match (l.exclusive_scope(), l.exclusive) {
                    (Some(scope), _) => scope.to_string(),
                    (None, true) => "(flagged, no group)".to_string(),
                    (None, false) => String::new(),
                },
                output::truncate(&l.description, 60),
            ]
        })
        .collect()
}

/// `--exclusive` / `--no-exclusive` as the tri-state the API body wants:
/// `None` leaves the server's current value alone.
pub(crate) fn exclusive_flag(exclusive: bool, no_exclusive: bool) -> Option<bool> {
    match (exclusive, no_exclusive) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    }
}

pub(crate) fn normalize_color(color: &str) -> String {
    color.trim_start_matches('#').to_string()
}

/// The confirmation line after a create or edit, from the label the server
/// returned. States the exclusive group when the server put it in one.
pub(crate) fn saved_message(verb: &str, owner: Owner<'_>, l: &Label) -> String {
    let mut msg = format!(
        "✓ {verb} {} '{}' (#{}) in {}",
        owner.noun(),
        l.name,
        l.color,
        owner.place()
    );
    if let Some(scope) = l.exclusive_scope() {
        msg.push_str(&format!(
            "\n  Exclusive group '{scope}': an issue keeps one '{scope}/' label; adding this one replaces another."
        ));
    } else if l.exclusive {
        msg.push_str(
            "\n  Flagged exclusive, but the name has no 'group/name' form, so it belongs to no exclusive group.",
        );
    }
    msg
}

pub(crate) fn delete_prompt(owner: Owner<'_>, name: &str) -> String {
    format!(
        "Delete {} '{name}' from {}? Type the name to confirm",
        owner.noun(),
        owner.place()
    )
}

pub(crate) fn deleted_message(owner: Owner<'_>, name: &str) -> String {
    format!("✓ Deleted {} '{name}' from {}", owner.noun(), owner.place())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn list(org: &str, json: bool, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let items = label::list_org(&client, org).await?;
    if json {
        return output::print_json(&serde_json::to_value(&items)?);
    }
    if items.is_empty() {
        println!("(no organization labels in {org})");
        return Ok(());
    }
    println!("Organization labels for {org} (shared by its repositories):");
    print!(
        "{}",
        output::render_table(&LIST_HEADERS, &label_rows(&items))
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn create(
    org: &str,
    name: &str,
    color: &str,
    description: Option<&str>,
    exclusive: bool,
    host: Option<&str>,
) -> Result<()> {
    let client = Client::connect(host)?;
    let color = normalize_color(color);
    let l = label::create_org(
        &client,
        org,
        &CreateLabel {
            name,
            color: &color,
            description,
            exclusive: exclusive.then_some(true),
        },
    )
    .await?;
    println!("{}", saved_message("Created", Owner::Org(org), &l));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn edit(
    org: &str,
    name: &str,
    patch: &EditLabel<'_>,
    host: Option<&str>,
) -> Result<()> {
    let client = Client::connect(host)?;
    let existing = label::get_org_by_name(&client, org, name).await?;
    let l = label::edit_org(&client, org, existing.id, patch).await?;
    println!("{}", saved_message("Updated", Owner::Org(org), &l));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn delete(org: &str, name: &str, yes: bool, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let existing = label::get_org_by_name(&client, org, name).await?;
    if !yes {
        let answer = crate::cli::editor::prompt_line(&delete_prompt(Owner::Org(org), name))?;
        if answer != name {
            return Err(anyhow!("aborted"));
        }
    }
    label::delete_org(&client, org, existing.id).await?;
    println!("{}", deleted_message(Owner::Org(org), name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn label(name: &str, exclusive: bool) -> Label {
        Label {
            id: 1,
            name: name.into(),
            color: "ee0000".into(),
            description: "d".into(),
            exclusive,
            is_archived: false,
        }
    }

    #[test]
    fn rows_show_the_exclusive_group() {
        let rows = label_rows(&[
            label("priority/high", true),
            label("priority/low", false),
            label("bug", true),
        ]);
        assert_eq!(rows[0], vec!["priority/high", "#ee0000", "priority", "d"]);
        assert_eq!(rows[1][2], "");
        assert_eq!(rows[2][2], "(flagged, no group)");
    }

    #[test]
    fn messages_say_which_collection_the_label_is_in() {
        let org = saved_message("Created", Owner::Org("team"), &label("bug", false));
        assert_eq!(
            org,
            "✓ Created organization label 'bug' (#ee0000) in organization team"
        );
        let repo = saved_message("Created", Owner::Repo("o", "r"), &label("bug", false));
        assert_eq!(repo, "✓ Created label 'bug' (#ee0000) in o/r");
        assert_eq!(
            delete_prompt(Owner::Org("team"), "bug"),
            "Delete organization label 'bug' from organization team? Type the name to confirm"
        );
        assert_eq!(
            deleted_message(Owner::Repo("o", "r"), "bug"),
            "✓ Deleted label 'bug' from o/r"
        );
    }

    #[test]
    fn saved_message_explains_the_exclusive_group_from_the_server_label() {
        let msg = saved_message("Created", Owner::Org("team"), &label("priority/high", true));
        assert!(msg.contains("Exclusive group 'priority'"));
        assert!(msg.contains("replaces another"));
        let flagged = saved_message("Updated", Owner::Repo("o", "r"), &label("bug", true));
        assert!(flagged.contains("belongs to no exclusive group"));
        // The request asked for exclusive, the server did not store it: the
        // message must not claim a group.
        let plain = saved_message(
            "Created",
            Owner::Org("team"),
            &label("priority/high", false),
        );
        assert!(!plain.contains("xclusive"));
    }

    #[test]
    fn exclusive_flag_is_tri_state() {
        assert_eq!(exclusive_flag(true, false), Some(true));
        assert_eq!(exclusive_flag(false, true), Some(false));
        assert_eq!(exclusive_flag(false, false), None);
    }

    #[test]
    fn color_drops_a_leading_hash() {
        assert_eq!(normalize_color("#d73a4a"), "d73a4a");
        assert_eq!(normalize_color("d73a4a"), "d73a4a");
    }

    #[test]
    fn org_flag_parses_on_every_verb_and_conflicts_with_repo() {
        for argv in [
            vec!["fj", "label", "list", "--org", "team"],
            vec![
                "fj",
                "label",
                "create",
                "bug",
                "--org",
                "team",
                "--label-color",
                "#d73a4a",
                "--exclusive",
            ],
            vec![
                "fj",
                "label",
                "edit",
                "bug",
                "--org",
                "team",
                "--label-color",
                "d73a4a",
                "--no-exclusive",
            ],
            // The global ANSI flag still parses beside the label's own color.
            vec!["fj", "--color", "never", "label", "create", "bug"],
            vec!["fj", "label", "delete", "bug", "--org", "team", "-y"],
        ] {
            assert!(
                crate::cli::Cli::try_parse_from(argv.clone()).is_ok(),
                "{argv:?}"
            );
        }
        assert!(crate::cli::Cli::try_parse_from([
            "fj", "label", "list", "--org", "team", "-R", "o/r"
        ])
        .is_err());
        assert!(crate::cli::Cli::try_parse_from([
            "fj",
            "label",
            "edit",
            "bug",
            "--exclusive",
            "--no-exclusive"
        ])
        .is_err());
    }
}
