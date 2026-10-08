//! `fj issue dependency`: what an issue or pull request depends on and what
//! it blocks, as two separate lists. Forgejo keeps both on the shared issue
//! index, so the number may be a pull request's. Every list printed here is
//! what the forge answered with, never a local edit.

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use reqwest::StatusCode;

use crate::api;
use crate::api::issue_dependency::{Direction, IssueMeta, Related};
use crate::api::issue_tracker::{status_of, work_item_exists, RepoTracker};
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct DependencyCmd {
    #[command(subcommand)]
    pub command: DependencySub,
}

#[derive(Debug, Subcommand)]
pub enum DependencySub {
    /// List what the work item depends on and what it blocks.
    List(ListArgs),
    /// Link another work item (depends on it by default; --blocks reverses).
    Add(ChangeArgs),
    /// Unlink a work item from the list it is shown under.
    Remove(ChangeArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number
    pub number: u64,
    /// Maximum number of work items to fetch per list
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ChangeArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Issue or pull request number
    pub number: u64,
    /// The other work item: `12`, `#12`, or `owner/repo#12`
    pub reference: String,
    /// NUMBER blocks REFERENCE, instead of NUMBER depending on REFERENCE
    #[arg(long)]
    pub blocks: bool,
}

impl ChangeArgs {
    fn direction(&self) -> Direction {
        if self.blocks {
            Direction::Blocks
        } else {
            Direction::DependsOn
        }
    }
}

fn verb(direction: Direction) -> &'static str {
    match direction {
        Direction::DependsOn => "depends on",
        Direction::Blocks => "blocks",
    }
}

/// The verb after "does not" or "cannot".
fn bare(direction: Direction) -> &'static str {
    match direction {
        Direction::DependsOn => "depend on",
        Direction::Blocks => "block",
    }
}

fn opposite(direction: Direction) -> Direction {
    match direction {
        Direction::DependsOn => Direction::Blocks,
        Direction::Blocks => Direction::DependsOn,
    }
}

/// The work item a typed reference names: `12`, `#12`, `!12`, or
/// `owner/repo#12` for one in another repository. A bare number is in
/// `owner`/`repo`.
fn parse_reference(input: &str, owner: &str, repo: &str) -> Result<IssueMeta> {
    let invalid =
        || anyhow!("'{input}' is not a work item reference (use 12, #12, or owner/repo#12)");
    let text = input.trim();
    let (slug, number) = match text.rfind(['#', '!']) {
        Some(at) => (&text[..at], &text[at + 1..]),
        None => ("", text),
    };
    let index = number
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(invalid)?;
    if slug.is_empty() {
        return Ok(IssueMeta {
            owner: owner.to_string(),
            repo: repo.to_string(),
            index,
        });
    }
    let (o, r) = api::split_repo(slug).map_err(|_| invalid())?;
    Ok(IssueMeta {
        owner: o.to_string(),
        repo: r.to_string(),
        index,
    })
}

fn same_item(a: &IssueMeta, b: &IssueMeta) -> bool {
    a.index == b.index
        && a.owner.eq_ignore_ascii_case(&b.owner)
        && a.repo.eq_ignore_ascii_case(&b.repo)
}

/// `#12` inside `home`'s repository, `owner/repo#12` outside it.
fn label(item: &IssueMeta, home: &IssueMeta) -> String {
    if item.owner.eq_ignore_ascii_case(&home.owner) && item.repo.eq_ignore_ascii_case(&home.repo) {
        format!("#{}", item.index)
    } else {
        format!("{}/{}#{}", item.owner, item.repo, item.index)
    }
}

/// A listed work item as an `IssueMeta`. An entry without a repository is in
/// `home`'s.
fn meta_of(related: &Related, home: &IssueMeta) -> IssueMeta {
    let repository = related
        .repository
        .as_ref()
        .filter(|r| !r.owner.is_empty() && !r.name.is_empty());
    IssueMeta {
        owner: repository.map_or_else(|| home.owner.clone(), |r| r.owner.clone()),
        repo: repository.map_or_else(|| home.repo.clone(), |r| r.name.clone()),
        index: related.number,
    }
}

