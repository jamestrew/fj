//! `fj issue reaction`: emoji reactions on an issue or pull request body, or
//! on a comment (`--comment ID`). Every mutation re-reads the reactions and
//! prints the tally the forge now holds.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use crate::api;
use crate::api::issue_reaction::{Reaction, Target};
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct ReactionCmd {
    #[command(subcommand)]
    pub command: ReactionSub,
}

#[derive(Debug, Subcommand)]
pub enum ReactionSub {
    /// Show the per-emoji tally and who reacted.
    List(ListArgs),
    /// Add your reaction.
    Add(ChangeArgs),
    /// Remove your reaction.
    Remove(ChangeArgs),
    /// Add your reaction, or remove it if you already reacted with it.
    Toggle(ChangeArgs),
}

/// The reactions offered. The forge may be configured with a different set;
/// it refuses one it does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReactionArg {
    #[value(name = "+1", alias = "thumbsup")]
    PlusOne,
    #[value(name = "-1", alias = "thumbsdown")]
    MinusOne,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

/// Wire names in the order the tally shows them.
const OFFERED: [&str; 8] = [
    "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
];

impl ReactionArg {
    pub fn as_str(self) -> &'static str {
        match self {
            ReactionArg::PlusOne => "+1",
            ReactionArg::MinusOne => "-1",
            ReactionArg::Laugh => "laugh",
            ReactionArg::Hooray => "hooray",
            ReactionArg::Confused => "confused",
            ReactionArg::Heart => "heart",
            ReactionArg::Rocket => "rocket",
            ReactionArg::Eyes => "eyes",
        }
    }
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number (reactions on its body)
    pub number: Option<u64>,
    /// React to this comment id instead (ids come from `issue view --comments --json`)
    #[arg(long, value_name = "ID", conflicts_with = "number")]
    pub comment: Option<u64>,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ChangeArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Reaction: +1, -1, laugh, hooray, confused, heart, rocket, eyes
    #[arg(value_enum, allow_negative_numbers = true)]
    pub reaction: ReactionArg,
    /// Issue or pull request number (reacts to its body)
    pub number: Option<u64>,
    /// React to this comment id instead (ids come from `issue view --comments --json`)
    #[arg(long, value_name = "ID", conflicts_with = "number")]
    pub comment: Option<u64>,
}

/// Exactly one of the number or `--comment` names the target.
fn target(number: Option<u64>, comment: Option<u64>) -> Result<Target> {
    match (number, comment) {
        (Some(n), None) => Ok(Target::Issue(n)),
        (None, Some(id)) => Ok(Target::Comment(id)),
        _ => Err(anyhow!(
            "pass an issue or pull request number, or --comment <ID>"
        )),
    }
}

