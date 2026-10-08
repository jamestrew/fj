//! `fj repo transfer`: request an ownership transfer after a review step, and
//! find, inspect, accept or reject a pending one.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};

use crate::api;
use crate::api::repo::{OwnerKind, OwnerLookup, RepoTransfer, TransferRepo};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::editor;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct TransferCmd {
    #[command(subcommand)]
    pub command: TransferSub,
}

#[derive(Debug, Subcommand)]
pub enum TransferSub {
    /// Request a transfer to another owner, after reviewing who receives it.
    ///
    /// Looks the new owner up on the forge first, says whether it is a user
    /// or an organization, who has to accept, and the access the request
    /// needs. Nothing is sent until you confirm. An unknown owner, or the
    /// current owner, is refused without sending anything. The result is
    /// reported as the forge returned it: moved, or pending acceptance.
    Request(RequestArgs),
    /// List pending transfers waiting on you or one of your organizations.
    ///
    /// Forgejo has no endpoint that lists incoming transfers. A pending
    /// transfer only shows on the repository it applies to, so this reads
    /// every repository in one listing and keeps the ones marked pending:
    /// by default the repositories your account can access (which is where a
    /// repository offered to you appears), or with --owner the repositories
    /// of that user or organization that you can see. A repository offered to
    /// one of your organizations may be in neither listing unless you name
    /// its current owner with --owner. To check one repository directly, use
    /// `fj repo transfer view -R <owner>/<name>`.
    List(ListArgs),
    /// Show the pending transfer on one repository, if any.
    View(ViewArgs),
    /// Accept a pending transfer of a repository to you or your organization.
    Accept(RespondArgs),
    /// Reject a pending transfer of a repository to you or your organization.
    Reject(RespondArgs),
}

#[derive(Debug, Args)]
pub struct RequestArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// User or organization that should own the repository.
    pub new_owner: String,
    /// Team of the receiving organization to add to the repository.
    /// Repeatable. Only valid when the new owner is an organization.
    #[arg(long = "team-id", value_name = "ID")]
    pub team_ids: Vec<u64>,
    /// Skip the confirmation prompt (the review is still printed).
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Scan this user's or organization's repositories instead of your own.
    #[arg(long, value_name = "LOGIN")]
    pub owner: Option<String>,
    /// Include pending transfers that are not addressed to you or your orgs.
    #[arg(long)]
    pub all: bool,
    /// Maximum number of transfers to show
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Emit JSON instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RespondArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn transfer(cmd: TransferCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        TransferSub::Request(args) => request(args, host).await,
        TransferSub::List(args) => list(args, host).await,
        TransferSub::View(args) => view(args, host).await,
        TransferSub::Accept(args) => respond(args, host, true).await,
        TransferSub::Reject(args) => respond(args, host, false).await,
    }
}

fn login_or_unknown(user: Option<&api::user::User>) -> &str {
    user.map(|u| u.login.as_str()).unwrap_or("(unknown)")
}

/// Refuse a transfer that must not be sent: to the current owner, to an owner
/// the forge does not know, on top of one already pending, or with teams for
/// a user. `current_owner` is the owner as the forge spells it.
pub(crate) fn check_target(
    slug: &str,
    current_owner: &str,
    requested: &str,
    lookup: Option<&OwnerLookup>,
    pending: Option<&RepoTransfer>,
    team_ids: &[u64],
) -> Result<()> {
    if let Some(pending) = pending {
        return Err(anyhow!(
            "{slug} already has a pending transfer to {}; it has to be accepted or rejected first. Nothing was sent",
            login_or_unknown(pending.recipient.as_ref())
        ));
    }
    if requested.eq_ignore_ascii_case(current_owner) {
        return Err(anyhow!(
            "{current_owner} already owns {slug}. Nothing was sent"
        ));
    }
    let Some(lookup) = lookup else {
        return Err(anyhow!(
            "no user or organization named '{requested}' on this forge (an organization you are not allowed to see looks the same). Nothing was sent"
        ));
    };
    if lookup.login.eq_ignore_ascii_case(current_owner) {
        return Err(anyhow!(
            "{current_owner} already owns {slug}. Nothing was sent"
        ));
    }
    if !team_ids.is_empty() && lookup.kind != OwnerKind::Organization {
        return Err(anyhow!(
            "--team-id only applies when the new owner is an organization, and {} is a user. Nothing was sent",
            lookup.login
        ));
    }
    Ok(())
}

