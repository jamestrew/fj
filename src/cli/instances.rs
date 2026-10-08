//! `fj instances`: list the Fjord Account user's Forgejo instances and set
//! the default one to route all other commands through.
//!
//! These calls go to fjord-platform (the gateway), not to a Forgejo instance.
//! See `crate::fjord` for the underlying HTTP layer.

use std::future::Future;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};

use crate::auth as token_store;
use crate::config::hosts::{AuthKind, Hosts};
use crate::fjord::{self, CommonsInfo, ForgeInstance};
use crate::output;

#[derive(Debug, Args)]
pub struct InstancesCmd {
    #[command(subcommand)]
    pub command: Option<InstancesSub>,
    /// Override which Fjord-Account host to operate against. Defaults to
    /// the current host if it's a Fjord Account.
    #[arg(long, global = true)]
    pub host: Option<String>,
    /// Emit JSON instead of a table. Only meaningful with `list` (or with
    /// no subcommand, which defaults to list).
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum InstancesSub {
    /// List the Forgejo instances owned by this Fjord Account, plus the
    /// shared community forge (the Commons) when the account has joined it.
    List(ListArgs),
    /// Set the default Forgejo instance subsequent commands will route to.
    Use(UseArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UseArgs {
    /// Instance id or slug (as printed by `fj instances list`). `commons`
    /// names the shared community forge; when it is browser-only it is opened
    /// in the browser instead and the default instance is left unchanged.
    pub id_or_slug: String,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: InstancesCmd, top_host: Option<&str>) -> Result<()> {
    // `--host` on the subcommand wins over the top-level `--host`.
    let host_flag = cmd.host.as_deref().or(top_host);
    match cmd.command {
        None => list(host_flag, cmd.json).await,
        Some(InstancesSub::List(args)) => list(host_flag, cmd.json || args.json).await,
        Some(InstancesSub::Use(args)) => use_instance(host_flag, args).await,
    }
}

/// How long the Commons lookup may keep the instance list waiting once the
/// list itself has arrived. The Commons is a nicety on this surface; the
/// account's own forges are the point of it.
const COMMONS_GRACE: Duration = Duration::from_secs(2);

/// What the best-effort Commons lookup produced.
#[derive(Debug, Clone)]
pub(crate) enum CommonsLookup {
    /// The platform returned a descriptor. It may still be un-joined.
    Found(CommonsInfo),
    /// The platform has no Commons: a 404 or `{ commons: null }`.
    Absent,
    /// The lookup failed or ran out of time. Recorded under `--debug`, and
    /// otherwise indistinguishable from `Absent` to the user on purpose.
    Unavailable,
}

/// The account's instances plus the Commons, kept apart.
///
/// `instances` is exactly what `/me/forge-instances` returned. The Commons is
/// not an instance the account owns and is never folded into that list; it is
/// resolved through its own lookup and only combined with the instances at the
/// two projections below, which every surface goes through.
#[derive(Debug, Clone)]
pub(crate) struct Listing {
    pub instances: Vec<ForgeInstance>,
    pub commons: CommonsLookup,
}

impl Listing {
    fn commons_info(&self) -> Option<&CommonsInfo> {
        match &self.commons {
            CommonsLookup::Found(info) => Some(info),
            CommonsLookup::Absent | CommonsLookup::Unavailable => None,
        }
    }

    /// The Commons as a switchable instance: joined and gateway-routable only.
    fn commons_instance(&self) -> Option<ForgeInstance> {
        self.commons_info()?.to_switchable_instance()
    }

    /// Everything `fj instances use` may select: the account's instances, then
    /// the Commons when it is joined and routable.
    ///
    /// The duplicate guard covers the platform ever listing the Commons in
    /// both places: the app carries the same check, and a duplicate would make
    /// `instances use` ambiguous.
    pub(crate) fn selectable(&self) -> Vec<ForgeInstance> {
        let mut all = self.instances.clone();
        if let Some(commons) = self.commons_instance() {
            if !all.iter().any(|i| i.id == commons.id) {
                all.push(commons);
            }
        }
        all
    }

    /// A joined Commons that cannot be routed through the gateway, with the
    /// URL to hand the user to in the browser.
    fn browser_handoff(&self) -> Option<(&CommonsInfo, &str)> {
        let info = self.commons_info()?;
        Some((info, info.browser_handoff_url()?))
    }
}

/// Await the instance list and the Commons lookup concurrently, without
/// letting the second hold up the first.
///
/// The Commons answer is taken if it lands before the list does, or within
/// `grace` after it. Otherwise it is abandoned and reported as `None`. A
/// failed list returns at once: there is nothing to co-present the Commons
/// with, and the caller is about to report the list error anyway.
async fn list_then_commons<L, C, T, U>(
    list: L,
    commons: C,
    grace: Duration,
) -> (Result<T>, Option<Result<U>>)
where
    L: Future<Output = Result<T>>,
    C: Future<Output = Result<U>>,
{
    tokio::pin!(list);
    tokio::pin!(commons);
    let mut early = None;
    let listed = loop {
        tokio::select! {
            result = &mut list => break result,
            result = &mut commons, if early.is_none() => early = Some(result),
        }
    };
    if listed.is_err() {
        return (listed, early);
    }
    let commons = match early {
        Some(result) => Some(result),
        None => tokio::time::timeout(grace, &mut commons).await.ok(),
    };
    (listed, commons)
}

/// The `--debug` record for a Commons lookup that did not produce an answer,
/// or `None` when it did. This is the only place a Commons failure is ever
/// reported: it is never a user-facing error and never a failure of the
/// user's own forges.
fn commons_failure_record(
    outcome: &Option<Result<Option<CommonsInfo>>>,
    grace: Duration,
) -> Option<String> {
    match outcome {
        Some(Ok(_)) => None,
        Some(Err(e)) => Some(format!(
            "commons lookup failed (instance list unaffected): {e:#}"
        )),
        None => Some(format!(
            "commons lookup gave no answer within {grace:?} of the instance list; \
             showing the list without it"
        )),
    }
}

fn settle_commons(outcome: Option<Result<Option<CommonsInfo>>>, grace: Duration) -> CommonsLookup {
    if let Some(record) = commons_failure_record(&outcome, grace) {
        if crate::client::debug_enabled() {
            eprintln!("{record}");
        }
    }
    match outcome {
        Some(Ok(Some(info))) => CommonsLookup::Found(info),
        Some(Ok(None)) => CommonsLookup::Absent,
        Some(Err(_)) | None => CommonsLookup::Unavailable,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn fetch_listing_with_grace(
    platform_url: &str,
    bearer: &str,
    grace: Duration,
) -> Result<Listing> {
    let (instances, commons) = list_then_commons(
        fjord::list_instances(platform_url, bearer),
        fjord::fetch_commons(platform_url, bearer),
        grace,
    )
    .await;
    Ok(Listing {
        instances: instances?,
        commons: settle_commons(commons, grace),
    })
}

/// One attempt at the listing with the given bearer and no session refresh.
/// Sign-in uses this directly: it holds a bearer minted seconds ago.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn fetch_listing_once(platform_url: &str, bearer: &str) -> Result<Listing> {
    fetch_listing_with_grace(platform_url, bearer, COMMONS_GRACE).await
}

/// List the account's forge instances, transparently refreshing the session
/// once if the bearer has expired. On an OIDC (SSO) host a stored refresh token
/// renews the access token silently and the call is retried; without one, the
/// original 401 (`SessionExpired`) surfaces so the user re-runs
/// `fj auth login --fjord`.
///
/// This refresh stays here (rather than folding into the shared `Client`'s new
/// central refresh) because `fj instances` talks to the platform's
/// `/api/v1/me/forge-instances` and `/api/v1/me/commons` endpoints directly,
/// NOT through `Client` (which is scoped to a single forge gateway). The two
/// paths are independent; the `Client` refresh covers `fj api`/`pr`/`issue`.
///
/// Only the instance list can fail this call. The Commons lookup is
/// best-effort and independent: an error or a slow answer leaves the Commons
/// unsurfaced (see [`CommonsLookup::Unavailable`]) and the list is returned
/// exactly as it would have been. A `None` from the Commons call is not an
/// error either: an older platform, or one with the feature off, answers 404.
///
/// The Commons call is made every time and its result is never persisted.
/// `membership.joined` can change without fj hearing about it, and a cached
/// "joined" would offer an instance the account can no longer use. It is also
/// the reason nothing here needs an expiry: fj stores no credential from this
/// path at all, since the gateway mints and attaches the forge token
/// server-side per call.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn fetch_listing(platform_url: &str, bearer: &str) -> Result<Listing> {
    match fetch_listing_once(platform_url, bearer).await {
        Ok(listing) => Ok(listing),
        Err(e) => {
            // Only the instance list's 401 gets here. The refreshed bearer is
            // used for both calls on the retry, since they share it.
            let expired = e.downcast_ref::<fjord::SessionExpired>().is_some();
            if expired && fjord::oidc::valid_refresh_token(platform_url, bearer)?.is_some() {
                let fresh = fjord::oidc::refresh_session(platform_url)
                    .await
                    .with_context(|| format!("{e}; automatic session refresh failed"))?;
                fetch_listing_once(platform_url, &fresh).await
            } else {
                Err(e)
            }
        }
    }
}

/// The note shown when the platform advertises a Commons the account has not
/// joined. Without it, `fj instances` looks identical to a platform that has no
/// Commons at all, which reads as "no such forge" rather than "not yours yet".
fn commons_hint(commons: Option<&fjord::CommonsInfo>) -> Option<String> {
    let c = commons?;
    if c.membership.joined {
        return None;
    }
    let name = if c.name.trim().is_empty() {
        "the Fjord Commons"
    } else {
        c.name.trim()
    };
    Some(format!(
        "{name} is available at {} but this account has not joined it, so it is not listed above.",
        c.url
    ))
}

/// Look up the active Fjord-Account host config + bearer. Errors clearly
/// when the user is signed in only via PAT.
fn resolve_fjord(host_flag: Option<&str>) -> Result<(String, crate::config::hosts::Host, String)> {
    let hosts = Hosts::load()?;
    let name = hosts.resolve_host(host_flag)?.to_string();
    let cfg = hosts
        .hosts
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow!("host '{name}' not configured"))?;
    if !matches!(cfg.auth_kind, AuthKind::FjordAccount) {
        return Err(anyhow!(
            "host '{name}' is signed in with a Forgejo PAT, not a Fjord Account. \
             Run `fj auth login --fjord` first."
        ));
    }
    let fa = cfg.fjord_account.as_ref().ok_or_else(|| {
        anyhow!("host '{name}' is fjord_account but missing fjord_account config")
    })?;
    let bearer = token_store::load_fjord_bearer(&fa.platform_url)?.ok_or_else(|| {
        anyhow!(
            "no Fjord Account session in keychain for {}. Run `fj auth login --fjord`.",
            fa.platform_url
        )
    })?;
    Ok((name, cfg, bearer))
}

const LIST_HEADERS: [&str; 8] = ["", "ID", "NAME", "SLUG", "REGION", "PLAN", "STATE", "URL"];

/// What the STATE column says for a Commons that only opens in the browser.
const BROWSER_ONLY: &str = "browser only";

/// Table rows for `fj instances list`: the account's instances, then the
/// joined Commons in the same table, as a selectable row when it is routable
/// and as a browser-only row (no id to select) when it is not.
fn list_rows(listing: &Listing, current_default: Option<&str>) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = listing
        .selectable()
        .iter()
        .map(|i| {
            let marker = if Some(i.id.as_str()) == current_default {
                "*"
            } else {
                ""
            };
            vec![
                marker.into(),
                i.id.clone(),
                i.display_name.clone(),
                i.slug.clone(),
                i.region.clone().unwrap_or_default(),
                i.plan.clone().unwrap_or_default(),
                i.state.clone().unwrap_or_default(),
                i.public_url.clone().unwrap_or_default(),
            ]
        })
        .collect();
    if let Some((info, url)) = listing.browser_handoff() {
        rows.push(vec![
            String::new(),
            "-".into(),
            info.display_name().to_string(),
            "commons".into(),
            "shared".into(),
            "commons".into(),
            BROWSER_ONLY.into(),
            url.to_string(),
        ]);
    }
    rows
}

fn member_suffix(info: &CommonsInfo) -> String {
    match info.member_count {
        Some(1) => " (1 member)".to_string(),
        Some(n) => format!(" ({n} members)"),
        None => String::new(),
    }
}

/// The lines printed under the table that say what the Commons row is. A
/// joined Commons is marked as the shared community forge either way; the
/// browser-only one also says, in so many words, that opening it leaves fj.
fn commons_notes(listing: &Listing) -> Vec<String> {
    let Some(info) = listing.commons_info() else {
        return Vec::new();
    };
    let name = info.display_name();
    let members = member_suffix(info);
    if let Some(instance) = listing.commons_instance() {
        return vec![format!(
            "{name} is the shared community forge{members}, not one of this account's own \
             instances. Select it with `fj instances use {}`.",
            instance.id
        )];
    }
    if let Some((_, url)) = listing.browser_handoff() {
        return vec![format!(
            "{name} is the shared community forge{members}. It is not routable through fj yet, \
             so it opens in your browser and leaves fj: {url} \
             (`fj instances use commons` opens it; it cannot be the default instance)."
        )];
    }
    commons_hint(Some(info)).into_iter().collect()
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(host_flag: Option<&str>, json: bool) -> Result<()> {
    let (_name, cfg, bearer) = resolve_fjord(host_flag)?;
    let fa = cfg
        .fjord_account
        .as_ref()
        .expect("resolve_fjord guarantees fjord_account is Some");
    let listing = fetch_listing(&fa.platform_url, &bearer)
        .await
        .with_context(|| format!("listing forge instances at {}", fa.platform_url))?;

    if json {
        return output::print_json(&serde_json::to_value(serializable_listing(&listing))?);
    }

    let rows = list_rows(&listing, fa.default_instance_id.as_deref());
    let notes = commons_notes(&listing);
    let provision =
        "Provision one through the Fjord Account dashboard, then re-run `fj instances`.";

    if rows.is_empty() {
        println!(
            "(no Forgejo instances yet for this account at {})",
            fa.platform_url
        );
        // Printed before the provisioning hint: a user with no instances who
        // has not joined the Commons is being told about the wrong thing
        // otherwise.
        for note in &notes {
            println!("{}", output::dim(note));
        }
        println!("{}", output::dim(provision));
        return Ok(());
    }

    print!("{}", output::render_table(&LIST_HEADERS, &rows));
    for note in &notes {
        println!("{}", output::dim(note));
    }
    if listing.instances.is_empty() {
        // Only the Commons is listed. It is not the account's own forge, so
        // the empty state for those still applies.
        println!(
            "{}",
            output::dim(&format!(
                "No Forgejo instances of your own yet at {}. {provision}",
                fa.platform_url
            ))
        );
    }
    Ok(())
}

/// JSON projection of the instance list. Stable shape, separate from the
/// fjord-platform wire shape so we control what `--json` consumers see.
fn serializable_list(instances: &[ForgeInstance]) -> Vec<serde_json::Value> {
    instances
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "displayName": i.display_name,
                "slug": i.slug,
                "region": i.region,
                "plan": i.plan,
                "state": i.state,
                "publicUrl": i.public_url,
                "kind": "instance",
                "routing": "gateway",
            })
        })
        .collect()
}