fn target_label(target: Target) -> String {
    match target {
        Target::Issue(n) => format!("#{n}"),
        Target::Comment(id) => format!("comment {id}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub content: String,
    pub count: usize,
    pub users: Vec<String>,
}

/// Group reactions per emoji: the offered set first in its fixed order, then
/// anything else the forge allows, by name. Emoji nobody used are left out.
fn tally(reactions: &[Reaction]) -> Vec<Tally> {
    let mut out: Vec<Tally> = Vec::new();
    for r in reactions {
        let user = r.user.as_ref().map(|u| u.login.clone());
        match out.iter_mut().find(|t| t.content == r.content) {
            Some(t) => {
                t.count += 1;
                t.users.extend(user);
            }
            None => out.push(Tally {
                content: r.content.clone(),
                count: 1,
                users: user.into_iter().collect(),
            }),
        }
    }
    let rank = |content: &str| {
        OFFERED
            .iter()
            .position(|o| *o == content)
            .unwrap_or(OFFERED.len())
    };
    out.sort_by(|a, b| {
        rank(&a.content)
            .cmp(&rank(&b.content))
            .then_with(|| a.content.cmp(&b.content))
    });
    out
}

/// Whether `login` already reacted with `content`.
fn has_reacted(reactions: &[Reaction], login: &str, content: &str) -> bool {
    reactions.iter().any(|r| {
        r.content == content
            && r.user
                .as_ref()
                .is_some_and(|u| u.login.eq_ignore_ascii_case(login))
    })
}

fn tally_table(tallies: &[Tally]) -> String {
    let rows: Vec<Vec<String>> = tallies
        .iter()
        .map(|t| {
            vec![
                t.content.clone(),
                t.count.to_string(),
                output::dim(&t.users.join(", ")),
            ]
        })
        .collect();
    output::render_table(&["REACTION", "COUNT", "USERS"], &rows)
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: ReactionCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        ReactionSub::List(args) => list(args, host).await,
        ReactionSub::Add(args) => change(args, host, Change::Add).await,
        ReactionSub::Remove(args) => change(args, host, Change::Remove).await,
        ReactionSub::Toggle(args) => change(args, host, Change::Toggle).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    Remove,
    Toggle,
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let target = target(args.number, args.comment)?;
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let reactions = api::issue_reaction::list(&ctx.client, &ctx.owner, &ctx.name, target).await?;
    let tallies = tally(&reactions);
    if args.json {
        return output::print_json(&serde_json::json!({
            "total_count": reactions.len(),
            "tally": tallies,
            "items": reactions,
        }));
    }
    if tallies.is_empty() {
        println!("(no reactions)");
        return Ok(());
    }
    print!("{}", tally_table(&tallies));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn change(args: ChangeArgs, host: Option<&str>, change: Change) -> Result<()> {
    let target = target(args.number, args.comment)?;
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let content = args.reaction.as_str();
    let add = match change {
        Change::Add => true,
        Change::Remove => false,
        Change::Toggle => {
            let me = api::user::current(&ctx.client).await?;
            let current =
                api::issue_reaction::list(&ctx.client, &ctx.owner, &ctx.name, target).await?;
            !has_reacted(&current, &me.login, content)
        }
    };
    let label = target_label(target);
    if add {
        let r =
            api::issue_reaction::add(&ctx.client, &ctx.owner, &ctx.name, target, content).await?;
        println!("✓ Reacted {} on {label}", r.content);
    } else {
        api::issue_reaction::remove(&ctx.client, &ctx.owner, &ctx.name, target, content).await?;
        println!("✓ Removed your {content} reaction from {label}");
    }
    print_current(&ctx, target).await
}

/// Print the tally as the forge holds it after a change.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn print_current(ctx: &RepoContext, target: Target) -> Result<()> {
    let reactions = api::issue_reaction::list(&ctx.client, &ctx.owner, &ctx.name, target).await?;
    let tallies = tally(&reactions);
    if tallies.is_empty() {
        println!("(no reactions)");
    } else {
        print!("{}", tally_table(&tallies));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::issue::{IssueCmd, IssueSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn reaction(content: &str, login: &str) -> Reaction {
        serde_json::from_value(serde_json::json!({
            "content": content,
            "user": { "id": 1, "login": login },
            "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn parse(argv: &[&str]) -> ReactionSub {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Issue(IssueCmd {
                command: IssueSub::Reaction(cmd),
            }) => cmd.command,
            other => panic!("expected issue reaction, got {other:?}"),
        }
    }

    #[test]
    fn every_offered_reaction_parses_to_its_wire_name() {
        for name in OFFERED {
            match parse(&["fj", "issue", "reaction", "add", name, "12"]) {
                ReactionSub::Add(a) => {
                    assert_eq!(a.reaction.as_str(), name);
                    assert_eq!(a.number, Some(12));
                }
                other => panic!("expected add, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_reaction_is_rejected() {
        assert!(Cli::try_parse_from(["fj", "issue", "reaction", "add", "tada", "12"]).is_err());
    }

    #[test]
    fn comment_flag_targets_a_comment_and_excludes_a_number() {
        match parse(&["fj", "issue", "reaction", "toggle", "-1", "--comment", "55"]) {
            ReactionSub::Toggle(a) => {
                assert_eq!(a.reaction, ReactionArg::MinusOne);
                assert_eq!(target(a.number, a.comment).unwrap(), Target::Comment(55));
            }
            other => panic!("expected toggle, got {other:?}"),
        }
        assert!(Cli::try_parse_from([
            "fj",
            "issue",
            "reaction",
            "add",
            "heart",
            "12",
            "--comment",
            "55"
        ])
        .is_err());
    }

    #[test]
    fn target_requires_a_number_or_a_comment() {
        assert_eq!(target(Some(3), None).unwrap(), Target::Issue(3));
        assert!(target(None, None).is_err());
        assert_eq!(target_label(Target::Issue(3)), "#3");
        assert_eq!(target_label(Target::Comment(9)), "comment 9");
    }

    #[test]
    fn tally_counts_per_emoji_in_offered_order() {
        let reactions = [
            reaction("rocket", "alice"),
            reaction("+1", "bob"),
            reaction("zebra", "carol"),
            reaction("+1", "alice"),
        ];
        let t = tally(&reactions);
        assert_eq!(
            t.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            ["+1", "rocket", "zebra"]
        );
        assert_eq!(t[0].count, 2);
        assert_eq!(t[0].users, ["bob", "alice"]);
        assert!(tally(&[]).is_empty());
        assert!(tally_table(&t).contains("rocket"));
    }

    #[test]
    fn has_reacted_matches_own_login_and_emoji_only() {
        let reactions = [reaction("heart", "Alice"), reaction("eyes", "bob")];
        assert!(has_reacted(&reactions, "alice", "heart"));
        assert!(!has_reacted(&reactions, "alice", "eyes"));
        assert!(!has_reacted(&reactions, "carol", "heart"));
    }
}
