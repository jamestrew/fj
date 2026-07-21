//! `fj instances`: list the Fjord Account user's Forgejo instances and set
//! the default one to route all other commands through.
//!
//! These calls go to fjord-platform (the gateway), not to a Forgejo instance.
//! See `crate::fjord` for the underlying HTTP layer.

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};

use crate::auth as token_store;
use crate::config::hosts::{AuthKind, Hosts};
use crate::fjord::{self, ForgeInstance};
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
    /// List the Forgejo instances owned by this Fjord Account.
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
    /// Instance id or slug (as printed by `fj instances list`).
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

/// List the account's forge instances, transparently refreshing the session
/// once if the bearer has expired. On an OIDC (SSO) host a stored refresh token
/// renews the access token silently and the call is retried; without one, the
/// original 401 (`SessionExpired`) surfaces so the user re-runs
/// `fj auth login --fjord`.
#[cfg_attr(coverage_nightly, coverage(off))]
async fn fetch_instances(platform_url: &str, bearer: &str) -> Result<Vec<ForgeInstance>> {
    match fjord::list_instances(platform_url, bearer).await {
        Ok(instances) => Ok(instances),
        Err(e) => {
            let expired = e.downcast_ref::<fjord::SessionExpired>().is_some();
            if expired && fjord::oidc::valid_refresh_token(platform_url, bearer)?.is_some() {
                let fresh = fjord::oidc::refresh_session(platform_url)
                    .await
                    .with_context(|| format!("{e}; automatic session refresh failed"))?;
                fjord::list_instances(platform_url, &fresh).await
            } else {
                Err(e)
            }
        }
    }
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

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(host_flag: Option<&str>, json: bool) -> Result<()> {
    let (_name, cfg, bearer) = resolve_fjord(host_flag)?;
    let fa = cfg
        .fjord_account
        .as_ref()
        .expect("resolve_fjord guarantees fjord_account is Some");
    let instances = fetch_instances(&fa.platform_url, &bearer)
        .await
        .with_context(|| format!("listing forge instances at {}", fa.platform_url))?;

    if json {
        return output::print_json(&serde_json::to_value(serializable_list(&instances))?);
    }

    if instances.is_empty() {
        println!(
            "(no Forgejo instances yet for this account at {})",
            fa.platform_url
        );
        println!(
            "{}",
            output::dim(
                "Provision one through the Fjord Account dashboard, then re-run `fj instances`."
            )
        );
        return Ok(());
    }

    let current_default = fa.default_instance_id.as_deref();
    let rows: Vec<Vec<String>> = instances
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
    let table = output::render_table(
        &["", "ID", "NAME", "SLUG", "REGION", "PLAN", "STATE", "URL"],
        &rows,
    );
    print!("{table}");
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
            })
        })
        .collect()
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
    let instances = fetch_instances(&fa.platform_url, &bearer)
        .await
        .with_context(|| format!("listing forge instances at {}", fa.platform_url))?;
    let target = instances
        .iter()
        .find(|i| i.id == args.id_or_slug || i.slug == args.id_or_slug)
        .ok_or_else(|| {
            anyhow!(
                "no instance matches '{}'. Run `fj instances list` to see what's available.",
                args.id_or_slug
            )
        })?;

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
}