/// What the requester is allowed to do at the receiving end, as far as the
/// forge would say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Requester<'a> {
    pub login: &'a str,
    pub site_admin: bool,
    /// Whether the requester may create repositories in the receiving
    /// organization. `None` for a user target, or when it could not be read.
    pub can_create_in_org: Option<bool>,
}

/// Who has to accept before the repository moves.
pub(crate) fn who_must_accept(target: &OwnerLookup, me: Requester<'_>) -> String {
    let name = &target.login;
    if me.site_admin {
        return "nobody. You are a site administrator, so the forge moves the repository as soon as you submit.".to_string();
    }
    match target.kind {
        OwnerKind::User if name.eq_ignore_ascii_case(me.login) => {
            "nobody. You are the receiving owner, so the forge moves the repository as soon as you submit.".to_string()
        }
        OwnerKind::User => format!(
            "{name} has to accept. Until then the repository stays where it is, marked as pending."
        ),
        OwnerKind::Organization => match me.can_create_in_org {
            Some(true) => format!(
                "nobody. You may create repositories in {name}, so the forge moves the repository as soon as you submit."
            ),
            Some(false) => format!(
                "a member of {name} who may create repositories there (an owner, or a team with that right) has to accept. Until then the repository stays where it is, marked as pending."
            ),
            None => format!(
                "unless you may create repositories in {name} (could not be read), a member of {name} who may (an owner, or a team with that right) has to accept."
            ),
        },
    }
}

/// The review shown before a transfer is submitted.
pub(crate) fn review_text(
    repo: &TransferRepo,
    target: &OwnerLookup,
    me: Requester<'_>,
    team_ids: &[u64],
) -> String {
    let mut out = format!("Transfer {} to {}\n", repo.full_name, target.login);
    let display = if target.full_name.is_empty() {
        String::new()
    } else {
        format!(", {}", target.full_name)
    };
    out.push_str(&format!(
        "  Receiving owner: {} ({}{display})\n",
        target.login,
        target.kind.label()
    ));
    out.push_str(&format!("  New location: {}/{}\n", target.login, repo.name));
    out.push_str(&format!(
        "  Who must accept: {}\n",
        who_must_accept(target, me)
    ));
    out.push_str(&format!(
        "  Permission required: owner access to {} to request it (its owning user, an owner of its owning organization, or a site administrator).\n",
        repo.full_name
    ));
    if !team_ids.is_empty() {
        let ids: Vec<String> = team_ids.iter().map(u64::to_string).collect();
        out.push_str(&format!(
            "  Teams of {} to add: {}\n",
            target.login,
            ids.join(", ")
        ));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The repository already belongs to the new owner.
    Moved { full_name: String },
    /// The forge recorded the transfer and is waiting on the recipient.
    Pending { recipient: String },
    /// Neither: the response shows the old owner and no pending transfer.
    Unclear { owner: String },
}

/// Read the result of a transfer request off the repository the forge
/// returned, not off the request.
pub(crate) fn outcome(after: &TransferRepo, new_owner: &str) -> Outcome {
    if let Some(pending) = &after.repo_transfer {
        return Outcome::Pending {
            recipient: login_or_unknown(pending.recipient.as_ref()).to_string(),
        };
    }
    if after.owner.login.eq_ignore_ascii_case(new_owner) {
        Outcome::Moved {
            full_name: after.full_name.clone(),
        }
    } else {
        Outcome::Unclear {
            owner: after.owner.login.clone(),
        }
    }
}

pub(crate) fn outcome_line(before: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Moved { full_name } => format!("✓ Moved: {before} is now {full_name}"),
        Outcome::Pending { recipient } => format!(
            "✓ Pending: {before} stays where it is until {recipient} accepts the transfer"
        ),
        Outcome::Unclear { owner } => format!(
            "The forge accepted the request, but {before} is still owned by {owner} with no pending transfer shown. Check it with `fj repo transfer view`"
        ),
    }
}