fn contains(list: &[Related], item: &IssueMeta, home: &IssueMeta) -> bool {
    list.iter().any(|r| same_item(&meta_of(r, home), item))
}

/// Refuse a self reference. Needs nothing from the forge, so it runs before
/// any request.
fn check_not_self(this: &IssueMeta, other: &IssueMeta) -> Result<()> {
    if same_item(this, other) {
        bail!(
            "#{} cannot depend on or block itself; nothing was sent",
            this.index
        );
    }
    Ok(())
}

/// Refuse a link that already exists in `direction`, or that would close a
/// loop with the opposite one, before any change is sent. `same` and `reverse`
/// are the lists the forge holds for `direction` and its opposite.
fn check_add(
    this: &IssueMeta,
    other: &IssueMeta,
    direction: Direction,
    same: &[Related],
    reverse: &[Related],
) -> Result<()> {
    let other_label = label(other, this);
    if contains(same, other, this) {
        bail!(
            "#{} already {} {other_label}; nothing was sent",
            this.index,
            verb(direction)
        );
    }
    if contains(reverse, other, this) {
        bail!(
            "#{} already {} {other_label}, so it cannot also {} it; nothing was sent",
            this.index,
            verb(opposite(direction)),
            bare(direction)
        );
    }
    Ok(())
}

/// Refuse to remove a link that is not in the list for `direction`, pointing
/// at the other list when it is there, so a removal never flips direction.
fn check_remove(
    this: &IssueMeta,
    other: &IssueMeta,
    direction: Direction,
    same: &[Related],
    reverse: &[Related],
) -> Result<()> {
    if contains(same, other, this) {
        return Ok(());
    }
    let other_label = label(other, this);
    if contains(reverse, other, this) {
        let hint = match direction {
            Direction::DependsOn => "pass --blocks to remove that",
            Direction::Blocks => "drop --blocks to remove that",
        };
        bail!(
            "#{} does not {} {other_label}; it {} it ({hint}). Nothing was sent",
            this.index,
            bare(direction),
            verb(opposite(direction))
        );
    }
    bail!(
        "#{} does not {} {other_label}; nothing was sent",
        this.index,
        bare(direction)
    )
}

/// Why the forge would not do it, as far as we can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refusal {
    /// The repository has dependencies switched off.
    Disabled,
    Archived,
    Denied,
    /// The forge answers 404 for both and did not say which.
    DisabledOrDenied,
    Missing(String),
}

impl Refusal {
    fn message(&self, repo: &str, number: u64, writing: bool) -> String {
        let action = if writing { "change" } else { "read" };
        match self {
            Refusal::Disabled => format!("dependencies are disabled on {repo}"),
            Refusal::Archived => {
                format!("{repo} is archived, so its dependencies are read-only")
            }
            Refusal::Denied => format!(
                "permission denied: your account may not {action} dependencies on {repo} #{number}"
            ),
            Refusal::DisabledOrDenied => format!(
                "the forge refused: dependencies are disabled on {repo}, or your account may not {action} them on #{number}"
            ),
            Refusal::Missing(item) => format!(
                "work item {item} not found (it does not exist, or your account cannot read it)"
            ),
        }
    }
}

/// What the repository itself rules out, before asking for anything else. A
/// forge that does not say whether dependencies are on is asked anyway.
fn preflight(tracker: &RepoTracker, writing: bool) -> Option<Refusal> {
    if tracker.dependencies_enabled() == Some(false) {
        return Some(Refusal::Disabled);
    }
    (writing && tracker.archived).then_some(Refusal::Archived)
}

