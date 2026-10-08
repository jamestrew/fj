//! `fj repo topics add|remove`: change single topics and print the list the
//! forge confirms afterwards.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};

/// Forgejo's limits on a topic and on a repository's topic set.
const MAX_TOPIC_LEN: usize = 35;
const MAX_TOPICS: usize = 25;

#[derive(Debug, Subcommand)]
pub enum TopicsSub {
    /// Add topics, keeping the ones already set, and print the confirmed list.
    Add(TopicEditArgs),
    /// Remove topics, keeping the rest, and print the confirmed list.
    Remove(TopicEditArgs),
}

#[derive(Debug, Args)]
pub struct TopicEditArgs {
    /// Topics to add or remove.
    #[arg(required = true, value_name = "TOPIC")]
    pub topics: Vec<String>,
    #[command(flatten)]
    pub r: RepoFlag,
}

/// Lowercase and validate one topic the way Forgejo does: it starts with a
/// letter or digit, continues with letters, digits, dashes or dots, and is at
/// most 35 characters.
pub(crate) fn normalize_topic(raw: &str) -> Result<String> {
    let topic = raw.trim().to_lowercase();
    if topic.is_empty() {
        return Err(anyhow!("a topic cannot be empty"));
    }
    if topic.chars().count() > MAX_TOPIC_LEN {
        return Err(anyhow!(
            "topic '{topic}' is longer than {MAX_TOPIC_LEN} characters"
        ));
    }
    let mut chars = topic.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    if !first_ok || !rest_ok {
        return Err(anyhow!(
            "topic '{topic}' is not valid: it must start with a letter or digit and contain only letters, digits, dashes and dots"
        ));
    }
    Ok(topic)
}

/// Normalize a list of topics, dropping repeats and keeping the given order.
pub(crate) fn normalize_topics(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        let topic = normalize_topic(r)?;
        if !out.contains(&topic) {
            out.push(topic);
        }
    }
    Ok(out)
}

/// The topics of `requested` that still have to be added to `current`.
/// Refuses a request that would exceed the forge's per-repository limit.
pub(crate) fn topics_to_add(current: &[String], requested: &[String]) -> Result<Vec<String>> {
    let new: Vec<String> = requested
        .iter()
        .filter(|t| !current.iter().any(|c| c.eq_ignore_ascii_case(t)))
        .cloned()
        .collect();
    let total = current.len() + new.len();
    if total > MAX_TOPICS {
        return Err(anyhow!(
            "a repository can have at most {MAX_TOPICS} topics; this would make {total}. Nothing was sent"
        ));
    }
    Ok(new)
}

/// Refuse to remove a topic the repository does not have.
pub(crate) fn check_removable(current: &[String], requested: &[String]) -> Result<()> {
    let missing: Vec<&str> = requested
        .iter()
        .filter(|t| !current.iter().any(|c| c.eq_ignore_ascii_case(t)))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "not a topic of this repository: {}. Nothing was sent",
            missing.join(", ")
        ))
    }
}