fn confirm(prompt: &str, expected: &str) -> Result<()> {
    if editor::prompt_line(prompt)?.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(anyhow!("aborted: nothing was sent"))
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn request(args: RequestArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let slug = ctx.slug();
    let requested = args.new_owner.trim();
    if requested.is_empty() {
        return Err(anyhow!("the new owner is required"));
    }
    if requested.eq_ignore_ascii_case(&ctx.owner) {
        return Err(anyhow!(
            "{} already owns {slug}. Nothing was sent",
            ctx.owner
        ));
    }
    let repo = api::repo::transfer_state(&ctx.client, &ctx.owner, &ctx.name).await?;
    let lookup = api::repo::lookup_owner(&ctx.client, requested).await?;
    check_target(
        &repo.full_name,
        &repo.owner.login,
        requested,
        lookup.as_ref(),
        repo.repo_transfer.as_ref(),
        &args.team_ids,
    )?;
    let target = lookup.expect("check_target refuses an unknown owner");
    let me = api::user::current(&ctx.client).await?;
    let can_create_in_org = match target.kind {
        OwnerKind::Organization => {
            api::org::user_permissions(&ctx.client, &me.login, &target.login)
                .await
                .ok()
                .map(|p| p.can_create_repository)
        }
        OwnerKind::User => None,
    };
    let requester = Requester {
        login: &me.login,
        site_admin: me.is_admin,
        can_create_in_org,
    };
    eprint!("{}", review_text(&repo, &target, requester, &args.team_ids));
    if !args.yes {
        confirm(
            &format!(
                "Request this transfer? Type the new owner ({}) to confirm",
                target.login
            ),
            &target.login,
        )?;
    }
    let after = api::repo::request_transfer(
        &ctx.client,
        &repo.owner.login,
        &repo.name,
        &target.login,
        &args.team_ids,
    )
    .await
    .map_err(|e| {
        api::org::with_forbidden_context(
            e,
            format!(
                "permission denied: requesting a transfer of {} needs owner access to it",
                repo.full_name
            ),
        )
    })?;
    println!(
        "{}",
        outcome_line(&repo.full_name, &outcome(&after, &target.login))
    );
    Ok(())
}

/// Where a pending transfer is headed, relative to the signed-in account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Direction {
    ToYou,
    ToYourOrg(String),
    Other,
}

impl Direction {
    fn label(&self) -> String {
        match self {
            Self::ToYou => "incoming (to you)".to_string(),
            Self::ToYourOrg(org) => format!("incoming (to your org {org})"),
            Self::Other => "not addressed to you".to_string(),
        }
    }
}

pub(crate) fn direction(transfer: &RepoTransfer, me: &str, my_orgs: &[String]) -> Direction {
    let Some(recipient) = transfer.recipient.as_ref() else {
        return Direction::Other;
    };
    if recipient.login.eq_ignore_ascii_case(me) {
        return Direction::ToYou;
    }
    match my_orgs
        .iter()
        .find(|o| o.eq_ignore_ascii_case(&recipient.login))
    {
        Some(org) => Direction::ToYourOrg(org.clone()),
        None => Direction::Other,
    }
}

/// One pending transfer, with the three things a recipient has to identify
/// before answering: the repository, its source owner and its target owner.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TransferRow {
    pub repository: String,
    pub source_owner: String,
    pub target_owner: String,
    pub requested_by: String,
    pub direction: String,
    pub teams: Vec<String>,
}

pub(crate) fn transfer_row(
    repo: &TransferRepo,
    me: &str,
    my_orgs: &[String],
) -> Option<TransferRow> {
    let t = repo.repo_transfer.as_ref()?;
    Some(TransferRow {
        repository: repo.full_name.clone(),
        source_owner: repo.owner.login.clone(),
        target_owner: login_or_unknown(t.recipient.as_ref()).to_string(),
        requested_by: login_or_unknown(t.doer.as_ref()).to_string(),
        direction: direction(t, me, my_orgs).label(),
        teams: t.teams.iter().map(|team| team.name.clone()).collect(),
    })
}