/// Read a failure status. Forgejo answers 404 alike for a missing work item,
/// dependencies being off, and an account that may not write, so the caller
/// probes which work items exist first. `other` is `None` for a read.
fn classify(
    status: StatusCode,
    enabled: Option<bool>,
    this: (&str, bool),
    other: Option<(&str, bool)>,
) -> Option<Refusal> {
    match status {
        StatusCode::FORBIDDEN => Some(Refusal::Denied),
        StatusCode::LOCKED => Some(Refusal::Archived),
        StatusCode::NOT_FOUND => {
            if !this.1 {
                return Some(Refusal::Missing(this.0.to_string()));
            }
            match other {
                Some((item, false)) => Some(Refusal::Missing(item.to_string())),
                // Both exist, so the 404 is a refusal to write.
                Some(_) if enabled == Some(true) => Some(Refusal::Denied),
                Some(_) => Some(Refusal::DisabledOrDenied),
                // The work item is readable but its lists are not: off.
                None => Some(Refusal::Disabled),
            }
        }
        _ => None,
    }
}

fn section(title: &str, items: &[Related], home: &IssueMeta) -> String {
    let mut out = format!("{} ({})\n", output::bold(title), items.len());
    if items.is_empty() {
        out.push_str(&format!("  {}\n", output::dim("(none)")));
        return out;
    }
    for item in items {
        let kind = if item.pull_request.is_some() {
            "pr"
        } else {
            "issue"
        };
        out.push_str(&format!(
            "  {}  {}  {}  {}\n",
            label(&meta_of(item, home), home),
            output::dim(kind),
            output::state_pill(&item.state, false),
            output::truncate(&item.title, 60)
        ));
    }
    out
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: DependencyCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        DependencySub::List(args) => list(args, host).await,
        DependencySub::Add(args) => change(args, host, true).await,
        DependencySub::Remove(args) => change(args, host, false).await,
    }
}

fn home(ctx: &RepoContext, number: u64) -> IssueMeta {
    IssueMeta {
        owner: ctx.owner.clone(),
        repo: ctx.name.clone(),
        index: number,
    }
}

/// Turn a forge failure into the distinct message it stands for, keeping the
/// original error as the cause. Anything we cannot read is passed through.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn explain(
    ctx: &RepoContext,
    tracker: &RepoTracker,
    this: &IssueMeta,
    other: Option<&IssueMeta>,
    err: anyhow::Error,
) -> anyhow::Error {
    let Some(status) = status_of(&err) else {
        return err;
    };
    let mut this_exists = true;
    let mut other_exists = other.map(|_| true);
    if status == StatusCode::NOT_FOUND {
        match work_item_exists(&ctx.client, &this.owner, &this.repo, this.index).await {
            Ok(found) => this_exists = found,
            Err(_) => return err,
        }
        if let (true, Some(o)) = (this_exists, other) {
            match work_item_exists(&ctx.client, &o.owner, &o.repo, o.index).await {
                Ok(found) => other_exists = Some(found),
                Err(_) => return err,
            }
        }
    }
    let this_label = label(this, this);
    let other_label = other.map(|o| label(o, this));
    let refusal = classify(
        status,
        tracker.dependencies_enabled(),
        (&this_label, this_exists),
        other_label.as_deref().zip(other_exists),
    );
    match refusal {
        Some(r) => err.context(r.message(&ctx.slug(), this.index, other.is_some())),
        None => err,
    }
}

