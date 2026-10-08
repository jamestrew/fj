//! `fj profile`: the signed-in account's profile fields, email addresses,
//! and avatar.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use reqwest::StatusCode;

use crate::api::user::{self, Email, Settings};
use crate::client::{ApiError, Client};
use crate::output;

#[derive(Debug, Args)]
pub struct ProfileCmd {
    #[command(subcommand)]
    pub command: ProfileSub,
}

#[derive(Debug, Subcommand)]
pub enum ProfileSub {
    /// Show your profile fields and privacy settings.
    View(ViewArgs),
    /// Edit profile fields. Only the flags you pass are sent.
    Edit(EditArgs),
    /// Manage the email addresses on your account.
    Email(EmailCmd),
    /// Update or remove your avatar.
    Avatar(AvatarCmd),
}

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Emit JSON instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Display name
    #[arg(long)]
    pub full_name: Option<String>,
    /// Biography shown on your profile
    #[arg(long)]
    pub bio: Option<String>,
    /// Website URL
    #[arg(long)]
    pub website: Option<String>,
    /// Location
    #[arg(long)]
    pub location: Option<String>,
    /// Pronouns
    #[arg(long)]
    pub pronouns: Option<String>,
    /// Hide your email address from other users
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    pub hide_email: Option<bool>,
    /// Hide your activity from other users
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    pub hide_activity: Option<bool>,
    /// Hide your pronouns from signed-out visitors
    #[arg(long, value_name = "BOOL", action = clap::ArgAction::Set)]
    pub hide_pronouns: Option<bool>,
    /// Emit the server's updated settings as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EmailCmd {
    #[command(subcommand)]
    pub command: EmailSub,
}

#[derive(Debug, Subcommand)]
pub enum EmailSub {
    /// List your email addresses with their verified and primary state.
    List(EmailListArgs),
    /// Add one or more email addresses.
    Add(EmailAddArgs),
    /// Remove one or more email addresses.
    Remove(EmailRemoveArgs),
}