/// Keep the transfers addressed to the account or its orgs (all of them with
/// `all`), capped at `limit`.
pub(crate) fn select_rows(
    repos: &[TransferRepo],
    me: &str,
    my_orgs: &[String],
    all: bool,
    limit: usize,
) -> Vec<TransferRow> {
    repos
        .iter()
        .filter(|r| {
            all || r
                .repo_transfer
                .as_ref()
                .is_some_and(|t| direction(t, me, my_orgs) != Direction::Other)
        })
        .filter_map(|r| transfer_row(r, me, my_orgs))
        .take(limit)
        .collect()
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: ListArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let me = api::user::current(&client).await?;
    let my_orgs: Vec<String> = api::org::list_for_user(&client)
        .await?
        .into_iter()
        .map(|o| o.username)
        .collect();
    let repos = api::repo::list_pending_transfers(&client, args.owner.as_deref()).await?;
    let rows = select_rows(&repos, &me.login, &my_orgs, args.all, args.limit as usize);
    if args.json {
        return output::print_json(&serde_json::to_value(&rows)?);
    }
    if rows.is_empty() {
        let scope = match &args.owner {
            Some(owner) => format!("{owner}'s repositories that you can see"),
            None => "the repositories your account can access".to_string(),
        };
        println!("(no pending transfers found in {scope})");
        return Ok(());
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.repository.clone(),
                r.source_owner.clone(),
                r.target_owner.clone(),
                r.requested_by.clone(),
                r.direction.clone(),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(
            &["REPOSITORY", "FROM", "TO", "REQUESTED BY", "DIRECTION"],
            &table
        )
    );
    Ok(())
}

/// The identification shown for one repository's pending transfer.
pub(crate) fn pending_text(repo: &TransferRepo, transfer: &RepoTransfer) -> String {
    let mut out = format!("Repository: {}\n", repo.full_name);
    out.push_str(&format!("Source owner: {}\n", repo.owner.login));
    out.push_str(&format!(
        "Target owner: {}\n",
        login_or_unknown(transfer.recipient.as_ref())
    ));
    out.push_str(&format!(
        "Requested by: {}\n",
        login_or_unknown(transfer.doer.as_ref())
    ));
    if !transfer.teams.is_empty() {
        let names: Vec<&str> = transfer.teams.iter().map(|t| t.name.as_str()).collect();
        out.push_str(&format!("Teams to add: {}\n", names.join(", ")));
    }
    out
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: ViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let repo = api::repo::transfer_state(&ctx.client, &ctx.owner, &ctx.name).await?;
    if args.json {
        return output::print_json(&serde_json::json!({
            "repository": repo.full_name,
            "source_owner": repo.owner.login,
            "repo_transfer": repo.repo_transfer,
        }));
    }
    match &repo.repo_transfer {
        Some(t) => print!("{}", pending_text(&repo, t)),
        None => println!("(no pending transfer on {})", repo.full_name),
    }
    Ok(())
}

