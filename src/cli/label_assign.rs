//! Adding labels to an issue or PR, and saying what the forge did with them.
//!
//! Forgejo's exclusive label groups (`scope/name` labels flagged
//! `exclusive`) allow one label per group on an issue. The rule is the
//! server's: fj sends the requested label ids unchanged and never removes a
//! sibling itself. What it adds is the explanation, computed only from the
//! label set the server confirms after the write, so a label the forge
//! displaced is reported as displaced and never silently lost.

use anyhow::Result;

use crate::api::issue::name_scope;
use crate::api::label::{self, Assignment, Label};
use crate::client::Client;

/// Resolve `names` (repo labels, then the owning org's), add them to issue or
/// PR `number`, and print what the forge changed beyond a plain add.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn add_names(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    names: &[String],
) -> Result<()> {
    let requested = label::resolve_names(client, owner, name, names).await?;
    add_resolved(client, owner, name, number, requested).await
}

/// As [`add_names`], for labels the caller already resolved.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn add_resolved(
    client: &Client,
    owner: &str,
    name: &str,
    number: u64,
    requested: Vec<Label>,
) -> Result<()> {
    let assignment = label::assign(client, owner, name, number, requested).await?;
    for line in explain(&assignment) {
        println!("{line}");
    }
    Ok(())
}

/// Lines for labels sent with a create request, where there is no earlier
/// set: the only thing to report is a requested label the forge did not keep
/// (two labels from one exclusive group were asked for at once).
pub(crate) fn explain_created(requested: &[Label], confirmed: &[Label]) -> Vec<String> {
    explain(&Assignment {
        before: Some(Vec::new()),
        requested: requested.to_vec(),
        after: confirmed.to_vec(),
    })
}

/// What the forge did beyond adding the requested labels. Empty when the
/// add was a plain add. Every statement is derived from `after`, the set the
/// server returned; `before` only supplies the names of what went missing.
pub(crate) fn explain(a: &Assignment) -> Vec<String> {
    let in_after = |id: u64| a.after.iter().any(|l| l.id == id);
    let removed: Vec<&Label> = a
        .before
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|l| !in_after(l.id))
        .collect();
    let mut explained: Vec<u64> = Vec::new();
    let mut seen: Vec<u64> = Vec::new();
    let mut lines = Vec::new();

    for wanted in &a.requested {
        if seen.contains(&wanted.id) {
            continue;
        }
        seen.push(wanted.id);
        match a.after.iter().find(|l| l.id == wanted.id) {
            // Applied. If the confirmed label is exclusive, any label that
            // left the same group in this write was displaced by it.
            Some(confirmed) => {
                let Some(scope) = confirmed.exclusive_scope() else {
                    continue;
                };
                let displaced: Vec<&Label> = removed
                    .iter()
                    .copied()
                    .filter(|l| name_scope(&l.name) == Some(scope) && !explained.contains(&l.id))
                    .collect();
                if displaced.is_empty() {
                    continue;
                }
                explained.extend(displaced.iter().map(|l| l.id));
                lines.push(format!(
                    "Replaced {} with '{}': '{scope}' is an exclusive group, so the forge keeps one label from it.",
                    quoted(&displaced),
                    confirmed.name
                ));
            }
            // Asked for, but not in the confirmed set.
            None => {
                let kept = wanted.exclusive_scope().and_then(|scope| {
                    a.after
                        .iter()
                        .find(|l| l.exclusive_scope() == Some(scope))
                        .map(|l| (scope, l))
                });
                lines.push(match kept {
                    Some((scope, kept)) => format!(
                        "'{}' was not applied: the forge kept '{}' from exclusive group '{scope}'.",
                        wanted.name, kept.name
                    ),
                    None => format!(
                        "'{}' was not applied: the forge did not return it in the confirmed labels.",
                        wanted.name
                    ),
                });
            }
        }
    }

    let unexplained: Vec<&Label> = removed
        .iter()
        .copied()
        .filter(|l| !explained.contains(&l.id))
        .collect();
    if !unexplained.is_empty() {
        lines.push(format!("The forge also removed {}.", quoted(&unexplained)));
    }
    if !lines.is_empty() {
        lines.push(format!("Labels now: {}", names_or_none(&a.after)));
    }
    lines
}