/// The Commons descriptor as `--json` shows it, attached to the Commons entry.
fn commons_json(info: &CommonsInfo) -> serde_json::Value {
    serde_json::json!({
        "url": info.url,
        "name": info.name,
        "memberCount": info.member_count,
        "gatewayForgeId": info.gateway_forge_id,
        "membership": {
            "joined": info.membership.joined,
            "namespace": info.membership.namespace,
            "orgSlug": info.membership.org_slug,
        },
    })
}

/// `--json` for `fj instances list`: the same entries as the table. Every
/// entry carries `kind` (`instance` or `commons`) and `routing` (`gateway` or
/// `browser`). The Commons entry adds a `commons` object with the descriptor.
/// A browser-only Commons has a null `id`, because there is nothing to pass
/// to `fj instances use`.
fn serializable_listing(listing: &Listing) -> Vec<serde_json::Value> {
    let commons_id = listing.commons_instance().map(|i| i.id);
    let mut out = serializable_list(&listing.selectable());
    if let Some(info) = listing.commons_info() {
        for entry in out.iter_mut() {
            if entry["id"].as_str() == commons_id.as_deref() && commons_id.is_some() {
                entry["kind"] = "commons".into();
                entry["commons"] = commons_json(info);
            }
        }
    }
    if let Some((info, url)) = listing.browser_handoff() {
        out.push(serde_json::json!({
            "id": null,
            "displayName": info.display_name(),
            "slug": "commons",
            "region": "shared",
            "plan": "commons",
            "state": null,
            "publicUrl": url,
            "kind": "commons",
            "routing": "browser",
            "commons": commons_json(info),
        }));
    }
    out
}