/// Both lists as the forge holds them: (depends on, blocks).
#[cfg_attr(coverage_nightly, coverage(off))]
async fn read_both(
    ctx: &RepoContext,
    number: u64,
    limit: Option<usize>,
) -> Result<(Vec<Related>, Vec<Related>)> {
    let read = |direction| {
        api::issue_dependency::list(&ctx.client, &ctx.owner, &ctx.name, number, direction, limit)
    };
    Ok((
        read(Direction::DependsOn).await?,
        read(Direction::Blocks).await?,
    ))
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let this = home(&ctx, args.number);
    let tracker = api::issue_tracker::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    if let Some(refusal) = preflight(&tracker, false) {
        bail!(refusal.message(&ctx.slug(), args.number, false));
    }
    let limit = Some(args.limit.max(1) as usize);
    let (depends_on, blocks) = match read_both(&ctx, args.number, limit).await {
        Ok(lists) => lists,
        Err(err) => return Err(explain(&ctx, &tracker, &this, None, err).await),
    };
    if args.json {
        return output::print_json(&serde_json::json!({
            "depends_on": depends_on,
            "blocks": blocks,
        }));
    }
    print!("{}", section("Depends on", &depends_on, &this));
    print!("{}", section("Blocks", &blocks, &this));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn change(args: ChangeArgs, host: Option<&str>, adding: bool) -> Result<()> {
    let direction = args.direction();
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let this = home(&ctx, args.number);
    let other = parse_reference(&args.reference, &ctx.owner, &ctx.name)?;
    check_not_self(&this, &other)?;

    let tracker = api::issue_tracker::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    if let Some(refusal) = preflight(&tracker, true) {
        bail!(refusal.message(&ctx.slug(), args.number, true));
    }
    let (depends_on, blocks) = match read_both(&ctx, args.number, None).await {
        Ok(lists) => lists,
        Err(err) => return Err(explain(&ctx, &tracker, &this, None, err).await),
    };
    let (same, reverse) = match direction {
        Direction::DependsOn => (&depends_on, &blocks),
        Direction::Blocks => (&blocks, &depends_on),
    };
    if adding {
        check_add(&this, &other, direction, same, reverse)?;
    } else {
        check_remove(&this, &other, direction, same, reverse)?;
    }

    let sent = if adding {
        api::issue_dependency::add(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            direction,
            &other,
        )
        .await
    } else {
        api::issue_dependency::remove(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            direction,
            &other,
        )
        .await
    };
    if let Err(err) = sent {
        return Err(explain(&ctx, &tracker, &this, Some(&other), err).await);
    }

    // Report what the forge now holds, not what was asked for.
    let (depends_on, blocks) = read_both(&ctx, args.number, None).await?;
    let now = match direction {
        Direction::DependsOn => &depends_on,
        Direction::Blocks => &blocks,
    };
    let other_label = label(&other, &this);
    let linked = contains(now, &other, &this);
    if linked != adding {
        bail!(
            "the forge accepted the request but #{} {} {} {other_label}",
            args.number,
            if linked { "still" } else { "does not" },
            if linked {
                verb(direction)
            } else {
                bare(direction)
            }
        );
    }
    if adding {
        println!("✓ #{} now {} {other_label}", args.number, verb(direction));
    } else {
        println!(
            "✓ #{} no longer {} {other_label}",
            args.number,
            verb(direction)
        );
    }
    print!("{}", section("Depends on", &depends_on, &this));
    print!("{}", section("Blocks", &blocks, &this));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::issue::{IssueCmd, IssueSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn this() -> IssueMeta {
        IssueMeta {
            owner: "o".into(),
            repo: "r".into(),
            index: 3,
        }
    }

    fn related(number: u64, repository: Option<(&str, &str)>) -> Related {
        let mut v = serde_json::json!({
            "number": number, "title": format!("item {number}"), "state": "open"
        });
        if let Some((owner, name)) = repository {
            v["repository"] = serde_json::json!({ "owner": owner, "name": name });
        }
        serde_json::from_value(v).unwrap()
    }

    fn parse(argv: &[&str]) -> DependencySub {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Issue(IssueCmd {
                command: IssueSub::Dependency(cmd),
            }) => cmd.command,
            other => panic!("expected issue dependency, got {other:?}"),
        }
    }

    #[test]
    fn add_defaults_to_depends_on_and_blocks_flag_reverses() {
        match parse(&["fj", "issue", "dependency", "add", "3", "#5"]) {
            DependencySub::Add(a) => {
                assert_eq!(a.direction(), Direction::DependsOn);
                assert_eq!(a.reference, "#5");
            }
            other => panic!("expected add, got {other:?}"),
        }
        match parse(&["fj", "issue", "dependency", "remove", "3", "5", "--blocks"]) {
            DependencySub::Remove(a) => assert_eq!(a.direction(), Direction::Blocks),
            other => panic!("expected remove, got {other:?}"),
        }
        match parse(&["fj", "issue", "dependency", "list", "3", "--json"]) {
            DependencySub::List(a) => assert!(a.json && a.limit == 30),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn parses_bare_hash_and_cross_repo_references() {
        for text in ["12", "#12", "!12", " #12 "] {
            assert_eq!(
                parse_reference(text, "o", "r").unwrap(),
                IssueMeta {
                    owner: "o".into(),
                    repo: "r".into(),
                    index: 12
                }
            );
        }
        assert_eq!(
            parse_reference("acme/widgets#7", "o", "r").unwrap(),
            IssueMeta {
                owner: "acme".into(),
                repo: "widgets".into(),
                index: 7
            }
        );
        for bad in ["", "abc", "#0", "#-1", "acme#7", "a/b/c#7", "12#"] {
            let err = parse_reference(bad, "o", "r").unwrap_err().to_string();
            assert!(err.contains("not a work item reference"), "{bad}: {err}");
        }
    }

    #[test]
    fn self_reference_is_refused_whatever_the_case() {
        let me = IssueMeta {
            owner: "O".into(),
            repo: "R".into(),
            index: 3,
        };
        let err = check_not_self(&this(), &me).unwrap_err().to_string();
        assert!(err.contains("itself") && err.contains("nothing was sent"));
        assert!(check_not_self(&this(), &parse_reference("4", "o", "r").unwrap()).is_ok());
        // Same number in another repository is a different work item.
        assert!(check_not_self(&this(), &parse_reference("x/y#3", "o", "r").unwrap()).is_ok());
    }

    #[test]
    fn duplicate_and_circular_links_are_refused_before_sending() {
        let five = parse_reference("5", "o", "r").unwrap();
        let listed = [related(5, Some(("o", "r")))];

        let dup = check_add(&this(), &five, Direction::DependsOn, &listed, &[])
            .unwrap_err()
            .to_string();
        assert_eq!(dup, "#3 already depends on #5; nothing was sent");

        let dup = check_add(&this(), &five, Direction::Blocks, &listed, &[])
            .unwrap_err()
            .to_string();
        assert_eq!(dup, "#3 already blocks #5; nothing was sent");

        let circular = check_add(&this(), &five, Direction::DependsOn, &[], &listed)
            .unwrap_err()
            .to_string();
        assert_eq!(
            circular,
            "#3 already blocks #5, so it cannot also depend on it; nothing was sent"
        );

        assert!(check_add(&this(), &five, Direction::DependsOn, &[], &[]).is_ok());
        // #5 in another repository is not the listed #5.
        let far = parse_reference("x/y#5", "o", "r").unwrap();
        assert!(check_add(&this(), &far, Direction::DependsOn, &listed, &[]).is_ok());
    }

    #[test]
    fn removal_only_applies_to_the_list_the_entry_is_under() {
        let five = parse_reference("5", "o", "r").unwrap();
        // An entry with no repository is in the home repository.
        let listed = [related(5, None)];

        assert!(check_remove(&this(), &five, Direction::DependsOn, &listed, &[]).is_ok());

        let wrong_way = check_remove(&this(), &five, Direction::DependsOn, &[], &listed)
            .unwrap_err()
            .to_string();
        assert!(
            wrong_way.contains("#3 does not depend on #5; it blocks it")
                && wrong_way.contains("pass --blocks"),
            "{wrong_way}"
        );
        let wrong_way = check_remove(&this(), &five, Direction::Blocks, &[], &listed)
            .unwrap_err()
            .to_string();
        assert!(
            wrong_way.contains("#3 does not block #5; it depends on it")
                && wrong_way.contains("drop --blocks"),
            "{wrong_way}"
        );

        let absent = check_remove(&this(), &five, Direction::Blocks, &[], &[])
            .unwrap_err()
            .to_string();
        assert_eq!(absent, "#3 does not block #5; nothing was sent");
    }

    #[test]
    fn preflight_reads_the_repository_switch_and_archive_state() {
        let repo = |json: &str| serde_json::from_str::<RepoTracker>(json).unwrap();
        let off = repo(r#"{"internal_tracker":{"enable_issue_dependencies":false}}"#);
        assert_eq!(preflight(&off, false), Some(Refusal::Disabled));
        assert_eq!(preflight(&off, true), Some(Refusal::Disabled));

        let archived =
            repo(r#"{"archived":true,"internal_tracker":{"enable_issue_dependencies":true}}"#);
        assert_eq!(preflight(&archived, false), None);
        assert_eq!(preflight(&archived, true), Some(Refusal::Archived));

        // A forge that does not say is asked anyway.
        assert_eq!(preflight(&repo("{}"), true), None);
    }

    #[test]
    fn classify_tells_the_failures_apart() {
        let here = ("#3", true);
        let other = Some(("#5", true));
        assert_eq!(
            classify(StatusCode::FORBIDDEN, Some(true), here, other),
            Some(Refusal::Denied)
        );
        assert_eq!(
            classify(StatusCode::LOCKED, Some(true), here, other),
            Some(Refusal::Archived)
        );
        assert_eq!(
            classify(StatusCode::NOT_FOUND, Some(true), ("#3", false), other),
            Some(Refusal::Missing("#3".into()))
        );
        assert_eq!(
            classify(StatusCode::NOT_FOUND, Some(true), here, Some(("#5", false))),
            Some(Refusal::Missing("#5".into()))
        );
        assert_eq!(
            classify(StatusCode::NOT_FOUND, Some(true), here, other),
            Some(Refusal::Denied)
        );
        assert_eq!(
            classify(StatusCode::NOT_FOUND, None, here, other),
            Some(Refusal::DisabledOrDenied)
        );
        // A read of an existing work item that 404s means the feature is off.
        assert_eq!(
            classify(StatusCode::NOT_FOUND, None, here, None),
            Some(Refusal::Disabled)
        );
        assert_eq!(
            classify(StatusCode::INTERNAL_SERVER_ERROR, None, here, other),
            None
        );
    }

    #[test]
    fn each_refusal_has_its_own_message() {
        let messages: Vec<String> = [
            Refusal::Disabled,
            Refusal::Archived,
            Refusal::Denied,
            Refusal::DisabledOrDenied,
            Refusal::Missing("#5".into()),
        ]
        .iter()
        .map(|r| r.message("o/r", 3, true))
        .collect();
        assert!(messages[0].starts_with("dependencies are disabled on o/r"));
        assert!(messages[1].contains("archived"));
        assert!(messages[2].starts_with("permission denied"));
        assert!(messages[4].starts_with("work item #5 not found"));
        let unique: std::collections::BTreeSet<&String> = messages.iter().collect();
        assert_eq!(unique.len(), messages.len());
        assert!(Refusal::Denied.message("o/r", 3, false).contains("read"));
    }

    #[test]
    fn section_labels_cross_repo_items_and_empty_lists() {
        let text = section(
            "Depends on",
            &[related(5, Some(("o", "r"))), related(9, Some(("x", "y")))],
            &this(),
        );
        assert!(text.contains("(2)") && text.contains("#5") && text.contains("x/y#9"));
        assert!(section("Blocks", &[], &this()).contains("(none)"));
    }
}