fn quoted(labels: &[&Label]) -> String {
    labels
        .iter()
        .map(|l| format!("'{}'", l.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn names_or_none(labels: &[Label]) -> String {
    if labels.is_empty() {
        return "(none)".to_string();
    }
    labels
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: u64, name: &str, exclusive: bool) -> Label {
        Label {
            id,
            name: name.into(),
            color: "ededed".into(),
            description: String::new(),
            exclusive,
            is_archived: false,
        }
    }

    #[test]
    fn exclusive_scope_follows_the_forgejo_rule() {
        assert_eq!(
            label(1, "priority/high", true).exclusive_scope(),
            Some("priority")
        );
        // The group is everything before the LAST slash.
        assert_eq!(
            label(1, "team/priority/high", true).exclusive_scope(),
            Some("team/priority")
        );
        // Not flagged exclusive: a slash alone does not make a group.
        assert_eq!(label(1, "priority/high", false).exclusive_scope(), None);
        // Flagged, but the name has no usable scope.
        assert_eq!(label(1, "bug", true).exclusive_scope(), None);
        assert_eq!(label(1, "/bug", true).exclusive_scope(), None);
        assert_eq!(label(1, "bug/", true).exclusive_scope(), None);
    }

    #[test]
    fn plain_add_needs_no_explanation() {
        let a = Assignment {
            before: None,
            requested: vec![label(1, "bug", false)],
            after: vec![label(1, "bug", false), label(2, "docs", false)],
        };
        assert!(explain(&a).is_empty());
    }

    #[test]
    fn exclusive_add_explains_the_sibling_the_forge_replaced() {
        let a = Assignment {
            before: Some(vec![
                label(1, "bug", false),
                label(10, "priority/low", true),
            ]),
            requested: vec![label(11, "priority/high", true)],
            // Forge-confirmed set: low is gone, high is on, bug untouched.
            after: vec![label(1, "bug", false), label(11, "priority/high", true)],
        };
        assert_eq!(
            explain(&a),
            vec![
                "Replaced 'priority/low' with 'priority/high': 'priority' is an exclusive group, so the forge keeps one label from it.".to_string(),
                "Labels now: bug, priority/high".to_string(),
            ]
        );
    }

    #[test]
    fn exclusive_add_with_no_sibling_present_is_a_plain_add() {
        let a = Assignment {
            before: Some(vec![label(1, "bug", false)]),
            requested: vec![label(11, "priority/high", true)],
            after: vec![label(1, "bug", false), label(11, "priority/high", true)],
        };
        assert!(explain(&a).is_empty());
    }

    #[test]
    fn replacement_is_only_claimed_when_the_confirmed_set_shows_it() {
        // The requested label is exclusive and a sibling was on the issue,
        // but the server kept both (an older forge without the rule). Do not
        // claim a replacement that did not happen.
        let a = Assignment {
            before: Some(vec![label(10, "priority/low", true)]),
            requested: vec![label(11, "priority/high", true)],
            after: vec![
                label(10, "priority/low", true),
                label(11, "priority/high", true),
            ],
        };
        assert!(explain(&a).is_empty());
    }

    #[test]
    fn a_label_from_another_group_is_not_blamed_on_the_exclusive_add() {
        let a = Assignment {
            before: Some(vec![
                label(10, "priority/low", true),
                label(20, "kind/bug", true),
            ]),
            requested: vec![label(11, "priority/high", true)],
            after: vec![label(11, "priority/high", true)],
        };
        let lines = explain(&a);
        assert!(lines[0].starts_with("Replaced 'priority/low' with 'priority/high'"));
        assert_eq!(lines[1], "The forge also removed 'kind/bug'.");
        assert_eq!(lines[2], "Labels now: priority/high");
    }

    #[test]
    fn two_requests_from_one_group_report_the_one_the_forge_dropped() {
        let a = Assignment {
            before: Some(vec![]),
            requested: vec![
                label(10, "priority/low", true),
                label(11, "priority/high", true),
            ],
            after: vec![label(11, "priority/high", true)],
        };
        assert_eq!(
            explain(&a),
            vec![
                "'priority/low' was not applied: the forge kept 'priority/high' from exclusive group 'priority'.".to_string(),
                "Labels now: priority/high".to_string(),
            ]
        );
    }

    #[test]
    fn a_requested_label_missing_from_the_confirmed_set_is_reported() {
        let lines = explain_created(&[label(1, "bug", false)], &[]);
        assert_eq!(
            lines,
            vec![
                "'bug' was not applied: the forge did not return it in the confirmed labels."
                    .to_string(),
                "Labels now: (none)".to_string(),
            ]
        );
        // And nothing is said when the create kept every label.
        assert!(explain_created(&[label(1, "bug", false)], &[label(1, "bug", false)]).is_empty());
    }

    #[test]
    fn exclusivity_is_read_from_the_confirmed_label_not_the_request() {
        // The catalogue copy said non-exclusive; the server's confirmed copy
        // is exclusive and a sibling left. Trust the confirmed copy.
        let a = Assignment {
            before: Some(vec![label(10, "size/s", true)]),
            requested: vec![label(11, "size/l", false)],
            after: vec![label(11, "size/l", true)],
        };
        assert!(explain(&a)[0].starts_with("Replaced 'size/s' with 'size/l'"));
    }
}