/// What `fj instances use <arg>` resolved to.
#[derive(Debug)]
enum UseTarget {
    /// A selectable instance: becomes the default.
    Instance(ForgeInstance),
    /// The joined, browser-only Commons: opened in the browser, never selected.
    BrowserHandoff { name: String, url: String },
}

fn resolve_use_target(listing: &Listing, arg: &str, platform_url: &str) -> Result<UseTarget> {
    if let Some(found) = listing
        .selectable()
        .into_iter()
        .find(|i| i.id == arg || i.slug == arg)
    {
        return Ok(UseTarget::Instance(found));
    }
    if arg.eq_ignore_ascii_case("commons") {
        if let Some((info, url)) = listing.browser_handoff() {
            return Ok(UseTarget::BrowserHandoff {
                name: info.display_name().to_string(),
                url: url.to_string(),
            });
        }
        // Asking for the Commons by name deserves the real reason. Without
        // this, "not joined" and "this platform has no Commons" both come
        // back as "no instance matches", which sends the user looking for a
        // typo instead of at a join link.
        if let Some(hint) = commons_hint(listing.commons_info()) {
            return Err(anyhow!("{hint}"));
        }
        if matches!(listing.commons, CommonsLookup::Absent) {
            return Err(anyhow!(
                "this platform ({platform_url}) does not advertise a Commons."
            ));
        }
        if matches!(listing.commons, CommonsLookup::Unavailable) {
            return Err(anyhow!(
                "the Commons could not be looked up at {platform_url} just now. Try again shortly \
                 (re-run with `--debug` for the cause)."
            ));
        }
    }
    Err(anyhow!(
        "no instance matches '{arg}'. Run `fj instances list` to see what's available."
    ))
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn use_instance(host_flag: Option<&str>, args: UseArgs) -> Result<()> {
    let (name, cfg, bearer) = resolve_fjord(host_flag)?;
    let fa = cfg
        .fjord_account
        .as_ref()
        .expect("resolve_fjord guarantees fjord_account is Some");

    // Refresh the list and look up by id or slug. We do this fetch every
    // time rather than letting the user pick something stale; instance ids
    // are stable but plans / states aren't.
    let listing = fetch_listing(&fa.platform_url, &bearer)
        .await
        .with_context(|| format!("listing forge instances at {}", fa.platform_url))?;
    let target = match resolve_use_target(&listing, &args.id_or_slug, &fa.platform_url)? {
        UseTarget::Instance(target) => target,
        UseTarget::BrowserHandoff { name, url } => return hand_off_to_browser(&name, &url),
    };

    // Persist the new default into hosts.toml.
    let mut hosts = Hosts::load()?;
    let host_mut = hosts
        .hosts
        .get_mut(&name)
        .ok_or_else(|| anyhow!("host '{name}' disappeared from hosts.toml between calls"))?;
    let fa_mut = host_mut
        .fjord_account
        .as_mut()
        .ok_or_else(|| anyhow!("host '{name}' lost its fjord_account config between calls"))?;
    fa_mut.default_instance_id = Some(target.id.clone());
    fa_mut.default_instance_display_name = Some(target.display_name.clone());
    fa_mut.default_instance_slug = Some(target.slug.clone());
    fa_mut.default_instance_public_url = target.public_url.clone();
    hosts.save()?;

    println!(
        "{} Default instance is now {} ({})",
        output::bold("✓"),
        output::bold(&target.display_name),
        target.id
    );
    if let Some(url) = target.public_url.as_deref() {
        println!("  {}", output::dim(url));
    }
    Ok(())
}

/// Send the user to a browser-only Commons. Nothing is written to hosts.toml:
/// a forge fj cannot route to must never become the selected instance.
///
/// The browser is launched only from an interactive terminal. A script gets
/// the labelled URL on stdout and no window.
#[cfg_attr(coverage_nightly, coverage(off))]
fn hand_off_to_browser(name: &str, url: &str) -> Result<()> {
    use std::io::IsTerminal;

    println!(
        "{} {} is the shared community forge and is not routable through fj yet.",
        output::bold("→"),
        output::bold(name)
    );
    println!("  It opens in your browser, which leaves fj: {url}");
    println!("  {}", output::dim("The default instance is unchanged."));
    if std::io::stdout().is_terminal() {
        if let Err(e) = crate::cli::web::open(url) {
            eprintln!(
                "  {}",
                output::dim(&format!(
                    "(couldn't launch a browser automatically: {e}; open the URL above yourself)"
                ))
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(id: &str) -> ForgeInstance {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "displayName": "My Forge",
            "slug": "my-forge",
            "region": "hel1",
            "plan": "starter",
            "state": "running",
            "publicUrl": "https://my-forge.example",
            "createdAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn serializable_list_uses_camel_case_keys() {
        let out = serializable_list(&[instance("inst_1")]);
        assert_eq!(out.len(), 1);
        let obj = &out[0];
        assert_eq!(obj["id"], "inst_1");
        assert_eq!(obj["displayName"], "My Forge");
        assert_eq!(obj["slug"], "my-forge");
        assert_eq!(obj["region"], "hel1");
        assert_eq!(obj["plan"], "starter");
        assert_eq!(obj["state"], "running");
        assert_eq!(obj["publicUrl"], "https://my-forge.example");
        // createdAt is intentionally not surfaced in the serialized view.
        assert!(obj.get("createdAt").is_none());
    }

    #[test]
    fn instances_list_accepts_json_after_subcommand() {
        use clap::Parser;

        let cli = crate::cli::Cli::try_parse_from(["fj", "instances", "list", "--json"])
            .expect("instances list should accept --json after the subcommand");
        let crate::cli::Command::Instances(InstancesCmd {
            command: Some(InstancesSub::List(args)),
            ..
        }) = cli.command
        else {
            panic!("expected instances list");
        };
        assert!(args.json);
    }

    #[test]
    fn serializable_list_preserves_order_and_null_optionals() {
        let mut sparse: ForgeInstance = instance("inst_2");
        sparse.region = None;
        sparse.public_url = None;
        let out = serializable_list(&[instance("inst_1"), sparse]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], "inst_1");
        assert_eq!(out[1]["id"], "inst_2");
        assert!(out[1]["region"].is_null());
        assert!(out[1]["publicUrl"].is_null());
    }

    fn commons(joined: bool, gateway: Option<&str>) -> CommonsInfo {
        serde_json::from_value(serde_json::json!({
            "url": "https://commons.example",
            "name": "The Commons",
            "memberCount": 42,
            "gatewayForgeId": gateway,
            "membership": { "joined": joined, "namespace": "stephen", "orgSlug": "commons" },
        }))
        .unwrap()
    }

    fn listing(instances: Vec<ForgeInstance>, commons: CommonsLookup) -> Listing {
        Listing { instances, commons }
    }

    #[test]
    fn commons_is_kept_out_of_the_instance_list_model() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, Some("box_1"))),
        );
        // The model holds only what /me/forge-instances returned. The Commons
        // joins it at the projection, not in the stored list.
        assert_eq!(l.instances.len(), 1);
        let selectable = l.selectable();
        assert_eq!(selectable.len(), 2);
        assert_eq!(selectable[1].id, "box_1");
        assert_eq!(selectable[1].slug, "commons");
    }

    #[test]
    fn selectable_does_not_duplicate_a_commons_the_platform_also_listed() {
        let l = listing(
            vec![instance("box_1")],
            CommonsLookup::Found(commons(true, Some("box_1"))),
        );
        assert_eq!(l.selectable().len(), 1);
    }

    #[test]
    fn routable_commons_is_listed_and_marked_shared() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, Some("box_1"))),
        );
        let rows = list_rows(&l, Some("box_1"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "*");
        assert_eq!(rows[1][1], "box_1");
        assert_eq!(rows[1][4], "shared");
        assert_eq!(rows[1][5], "commons");

        let notes = commons_notes(&l);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("shared community forge"), "{}", notes[0]);
        assert!(notes[0].contains("42 members"), "{}", notes[0]);
        assert!(notes[0].contains("fj instances use box_1"), "{}", notes[0]);
    }

    #[test]
    fn browser_only_commons_is_listed_without_a_selectable_id() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, None)),
        );
        // Not selectable: it has no gateway id to route through.
        assert_eq!(l.selectable().len(), 1);

        let rows = list_rows(&l, None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][1], "-");
        assert_eq!(rows[1][2], "The Commons");
        assert_eq!(rows[1][6], BROWSER_ONLY);
        assert_eq!(rows[1][7], "https://commons.example");

        let notes = commons_notes(&l);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("shared community forge"), "{}", notes[0]);
        assert!(notes[0].contains("leaves fj"), "{}", notes[0]);
        assert!(notes[0].contains("https://commons.example"), "{}", notes[0]);
    }

    #[test]
    fn browser_only_commons_is_listed_even_with_no_instances_of_ones_own() {
        let l = listing(Vec::new(), CommonsLookup::Found(commons(true, None)));
        let rows = list_rows(&l, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][6], BROWSER_ONLY);
    }

    #[test]
    fn unjoined_commons_is_not_presented_as_a_row() {
        for gateway in [Some("box_1"), None] {
            let l = listing(
                vec![instance("inst_1")],
                CommonsLookup::Found(commons(false, gateway)),
            );
            assert_eq!(list_rows(&l, None).len(), 1);
            assert_eq!(l.selectable().len(), 1);
            assert!(l.browser_handoff().is_none());
            assert_eq!(serializable_listing(&l).len(), 1);
            // The existing "not joined" hint is the only trace of it.
            let notes = commons_notes(&l);
            assert_eq!(notes.len(), 1);
            assert!(notes[0].contains("has not joined"), "{}", notes[0]);
        }
    }

    #[test]
    fn absent_or_unavailable_commons_leaves_the_list_as_it_was() {
        for lookup in [CommonsLookup::Absent, CommonsLookup::Unavailable] {
            let l = listing(vec![instance("inst_1")], lookup);
            let rows = list_rows(&l, None);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][1], "inst_1");
            // No note at all: a failed lookup is not reported to the user.
            assert!(commons_notes(&l).is_empty());
            let json = serializable_listing(&l);
            assert_eq!(json.len(), 1);
            assert_eq!(json[0]["kind"], "instance");
        }
    }

    #[test]
    fn json_marks_a_routable_commons_and_carries_the_descriptor() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, Some("box_1"))),
        );
        let json = serializable_listing(&l);
        assert_eq!(json.len(), 2);
        assert_eq!(json[0]["kind"], "instance");
        assert_eq!(json[0]["routing"], "gateway");
        assert!(json[0].get("commons").is_none());
        assert_eq!(json[1]["id"], "box_1");
        assert_eq!(json[1]["kind"], "commons");
        assert_eq!(json[1]["routing"], "gateway");
        assert_eq!(json[1]["commons"]["memberCount"], 42);
        assert_eq!(json[1]["commons"]["membership"]["joined"], true);
        assert_eq!(json[1]["commons"]["membership"]["namespace"], "stephen");
        assert_eq!(json[1]["commons"]["membership"]["orgSlug"], "commons");
    }

    #[test]
    fn json_lists_a_browser_only_commons_with_no_id() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, None)),
        );
        let json = serializable_listing(&l);
        assert_eq!(json.len(), 2);
        assert!(json[1]["id"].is_null());
        assert_eq!(json[1]["kind"], "commons");
        assert_eq!(json[1]["routing"], "browser");
        assert_eq!(json[1]["publicUrl"], "https://commons.example");
        assert!(json[1]["commons"]["gatewayForgeId"].is_null());
    }

    #[test]
    fn use_selects_an_instance_or_a_routable_commons() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, Some("box_1"))),
        );
        for (arg, id) in [
            ("inst_1", "inst_1"),
            ("my-forge", "inst_1"),
            ("box_1", "box_1"),
            ("commons", "box_1"),
        ] {
            match resolve_use_target(&l, arg, "https://p.example").unwrap() {
                UseTarget::Instance(found) => assert_eq!(found.id, id),
                other => panic!("expected an instance for {arg}, got {other:?}"),
            }
        }
    }

    #[test]
    fn use_hands_a_browser_only_commons_to_the_browser_instead_of_selecting_it() {
        let l = listing(
            vec![instance("inst_1")],
            CommonsLookup::Found(commons(true, None)),
        );
        for arg in ["commons", "Commons"] {
            match resolve_use_target(&l, arg, "https://p.example").unwrap() {
                UseTarget::BrowserHandoff { name, url } => {
                    assert_eq!(name, "The Commons");
                    assert_eq!(url, "https://commons.example");
                }
                other => panic!("expected a browser hand-off, got {other:?}"),
            }
        }
    }

    #[test]
    fn use_explains_why_the_commons_cannot_be_reached() {
        let message = |lookup: CommonsLookup| {
            resolve_use_target(
                &listing(vec![instance("inst_1")], lookup),
                "commons",
                "https://p.example",
            )
            .unwrap_err()
            .to_string()
        };
        assert!(message(CommonsLookup::Found(commons(false, Some("box_1")))).contains("not joined"));
        assert!(message(CommonsLookup::Absent).contains("does not advertise a Commons"));
        assert!(message(CommonsLookup::Unavailable).contains("could not be looked up"));

        let unknown = resolve_use_target(
            &listing(vec![instance("inst_1")], CommonsLookup::Absent),
            "nope",
            "https://p.example",
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("no instance matches 'nope'"), "{unknown}");
    }

    #[tokio::test]
    async fn a_commons_answer_that_beats_the_list_is_used() {
        let (list, commons) = list_then_commons(
            async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok::<_, anyhow::Error>(1)
            },
            async { Ok::<_, anyhow::Error>("commons") },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(list.unwrap(), 1);
        assert_eq!(commons.unwrap().unwrap(), "commons");
    }

    #[tokio::test]
    async fn a_slow_commons_is_abandoned_after_the_grace() {
        let started = std::time::Instant::now();
        let (list, commons) = list_then_commons(
            async { Ok::<_, anyhow::Error>(1) },
            async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<_, anyhow::Error>("commons")
            },
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(list.unwrap(), 1);
        assert!(commons.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_failed_commons_does_not_fail_the_list() {
        let (list, commons) = list_then_commons(
            async { Ok::<_, anyhow::Error>(1) },
            async { Err::<&str, _>(anyhow!("boom")) },
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(list.unwrap(), 1);
        assert!(commons.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_failed_list_returns_without_waiting_on_the_commons() {
        let started = std::time::Instant::now();
        let (list, commons) = list_then_commons(
            async { Err::<i32, _>(anyhow!("list down")) },
            async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<_, anyhow::Error>("commons")
            },
            Duration::from_secs(30),
        )
        .await;
        assert!(list.is_err());
        assert!(commons.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_commons_failure_is_recorded_for_debug_and_becomes_unavailable() {
        let grace = Duration::from_secs(2);

        let failed = Some(Err(anyhow!("HTTP 500")));
        let record = commons_failure_record(&failed, grace).expect("a failure is recorded");
        assert!(record.contains("commons lookup failed"), "{record}");
        assert!(record.contains("HTTP 500"), "{record}");
        assert!(matches!(
            settle_commons(failed, grace),
            CommonsLookup::Unavailable
        ));

        let timed_out = None;
        let record = commons_failure_record(&timed_out, grace).expect("a timeout is recorded");
        assert!(record.contains("no answer within"), "{record}");
        assert!(matches!(
            settle_commons(timed_out, grace),
            CommonsLookup::Unavailable
        ));

        // Success records nothing, whether or not there is a Commons.
        assert!(commons_failure_record(&Some(Ok(None)), grace).is_none());
        assert!(matches!(
            settle_commons(Some(Ok(None)), grace),
            CommonsLookup::Absent
        ));
        let found = Some(Ok(Some(commons(true, None))));
        assert!(commons_failure_record(&found, grace).is_none());
        assert!(matches!(
            settle_commons(found, grace),
            CommonsLookup::Found(_)
        ));
    }

    mod platform {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn server_with_one_instance() -> MockServer {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/forge-instances"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "instances": [{ "id": "inst_1", "displayName": "Mine", "slug": "mine" }],
                })))
                .mount(&server)
                .await;
            server
        }

        #[tokio::test]
        async fn a_commons_server_error_leaves_the_instance_list_intact() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
                .mount(&server)
                .await;

            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .expect("the instance list must not fail with the Commons");
            assert_eq!(l.instances.len(), 1);
            assert!(matches!(l.commons, CommonsLookup::Unavailable));
            assert_eq!(list_rows(&l, None).len(), 1);
        }

        #[tokio::test]
        async fn an_undecodable_commons_leaves_the_instance_list_intact() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({ "commons": { "name": 7 } })),
                )
                .mount(&server)
                .await;

            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(l.instances.len(), 1);
            assert!(matches!(l.commons, CommonsLookup::Unavailable));
        }

        #[tokio::test]
        async fn an_expired_session_on_the_commons_alone_does_not_fail_the_list() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;

            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(l.instances.len(), 1);
            assert!(matches!(l.commons, CommonsLookup::Unavailable));
        }

        #[tokio::test]
        async fn a_slow_commons_does_not_hold_the_instance_list() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_secs(20))
                        .set_body_json(serde_json::json!({ "commons": null })),
                )
                .mount(&server)
                .await;

            let started = std::time::Instant::now();
            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_millis(100))
                .await
                .unwrap();
            assert!(started.elapsed() < Duration::from_secs(10));
            assert_eq!(l.instances.len(), 1);
            assert!(matches!(l.commons, CommonsLookup::Unavailable));
        }

        #[tokio::test]
        async fn a_platform_without_a_commons_is_absent_not_unavailable() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;

            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .unwrap();
            assert!(matches!(l.commons, CommonsLookup::Absent));
        }

        #[tokio::test]
        async fn a_joined_browser_only_commons_is_co_presented() {
            let server = server_with_one_instance().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "commons": {
                        "url": "https://commons.example",
                        "name": "The Commons",
                        "memberCount": 7,
                        "gatewayForgeId": null,
                        "membership": { "joined": true },
                    },
                })))
                .mount(&server)
                .await;

            let l = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(l.instances.len(), 1);
            let rows = list_rows(&l, None);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[1][6], BROWSER_ONLY);
        }

        #[tokio::test]
        async fn a_failed_instance_list_is_still_an_error() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/forge-instances"))
                .respond_with(ResponseTemplate::new(500).set_body_string("down"))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me/commons"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;

            let err = fetch_listing_with_grace(&server.uri(), "bearer", Duration::from_secs(5))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("HTTP 500"), "{err}");
        }
    }
}