/// The confirmed result of accepting or rejecting, read off the repository
/// the forge returned. A response that contradicts the choice is an error.
pub(crate) fn respond_line(
    before: &str,
    recipient: &str,
    after: &TransferRepo,
    accept: bool,
) -> Result<String> {
    if after.repo_transfer.is_some() {
        return Err(anyhow!(
            "the forge answered, but {before} still shows a pending transfer"
        ));
    }
    let moved = after.owner.login.eq_ignore_ascii_case(recipient);
    match (accept, moved) {
        (true, true) => Ok(format!("✓ Accepted: {before} is now {}", after.full_name)),
        (false, false) => Ok(format!(
            "✓ Rejected: {} stays with {}",
            after.full_name, after.owner.login
        )),
        (true, false) => Err(anyhow!(
            "the forge answered, but {} is still owned by {}",
            after.full_name,
            after.owner.login
        )),
        (false, true) => Err(anyhow!(
            "the forge answered, but {} is now owned by {}",
            after.full_name,
            after.owner.login
        )),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn respond(args: RespondArgs, host: Option<&str>, accept: bool) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let repo = api::repo::transfer_state(&ctx.client, &ctx.owner, &ctx.name).await?;
    let Some(pending) = repo.repo_transfer.as_ref() else {
        return Err(anyhow!(
            "no pending transfer on {}. Nothing was sent",
            repo.full_name
        ));
    };
    let recipient = login_or_unknown(pending.recipient.as_ref()).to_string();
    eprint!("{}", pending_text(&repo, pending));
    let verb = if accept { "Accept" } else { "Reject" };
    if !args.yes {
        confirm(
            &format!(
                "{verb} this transfer? Type the repository ({}) to confirm",
                repo.full_name
            ),
            &repo.full_name,
        )?;
    }
    let sent = if accept {
        api::repo::accept_transfer(&ctx.client, &repo.owner.login, &repo.name).await
    } else {
        api::repo::reject_transfer(&ctx.client, &repo.owner.login, &repo.name).await
    };
    let after = sent.map_err(|e| {
        api::org::with_forbidden_context(
            e,
            format!(
                "permission denied: only the receiving owner ({recipient}, or a member of that organization who may create repositories in it) can answer this transfer"
            ),
        )
    })?;
    println!(
        "{}",
        respond_line(&repo.full_name, &recipient, &after, accept)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::repo::{RepoCmd, RepoSub};
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use serde_json::json;

    fn sub(argv: &[&str]) -> TransferSub {
        match Cli::try_parse_from(argv).expect("should parse").command {
            Command::Repo(RepoCmd {
                command: RepoSub::Transfer(TransferCmd { command }),
            }) => command,
            other => panic!("expected repo transfer, got {other:?}"),
        }
    }

    fn repo(owner: &str, name: &str, transfer: serde_json::Value) -> TransferRepo {
        serde_json::from_value(json!({
            "name": name,
            "full_name": format!("{owner}/{name}"),
            "owner": { "id": 1, "login": owner },
            "repo_transfer": transfer,
        }))
        .unwrap()
    }

    fn pending(doer: &str, recipient: &str) -> serde_json::Value {
        json!({
            "doer": { "id": 1, "login": doer },
            "recipient": { "id": 2, "login": recipient },
            "teams": []
        })
    }

    fn target(kind: OwnerKind, login: &str) -> OwnerLookup {
        OwnerLookup {
            kind,
            login: login.to_string(),
            full_name: String::new(),
        }
    }

    fn me(login: &str) -> Requester<'_> {
        Requester {
            login,
            site_admin: false,
            can_create_in_org: None,
        }
    }

    #[test]
    fn request_parses_owner_teams_and_yes() {
        match sub(&[
            "fj",
            "repo",
            "transfer",
            "request",
            "acme",
            "-R",
            "bob/web",
            "--team-id",
            "3",
            "--team-id",
            "4",
            "-y",
        ]) {
            TransferSub::Request(a) => {
                assert_eq!(a.new_owner, "acme");
                assert_eq!(a.r.repo.as_deref(), Some("bob/web"));
                assert_eq!(a.team_ids, vec![3, 4]);
                assert!(a.yes);
            }
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn request_and_respond_prompt_by_default() {
        match sub(&["fj", "repo", "transfer", "request", "acme"]) {
            TransferSub::Request(a) => assert!(!a.yes),
            other => panic!("expected request, got {other:?}"),
        }
        match sub(&["fj", "repo", "transfer", "accept", "-R", "bob/web"]) {
            TransferSub::Accept(a) => assert!(!a.yes),
            other => panic!("expected accept, got {other:?}"),
        }
        assert!(matches!(
            sub(&["fj", "repo", "transfer", "reject", "-R", "bob/web", "--yes"]),
            TransferSub::Reject(RespondArgs { yes: true, .. })
        ));
    }

    #[test]
    fn list_parses_owner_all_and_limit() {
        match sub(&[
            "fj", "repo", "transfer", "list", "--owner", "bob", "--all", "-L", "5",
        ]) {
            TransferSub::List(a) => {
                assert_eq!(a.owner.as_deref(), Some("bob"));
                assert!(a.all);
                assert_eq!(a.limit, 5);
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn unknown_owner_is_refused() {
        let err = check_target("bob/web", "bob", "ghost", None, None, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no user or organization named 'ghost'"),
            "{msg}"
        );
        assert!(msg.contains("Nothing was sent"), "{msg}");
    }

    #[test]
    fn current_owner_is_refused_in_any_spelling() {
        let same = target(OwnerKind::User, "bob");
        for requested in ["bob", "BOB"] {
            let err =
                check_target("bob/web", "bob", requested, Some(&same), None, &[]).unwrap_err();
            assert!(err.to_string().contains("already owns"), "{err}");
        }
        // The forge resolved the name to the current owner under another spelling.
        let err = check_target("bob/web", "bob", "robert", Some(&same), None, &[]).unwrap_err();
        assert!(err.to_string().contains("already owns"), "{err}");
    }

    #[test]
    fn pending_transfer_blocks_a_second_request() {
        let r = repo("bob", "web", pending("bob", "acme"));
        let other = target(OwnerKind::User, "carol");
        let err = check_target(
            "bob/web",
            "bob",
            "carol",
            Some(&other),
            r.repo_transfer.as_ref(),
            &[],
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("pending transfer to acme"),
            "{err}"
        );
    }

    #[test]
    fn team_ids_need_an_organization_target() {
        let user = target(OwnerKind::User, "carol");
        assert!(check_target("bob/web", "bob", "carol", Some(&user), None, &[3]).is_err());
        let org = target(OwnerKind::Organization, "acme");
        assert!(check_target("bob/web", "bob", "acme", Some(&org), None, &[3]).is_ok());
        assert!(check_target("bob/web", "bob", "carol", Some(&user), None, &[]).is_ok());
    }

    #[test]
    fn review_names_a_user_and_who_accepts() {
        let r = repo("bob", "web", json!(null));
        let mut t = target(OwnerKind::User, "carol");
        t.full_name = "Carol Example".into();
        let text = review_text(&r, &t, me("bob"), &[]);
        assert!(text.contains("Transfer bob/web to carol"), "{text}");
        assert!(
            text.contains("Receiving owner: carol (user, Carol Example)"),
            "{text}"
        );
        assert!(text.contains("New location: carol/web"), "{text}");
        assert!(
            text.contains("Who must accept: carol has to accept"),
            "{text}"
        );
        assert!(
            text.contains("Permission required: owner access to bob/web"),
            "{text}"
        );
    }

    #[test]
    fn review_names_an_organization_and_its_acceptors() {
        let r = repo("bob", "web", json!(null));
        let t = target(OwnerKind::Organization, "acme");
        let outsider = Requester {
            can_create_in_org: Some(false),
            ..me("bob")
        };
        let text = review_text(&r, &t, outsider, &[7]);
        assert!(
            text.contains("Receiving owner: acme (organization)"),
            "{text}"
        );
        assert!(
            text.contains("a member of acme who may create repositories there"),
            "{text}"
        );
        assert!(text.contains("Teams of acme to add: 7"), "{text}");

        let member = Requester {
            can_create_in_org: Some(true),
            ..me("bob")
        };
        assert!(who_must_accept(&t, member).starts_with("nobody"));
        assert!(who_must_accept(&t, me("bob")).contains("could not be read"));
    }

    #[test]
    fn nobody_accepts_for_self_or_site_admin() {
        let carol = target(OwnerKind::User, "carol");
        assert!(who_must_accept(&carol, me("Carol")).starts_with("nobody"));
        let admin = Requester {
            site_admin: true,
            ..me("root")
        };
        assert!(who_must_accept(&carol, admin).starts_with("nobody"));
    }

    #[test]
    fn outcome_is_read_from_the_returned_repository() {
        let moved = repo("acme", "web", json!(null));
        assert_eq!(
            outcome(&moved, "Acme"),
            Outcome::Moved {
                full_name: "acme/web".into()
            }
        );
        let waiting = repo("bob", "web", pending("bob", "carol"));
        assert_eq!(
            outcome(&waiting, "carol"),
            Outcome::Pending {
                recipient: "carol".into()
            }
        );
        let odd = repo("bob", "web", json!(null));
        assert_eq!(
            outcome(&odd, "carol"),
            Outcome::Unclear {
                owner: "bob".into()
            }
        );
    }

    #[test]
    fn outcome_lines_say_moved_or_pending() {
        let moved = outcome_line(
            "bob/web",
            &Outcome::Moved {
                full_name: "acme/web".into(),
            },
        );
        assert_eq!(moved, "✓ Moved: bob/web is now acme/web");
        let waiting = outcome_line(
            "bob/web",
            &Outcome::Pending {
                recipient: "carol".into(),
            },
        );
        assert!(waiting.starts_with("✓ Pending:"), "{waiting}");
        assert!(waiting.contains("until carol accepts"), "{waiting}");
        let odd = outcome_line(
            "bob/web",
            &Outcome::Unclear {
                owner: "bob".into(),
            },
        );
        assert!(!odd.starts_with('✓'), "{odd}");
    }

    #[test]
    fn direction_matches_you_or_your_orgs() {
        let r = repo("bob", "web", pending("bob", "Carol"));
        let t = r.repo_transfer.as_ref().unwrap();
        let orgs = vec!["acme".to_string()];
        assert_eq!(direction(t, "carol", &orgs), Direction::ToYou);
        assert_eq!(direction(t, "dave", &orgs), Direction::Other);
        let r = repo("bob", "web", pending("bob", "ACME"));
        assert_eq!(
            direction(r.repo_transfer.as_ref().unwrap(), "dave", &orgs),
            Direction::ToYourOrg("acme".into())
        );
    }

    #[test]
    fn select_rows_keeps_incoming_unless_all() {
        let repos = vec![
            repo("bob", "web", pending("bob", "carol")),
            repo("carol", "lib", pending("carol", "erin")),
            repo("dan", "api", pending("dan", "acme")),
        ];
        let orgs = vec!["acme".to_string()];
        let rows = select_rows(&repos, "carol", &orgs, false, 30);
        let names: Vec<&str> = rows.iter().map(|r| r.repository.as_str()).collect();
        assert_eq!(names, vec!["bob/web", "dan/api"]);
        assert_eq!(rows[0].source_owner, "bob");
        assert_eq!(rows[0].target_owner, "carol");
        assert_eq!(rows[0].requested_by, "bob");
        assert_eq!(rows[1].direction, "incoming (to your org acme)");

        assert_eq!(select_rows(&repos, "carol", &orgs, true, 30).len(), 3);
        assert_eq!(select_rows(&repos, "carol", &orgs, true, 1).len(), 1);
    }

    #[test]
    fn pending_text_identifies_source_target_and_repository() {
        let r = repo("bob", "web", pending("bob", "acme"));
        let text = pending_text(&r, r.repo_transfer.as_ref().unwrap());
        assert!(text.contains("Repository: bob/web"), "{text}");
        assert!(text.contains("Source owner: bob"), "{text}");
        assert!(text.contains("Target owner: acme"), "{text}");
        assert!(text.contains("Requested by: bob"), "{text}");
    }

    #[test]
    fn respond_line_reflects_the_confirmed_result() {
        let accepted = repo("acme", "web", json!(null));
        assert_eq!(
            respond_line("bob/web", "acme", &accepted, true).unwrap(),
            "✓ Accepted: bob/web is now acme/web"
        );
        let rejected = repo("bob", "web", json!(null));
        assert_eq!(
            respond_line("bob/web", "acme", &rejected, false).unwrap(),
            "✓ Rejected: bob/web stays with bob"
        );
    }

    #[test]
    fn respond_line_errors_when_the_server_state_disagrees() {
        let still_pending = repo("bob", "web", pending("bob", "acme"));
        assert!(respond_line("bob/web", "acme", &still_pending, true).is_err());
        let not_moved = repo("bob", "web", json!(null));
        assert!(respond_line("bob/web", "acme", &not_moved, true).is_err());
        let moved = repo("acme", "web", json!(null));
        assert!(respond_line("bob/web", "acme", &moved, false).is_err());
    }
}