#[derive(Debug, Args)]
pub struct EmailListArgs {
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EmailAddArgs {
    /// Addresses to add
    #[arg(required = true)]
    pub addresses: Vec<String>,
    /// Emit the addresses the server reported back as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct EmailRemoveArgs {
    /// Addresses to remove
    #[arg(required = true)]
    pub addresses: Vec<String>,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct AvatarCmd {
    #[command(subcommand)]
    pub command: AvatarSub,
}

#[derive(Debug, Subcommand)]
pub enum AvatarSub {
    /// Upload an image file (PNG, JPEG, GIF or WebP) as your avatar.
    Set(AvatarSetArgs),
    /// Remove your avatar.
    Delete(AvatarDeleteArgs),
}

#[derive(Debug, Args)]
pub struct AvatarSetArgs {
    /// Path to the image file
    pub file: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub struct AvatarDeleteArgs {
    /// Skip the confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: ProfileCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        ProfileSub::View(args) => view(&client, args).await,
        ProfileSub::Edit(args) => edit(&client, args).await,
        ProfileSub::Email(c) => match c.command {
            EmailSub::List(args) => email_list(&client, args).await,
            EmailSub::Add(args) => email_add(&client, args).await,
            EmailSub::Remove(args) => email_remove(&client, args).await,
        },
        ProfileSub::Avatar(c) => match c.command {
            AvatarSub::Set(args) => avatar_set(&client, args).await,
            AvatarSub::Delete(args) => avatar_delete(&client, args).await,
        },
    }
}

/// Put the forge's own words in the headline when it refuses an account
/// change (a policy that forbids editing the profile, an email outside the
/// domain allow-list, a missing token scope). The generic 403/422 headlines
/// drop or bury that message. 401 and 429 keep their standard wording.
fn refusal_context(err: anyhow::Error, what: &str) -> anyhow::Error {
    let refusal = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .filter(|api| {
            api.status.is_client_error()
                && api.status != StatusCode::UNAUTHORIZED
                && api.status != StatusCode::TOO_MANY_REQUESTS
        })
        .map(|api| (api.status.as_u16(), api.message.trim().to_string()));
    match refusal {
        Some((status, message)) if message.is_empty() => err.context(format!(
            "{what}: the forge refused (HTTP {status}) without giving a reason"
        )),
        Some((status, message)) => err.context(format!(
            "{what}: the forge refused (HTTP {status}): {message}"
        )),
        None => err,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(client: &Client, args: ViewArgs) -> Result<()> {
    let settings = user::settings(client)
        .await
        .map_err(|e| refusal_context(e, "reading your profile"))?;
    if args.json {
        return output::print_json(&serde_json::to_value(&settings)?);
    }
    print!("{}", render_settings(&settings));
    Ok(())
}

fn render_settings(s: &Settings) -> String {
    fn text(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "unknown (not reported by the forge)".into(),
            Some("") => "(empty)".into(),
            Some(value) => value.to_string(),
        }
    }
    fn flag(v: Option<bool>) -> String {
        match v {
            None => "unknown (not reported by the forge)".into(),
            Some(true) => "yes".into(),
            Some(false) => "no".into(),
        }
    }
    [
        ("full name", text(&s.full_name)),
        ("bio", text(&s.description)),
        ("website", text(&s.website)),
        ("location", text(&s.location)),
        ("pronouns", text(&s.pronouns)),
        ("hide email", flag(s.hide_email)),
        ("hide activity", flag(s.hide_activity)),
        ("hide pronouns", flag(s.hide_pronouns)),
    ]
    .iter()
    .map(|(label, value)| format!("{:<14} {value}\n", format!("{label}:")))
    .collect()
}

/// The `PATCH` body: exactly the fields the user passed.
fn build_edit(args: &EditArgs) -> Result<Settings> {
    let body = Settings {
        full_name: args.full_name.clone(),
        description: args.bio.clone(),
        website: args.website.clone(),
        location: args.location.clone(),
        pronouns: args.pronouns.clone(),
        hide_email: args.hide_email,
        hide_activity: args.hide_activity,
        hide_pronouns: args.hide_pronouns,
    };
    if body == Settings::default() {
        bail!(
            "nothing to change: pass at least one of --full-name, --bio, --website, \
             --location, --pronouns, --hide-email, --hide-activity, --hide-pronouns"
        );
    }
    Ok(body)
}

/// Requested fields the server's response does not reflect. An accepted
/// request is not proof the change was applied.
fn unapplied(requested: &Settings, confirmed: &Settings) -> Vec<&'static str> {
    let mut out = Vec::new();
    macro_rules! check {
        ($field:ident, $label:literal) => {
            if requested.$field.is_some() && requested.$field != confirmed.$field {
                out.push($label);
            }
        };
    }
    check!(full_name, "full name");
    check!(description, "bio");
    check!(website, "website");
    check!(location, "location");
    check!(pronouns, "pronouns");
    check!(hide_email, "hide email");
    check!(hide_activity, "hide activity");
    check!(hide_pronouns, "hide pronouns");
    out
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn edit(client: &Client, args: EditArgs) -> Result<()> {
    let body = build_edit(&args)?;
    let confirmed = user::edit_settings(client, &body)
        .await
        .map_err(|e| refusal_context(e, "updating your profile"))?;
    let kept = unapplied(&body, &confirmed);
    if !kept.is_empty() {
        eprintln!(
            "warning: the forge accepted the request but its response does not show the \
             requested value for: {}",
            kept.join(", ")
        );
    }
    if args.json {
        return output::print_json(&serde_json::to_value(&confirmed)?);
    }
    println!("✓ Profile as the forge now reports it:");
    print!("{}", render_settings(&confirmed));
    Ok(())
}

fn verified_label(verified: Option<bool>) -> &'static str {
    match verified {
        Some(true) => "verified",
        Some(false) => "unverified",
        None => "unknown",
    }
}

fn email_rows(emails: &[Email]) -> Vec<Vec<String>> {
    emails
        .iter()
        .map(|e| {
            vec![
                e.email.clone(),
                verified_label(e.verified).to_string(),
                match e.primary {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                }
                .to_string(),
            ]
        })
        .collect()
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn email_list(client: &Client, args: EmailListArgs) -> Result<()> {
    let emails = user::emails(client)
        .await
        .map_err(|e| refusal_context(e, "listing your email addresses"))?;
    if args.json {
        return output::print_json(&serde_json::to_value(&emails)?);
    }
    if emails.is_empty() {
        println!("(no email addresses)");
        return Ok(());
    }
    print!(
        "{}",
        output::render_table(&["EMAIL", "VERIFIED", "PRIMARY"], &email_rows(&emails))
    );
    Ok(())
}

/// Reject input that cannot be an address before it reaches the server.
/// Anything subtler (allow-lists, duplicates) is the forge's call.
fn check_addresses(addresses: &[String]) -> Result<()> {
    for address in addresses {
        let ok = address
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
            && !address.chars().any(char::is_whitespace);
        if !ok {
            bail!("'{address}' is not an email address");
        }
    }
    Ok(())
}

/// One line per requested address with the state the server reported for it.
/// An address the forge accepted is shown as verified only when the forge
/// says so.
fn added_report(requested: &[String], returned: &[Email]) -> Vec<String> {
    requested
        .iter()
        .map(|address| {
            let found = returned
                .iter()
                .find(|e| e.email.eq_ignore_ascii_case(address));
            match found.map(|e| e.verified) {
                Some(Some(true)) => {
                    format!("{address}: added, verified (as reported by the forge)")
                }
                Some(Some(false)) => {
                    format!("{address}: added, unverified (pending verification by the forge)")
                }
                Some(None) => {
                    format!("{address}: added, verification state not reported by the forge")
                }
                None => format!(
                    "{address}: accepted, but absent from the forge's response; \
                     run `fj profile email list` for its state"
                ),
            }
        })
        .collect()
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn email_add(client: &Client, args: EmailAddArgs) -> Result<()> {
    check_addresses(&args.addresses)?;
    let returned = user::add_emails(client, &args.addresses)
        .await
        .map_err(|e| refusal_context(e, "adding the email address"))?;
    if args.json {
        return output::print_json(&serde_json::to_value(&returned)?);
    }
    for line in added_report(&args.addresses, &returned) {
        println!("{line}");
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn email_remove(client: &Client, args: EmailRemoveArgs) -> Result<()> {
    check_addresses(&args.addresses)?;
    let list = args.addresses.join(", ");
    super::admin::confirm_typed(
        args.yes,
        &format!("Remove {list} from your account?"),
        "remove",
    )?;
    user::delete_emails(client, &args.addresses)
        .await
        .map_err(|e| refusal_context(e, "removing the email address"))?;
    println!("✓ Removed {list}");
    Ok(())
}

/// Largest avatar file fj will send. Forgejo's own default limit is 1 MiB
/// and is configurable per forge; this cap only stops an obviously wrong
/// file from being base64-encoded into a request. The forge's limit still
/// applies and is reported in its own words.
const MAX_AVATAR_BYTES: u64 = 8 * 1024 * 1024;

/// Recognize the image formats Forgejo accepts for avatars by magic bytes.
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("PNG")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("JPEG")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("GIF")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("WebP")
    } else {
        None
    }
}

/// Read and validate an avatar file: it must exist, be a regular non-empty
/// file within the size cap, and look like a supported image.
fn load_avatar(path: &Path) -> Result<(Vec<u8>, &'static str)> {
    let shown = path.display();
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {shown}"))?;
    if !meta.is_file() {
        bail!("{shown} is not a regular file");
    }
    if meta.len() == 0 {
        bail!("{shown} is empty");
    }
    if meta.len() > MAX_AVATAR_BYTES {
        bail!(
            "{shown} is {} bytes; fj sends avatars up to {MAX_AVATAR_BYTES} bytes",
            meta.len()
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {shown}"))?;
    let kind = sniff_image(&bytes)
        .ok_or_else(|| anyhow!("{shown} does not look like a PNG, JPEG, GIF or WebP image"))?;
    Ok((bytes, kind))
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn avatar_set(client: &Client, args: AvatarSetArgs) -> Result<()> {
    use base64::Engine;
    let (bytes, kind) = load_avatar(&args.file)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    user::update_avatar(client, &encoded)
        .await
        .map_err(|e| refusal_context(e, "updating your avatar"))?;
    println!(
        "✓ The forge accepted the {kind} image ({} bytes) as your avatar",
        bytes.len()
    );
    // Best effort: show where the forge now serves it from.
    if let Ok(me) = user::current(client).await {
        if !me.avatar_url.is_empty() {
            println!("  avatar: {}", me.avatar_url);
        }
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn avatar_delete(client: &Client, args: AvatarDeleteArgs) -> Result<()> {
    super::admin::confirm_typed(args.yes, "Remove your avatar?", "remove")?;
    user::delete_avatar(client)
        .await
        .map_err(|e| refusal_context(e, "removing your avatar"))?;
    println!("✓ The forge removed your avatar");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::io::Write;

    fn api_error(status: StatusCode, message: &str) -> anyhow::Error {
        anyhow::Error::new(ApiError {
            status,
            url: "https://forge.example/api/v1/user/emails".into(),
            message: message.into(),
            body: String::new(),
        })
    }

    fn edit_args(argv: &[&str]) -> EditArgs {
        let mut full = vec!["fj", "profile", "edit"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full).expect("parses").command {
            Command::Profile(ProfileCmd {
                command: ProfileSub::Edit(args),
            }) => args,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    fn email(address: &str, verified: Option<bool>, primary: Option<bool>) -> Email {
        Email {
            email: address.into(),
            verified,
            primary,
        }
    }

    #[test]
    fn edit_sends_only_the_fields_that_were_passed() {
        let body = build_edit(&edit_args(&["--bio", "hello", "--hide-email", "true"])).unwrap();
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({"description": "hello", "hide_email": true})
        );
    }

    #[test]
    fn edit_can_clear_a_field_and_turn_a_toggle_off() {
        let body = build_edit(&edit_args(&["--website", "", "--hide-activity", "false"])).unwrap();
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({"website": "", "hide_activity": false})
        );
    }

    #[test]
    fn edit_with_no_fields_is_refused_before_any_request() {
        let err = build_edit(&edit_args(&[])).unwrap_err();
        assert!(err.to_string().contains("nothing to change"), "{err}");
    }

    #[test]
    fn unapplied_names_fields_the_server_did_not_change() {
        let requested = Settings {
            full_name: Some("New Name".into()),
            location: Some("Oslo".into()),
            hide_email: Some(true),
            ..Settings::default()
        };
        let confirmed = Settings {
            full_name: Some("Old Name".into()),
            location: Some("Oslo".into()),
            website: Some("https://example.com".into()),
            ..Settings::default()
        };
        assert_eq!(
            unapplied(&requested, &confirmed),
            vec!["full name", "hide email"]
        );
        assert!(unapplied(&requested, &requested).is_empty());
    }

    #[test]
    fn settings_view_distinguishes_empty_from_unreported() {
        let text = render_settings(&Settings {
            full_name: Some("Alice".into()),
            description: Some(String::new()),
            hide_email: Some(true),
            hide_activity: Some(false),
            ..Settings::default()
        });
        assert!(text.contains("full name:     Alice\n"), "{text}");
        assert!(text.contains("bio:           (empty)\n"), "{text}");
        assert!(
            text.contains("website:       unknown (not reported by the forge)\n"),
            "{text}"
        );
        assert!(text.contains("hide email:    yes\n"), "{text}");
        assert!(text.contains("hide activity: no\n"), "{text}");
        assert!(
            text.contains("hide pronouns: unknown (not reported by the forge)\n"),
            "{text}"
        );
    }

    #[test]
    fn email_rows_show_actual_verified_and_primary_state() {
        let rows = email_rows(&[
            email("a@example.com", Some(true), Some(true)),
            email("b@example.com", Some(false), Some(false)),
            email("c@example.com", None, None),
        ]);
        assert_eq!(rows[0], vec!["a@example.com", "verified", "yes"]);
        assert_eq!(rows[1], vec!["b@example.com", "unverified", "no"]);
        assert_eq!(rows[2], vec!["c@example.com", "unknown", "unknown"]);
    }

    #[test]
    fn an_accepted_address_is_not_reported_as_verified() {
        let requested = vec![
            "New@Example.com".to_string(),
            "done@example.com".to_string(),
            "quiet@example.com".to_string(),
            "gone@example.com".to_string(),
        ];
        let returned = vec![
            email("new@example.com", Some(false), Some(false)),
            email("done@example.com", Some(true), Some(false)),
            email("quiet@example.com", None, None),
        ];
        let lines = added_report(&requested, &returned);
        assert_eq!(
            lines[0],
            "New@Example.com: added, unverified (pending verification by the forge)"
        );
        assert_eq!(
            lines[1],
            "done@example.com: added, verified (as reported by the forge)"
        );
        assert!(lines[2].contains("verification state not reported"));
        assert!(!lines[2].contains("added, verified"));
        assert!(lines[3].contains("absent from the forge's response"));
        assert!(!lines[3].contains("verified"));
    }

    #[test]
    fn obviously_malformed_addresses_are_rejected_locally() {
        assert!(check_addresses(&["a@example.com".into()]).is_ok());
        for bad in ["nope", "@example.com", "a@", "a b@example.com"] {
            assert!(check_addresses(&[bad.to_string()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_policy_refusal_is_headlined_with_the_forges_own_message() {
        let err = refusal_context(
            api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Email address a@blocked.test is not allowed by the domain allowlist",
            ),
            "adding the email address",
        );
        assert_eq!(
            err.to_string(),
            "adding the email address: the forge refused (HTTP 422): \
             Email address a@blocked.test is not allowed by the domain allowlist"
        );

        let err = refusal_context(
            api_error(StatusCode::FORBIDDEN, "changing full name is disabled"),
            "updating your profile",
        );
        assert!(
            err.to_string().ends_with("changing full name is disabled"),
            "{err}"
        );
        assert!(err.chain().any(|c| c.downcast_ref::<ApiError>().is_some()));
    }

    #[test]
    fn refusal_context_leaves_auth_rate_limit_and_server_errors_alone() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
        ] {
            let err = refusal_context(api_error(status, "x"), "updating your profile");
            assert!(!err.to_string().contains("the forge refused"), "{status}");
        }
        let err = refusal_context(
            api_error(StatusCode::FORBIDDEN, " "),
            "removing your avatar",
        );
        assert_eq!(
            err.to_string(),
            "removing your avatar: the forge refused (HTTP 403) without giving a reason"
        );
    }

    #[test]
    fn sniff_image_recognizes_supported_formats_only() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n...."), Some("PNG"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("JPEG"));
        assert_eq!(sniff_image(b"GIF89a..."), Some("GIF"));
        assert_eq!(sniff_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("WebP"));
        assert_eq!(sniff_image(b"<svg xmlns="), None);
        assert_eq!(sniff_image(b"RIFF\x00\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn load_avatar_accepts_a_plausible_image() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"\x89PNG\r\n\x1a\nrest-of-file").unwrap();
        let (bytes, kind) = load_avatar(f.path()).unwrap();
        assert_eq!(kind, "PNG");
        assert_eq!(bytes.len(), 20);
    }

    #[test]
    fn load_avatar_rejects_missing_empty_directory_and_non_image() {
        let dir = tempfile::tempdir().unwrap();

        let err = load_avatar(&dir.path().join("nope.png")).unwrap_err();
        assert!(err.to_string().contains("cannot read"), "{err}");

        let err = load_avatar(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");

        let empty = dir.path().join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        let err = load_avatar(&empty).unwrap_err();
        assert!(err.to_string().contains("is empty"), "{err}");

        let text = dir.path().join("notes.png");
        std::fs::write(&text, b"just some text").unwrap();
        let err = load_avatar(&text).unwrap_err();
        assert!(err.to_string().contains("does not look like"), "{err}");
    }

    #[test]
    fn load_avatar_rejects_an_oversized_file_without_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.png");
        // Sparse: sets the length without writing 8 MiB of data.
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_AVATAR_BYTES + 1).unwrap();
        let err = load_avatar(&big).unwrap_err();
        assert!(err.to_string().contains("fj sends avatars up to"), "{err}");
    }

    #[test]
    fn email_and_avatar_subcommands_parse() {
        let cli = Cli::try_parse_from(["fj", "profile", "email", "remove", "a@example.com", "-y"])
            .expect("remove parses");
        match cli.command {
            Command::Profile(ProfileCmd {
                command:
                    ProfileSub::Email(EmailCmd {
                        command: EmailSub::Remove(args),
                    }),
            }) => assert!(args.yes),
            other => panic!("unexpected command: {other:?}"),
        }
        assert!(Cli::try_parse_from(["fj", "profile", "email", "add"]).is_err());
        assert!(Cli::try_parse_from(["fj", "profile", "avatar", "set", "me.png"]).is_ok());
        assert!(Cli::try_parse_from(["fj", "profile", "avatar", "delete", "--yes"]).is_ok());
    }
}