/// Check the list the forge returned against what the change asked for:
/// every topic of `expected_present` is there, none of `expected_absent` is.
pub(crate) fn check_confirmed(
    confirmed: &[String],
    expected_present: &[String],
    expected_absent: &[String],
) -> Result<()> {
    let has = |t: &String| confirmed.iter().any(|c| c.eq_ignore_ascii_case(t));
    let lost: Vec<&str> = expected_present
        .iter()
        .filter(|t| !has(t))
        .map(String::as_str)
        .collect();
    let kept: Vec<&str> = expected_absent
        .iter()
        .filter(|t| has(t))
        .map(String::as_str)
        .collect();
    if lost.is_empty() && kept.is_empty() {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !lost.is_empty() {
        parts.push(format!("not in the list: {}", lost.join(", ")));
    }
    if !kept.is_empty() {
        parts.push(format!("still in the list: {}", kept.join(", ")));
    }
    Err(anyhow!(
        "the forge accepted the change, but its topic list disagrees ({}). Confirmed topics: {}",
        parts.join("; "),
        topic_list(confirmed)
    ))
}

pub(crate) fn topic_list(topics: &[String]) -> String {
    if topics.is_empty() {
        "(none)".to_string()
    } else {
        topics.join(", ")
    }
}

/// Split `--set a,b,c` into normalized topics.
pub(crate) fn parse_set(raw: &str) -> Result<Vec<String>> {
    let parts: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let topics = normalize_topics(&parts)?;
    if topics.len() > MAX_TOPICS {
        return Err(anyhow!(
            "a repository can have at most {MAX_TOPICS} topics; {} were given. Nothing was sent",
            topics.len()
        ));
    }
    Ok(topics)
}

fn denied(slug: &str) -> String {
    format!("permission denied: changing the topics of {slug} needs write access to it")
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn run(sub: TopicsSub, host: Option<&str>) -> Result<()> {
    match sub {
        TopicsSub::Add(args) => add(args, host).await,
        TopicsSub::Remove(args) => remove(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn add(args: TopicEditArgs, host: Option<&str>) -> Result<()> {
    let requested = normalize_topics(&args.topics)?;
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let slug = ctx.slug();
    let current = api::repo::list_topics(&ctx.client, &ctx.owner, &ctx.name).await?;
    let new = topics_to_add(&current, &requested)?;
    for topic in &new {
        api::repo::add_topic(&ctx.client, &ctx.owner, &ctx.name, topic)
            .await
            .map_err(|e| api::org::with_forbidden_context(e, denied(&slug)))?;
    }
    let confirmed = api::repo::list_topics(&ctx.client, &ctx.owner, &ctx.name).await?;
    check_confirmed(&confirmed, &requested, &[])?;
    if new.is_empty() {
        println!("Already set, nothing to add.");
    } else {
        println!("✓ Added {} to {slug}", new.join(", "));
    }
    println!("Topics: {}", topic_list(&confirmed));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn remove(args: TopicEditArgs, host: Option<&str>) -> Result<()> {
    let requested = normalize_topics(&args.topics)?;
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let slug = ctx.slug();
    let current = api::repo::list_topics(&ctx.client, &ctx.owner, &ctx.name).await?;
    check_removable(&current, &requested)?;
    for topic in &requested {
        api::repo::remove_topic(&ctx.client, &ctx.owner, &ctx.name, topic)
            .await
            .map_err(|e| api::org::with_forbidden_context(e, denied(&slug)))?;
    }
    let confirmed = api::repo::list_topics(&ctx.client, &ctx.owner, &ctx.name).await?;
    check_confirmed(&confirmed, &[], &requested)?;
    println!("✓ Removed {} from {slug}", requested.join(", "));
    println!("Topics: {}", topic_list(&confirmed));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::repo::{RepoCmd, RepoSub, TopicsArgs};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn topics_args(argv: &[&str]) -> TopicsArgs {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Repo(RepoCmd {
                command: RepoSub::Topics(args),
            }) => args,
            other => panic!("expected repo topics, got {other:?}"),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn existing_invocations_still_parse() {
        let a = topics_args(&["fj", "repo", "topics", "owner/repo"]);
        assert!(a.command.is_none());
        assert_eq!(a.repo.as_deref(), Some("owner/repo"));
        assert!(a.set.is_none());

        let a = topics_args(&["fj", "repo", "topics", "-R", "owner/repo", "--set", "a,b"]);
        assert!(a.command.is_none());
        assert_eq!(a.repo_flag.as_deref(), Some("owner/repo"));
        assert_eq!(a.set.as_deref(), Some("a,b"));

        assert!(topics_args(&["fj", "repo", "topics"]).command.is_none());
    }

    #[test]
    fn add_and_remove_parse_topics_and_repo() {
        match topics_args(&["fj", "repo", "topics", "add", "cli", "rust", "-R", "o/r"]).command {
            Some(TopicsSub::Add(a)) => {
                assert_eq!(a.topics, strings(&["cli", "rust"]));
                assert_eq!(a.r.repo.as_deref(), Some("o/r"));
            }
            other => panic!("expected add, got {other:?}"),
        }
        match topics_args(&["fj", "repo", "topics", "remove", "cli"]).command {
            Some(TopicsSub::Remove(a)) => assert_eq!(a.topics, strings(&["cli"])),
            other => panic!("expected remove, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["fj", "repo", "topics", "add"]).is_err());
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(normalize_topic("  Rust ").unwrap(), "rust");
        assert_eq!(normalize_topic("web-3.0").unwrap(), "web-3.0");
        assert_eq!(normalize_topic("9lives").unwrap(), "9lives");
    }

    #[test]
    fn normalize_rejects_invalid_topics() {
        for bad in ["", "  ", "-lead", ".dot", "has space", "under_score", "ünï"] {
            assert!(normalize_topic(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(normalize_topic(&"a".repeat(35)).is_ok());
        let err = normalize_topic(&"a".repeat(36)).unwrap_err();
        assert!(err.to_string().contains("longer than 35"), "{err}");
    }

    #[test]
    fn normalize_topics_drops_repeats_in_order() {
        assert_eq!(
            normalize_topics(&strings(&["Rust", "cli", "rust"])).unwrap(),
            strings(&["rust", "cli"])
        );
        assert!(normalize_topics(&strings(&["ok", "not ok"])).is_err());
    }

    #[test]
    fn topics_to_add_skips_what_is_already_set() {
        let current = strings(&["cli", "rust"]);
        assert_eq!(
            topics_to_add(&current, &strings(&["rust", "forgejo"])).unwrap(),
            strings(&["forgejo"])
        );
        assert!(topics_to_add(&current, &strings(&["cli"]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn topics_to_add_refuses_past_the_limit() {
        let current: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
        let err = topics_to_add(&current, &strings(&["extra"])).unwrap_err();
        assert!(err.to_string().contains("at most 25"), "{err}");
        // Re-adding an existing topic at the limit changes nothing, so it is fine.
        assert!(topics_to_add(&current, &strings(&["t3"])).is_ok());
    }

    #[test]
    fn removing_an_absent_topic_is_refused() {
        let current = strings(&["cli", "rust"]);
        assert!(check_removable(&current, &strings(&["rust"])).is_ok());
        let err = check_removable(&current, &strings(&["rust", "go", "zig"])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("go, zig"), "{msg}");
        assert!(msg.contains("Nothing was sent"), "{msg}");
    }

    #[test]
    fn confirmed_list_must_match_the_change() {
        let confirmed = strings(&["cli", "rust"]);
        assert!(check_confirmed(&confirmed, &strings(&["rust"]), &strings(&["go"])).is_ok());
        let err = check_confirmed(&confirmed, &strings(&["go"]), &[]).unwrap_err();
        assert!(err.to_string().contains("not in the list: go"), "{err}");
        let err = check_confirmed(&confirmed, &[], &strings(&["cli"])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("still in the list: cli"), "{msg}");
        assert!(msg.contains("Confirmed topics: cli, rust"), "{msg}");
    }

    #[test]
    fn parse_set_splits_normalizes_and_allows_clearing() {
        assert_eq!(
            parse_set("Rust, cli,,rust").unwrap(),
            strings(&["rust", "cli"])
        );
        assert!(parse_set("").unwrap().is_empty());
        assert!(parse_set("ok,not ok").is_err());
        let many: Vec<String> = (0..26).map(|i| format!("t{i}")).collect();
        assert!(parse_set(&many.join(",")).is_err());
    }

    #[test]
    fn topic_list_renders_empty_as_none() {
        assert_eq!(topic_list(&[]), "(none)");
        assert_eq!(topic_list(&strings(&["a", "b"])), "a, b");
    }
}
