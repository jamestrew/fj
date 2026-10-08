//! `fj repo commit`: list a repository's commits at a ref and view one
//! commit, each with the server's signature verification verdict.
//!
//! Also hosts `RefArgs`, the `--ref` / `--branch` / `--tag` selector shared
//! with `fj repo contents`.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api;
use crate::api::commit::{CommitIdentity, ListOptions, RepoCommit, Verification};
use crate::cli::context::{resolve_repo, RepoContext, RepoFlag};
use crate::output;

#[derive(Debug, Args)]
pub struct CommitCmd {
    #[command(subcommand)]
    pub command: CommitSub,
}

#[derive(Debug, Subcommand)]
pub enum CommitSub {
    /// List commits on the default branch, or at a branch, tag, or SHA.
    List(CommitListArgs),
    /// View one commit: message, signature state, stats, and changed files.
    View(CommitViewArgs),
}

/// Which ref to read at. At most one of the three may be given; none means
/// the repository's default branch.
#[derive(Debug, Args, Clone, Default)]
pub struct RefArgs {
    /// Branch, tag, or commit SHA to read at (the server resolves it).
    #[arg(long = "ref", value_name = "REF", conflicts_with_all = ["branch", "tag"])]
    pub git_ref: Option<String>,
    /// Read at this branch.
    #[arg(long, value_name = "NAME", conflicts_with = "tag")]
    pub branch: Option<String>,
    /// Read at this tag. A tag is immutable, so this is always a read-only view.
    #[arg(long, value_name = "NAME")]
    pub tag: Option<String>,
}

#[derive(Debug, Args)]
pub struct CommitListArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    #[command(flatten)]
    pub at: RefArgs,
    /// Only commits that touch this file or directory.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
    /// Maximum number of commits to list.
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CommitViewArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Commit SHA (full or abbreviated).
    pub sha: String,
    /// Emit JSON instead of text
    #[arg(long)]
    pub json: bool,
}

/// The ref the user picked, before any server lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefSelection {
    DefaultBranch,
    Any(String),
    Branch(String),
    Tag(String),
}

impl RefArgs {
    pub(crate) fn selection(&self) -> RefSelection {
        if let Some(tag) = &self.tag {
            RefSelection::Tag(tag.clone())
        } else if let Some(branch) = &self.branch {
            RefSelection::Branch(branch.clone())
        } else if let Some(r) = &self.git_ref {
            RefSelection::Any(r.clone())
        } else {
            RefSelection::DefaultBranch
        }
    }
}

/// A selection resolved against the server: what to send as the ref, and the
/// line that tells the reader what kind of ref they are looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedRef {
    /// Value for the endpoint's ref parameter; `None` means default branch.
    pub query: Option<String>,
    /// Human label, e.g. `Tag v1.0.0 (commit abc1234), read-only`.
    pub label: Option<String>,
}

/// Build the resolved ref. A tag is sent as the commit SHA the server reported
/// for it (`tag_commit`), so a branch that happens to share the tag's name can
/// never be read by mistake; without a reported SHA the tag name is sent.
pub(crate) fn resolved_ref(selection: &RefSelection, tag_commit: Option<&str>) -> ResolvedRef {
    match selection {
        RefSelection::DefaultBranch => ResolvedRef {
            query: None,
            label: None,
        },
        RefSelection::Any(r) => ResolvedRef {
            query: Some(r.clone()),
            label: Some(format!("Ref {r}")),
        },
        RefSelection::Branch(b) => ResolvedRef {
            query: Some(b.clone()),
            label: Some(format!("Branch {b}")),
        },
        RefSelection::Tag(t) => match tag_commit.filter(|sha| !sha.is_empty()) {
            Some(sha) => ResolvedRef {
                query: Some(sha.to_string()),
                label: Some(format!("Tag {t} (commit {}), read-only", short_sha(sha))),
            },
            None => ResolvedRef {
                query: Some(t.clone()),
                label: Some(format!("Tag {t}, read-only")),
            },
        },
    }
}

/// Resolve the selected ref. Only a `--tag` costs a request: the tag is looked
/// up so a missing tag fails as "tag not found" and the read is pinned to the
/// tag's commit.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn resolve_ref(ctx: &RepoContext, at: &RefArgs) -> Result<ResolvedRef> {
    let selection = at.selection();
    let tag_commit = match &selection {
        RefSelection::Tag(tag) => Some(
            api::tag::get(&ctx.client, &ctx.owner, &ctx.name, tag)
                .await?
                .commit
                .sha,
        ),
        _ => None,
    };
    Ok(resolved_ref(&selection, tag_commit.as_deref()))
}

/// What the server said about a commit's signature. `NotReported` is not the
/// same as `Unsigned`: it means the response carried no `verification` object,
/// so nothing is known and nothing is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignatureState {
    /// `verification.verified` is true. The signer is named only here.
    Verified {
        signer: Option<String>,
    },
    /// A signature is present but the server did not verify it.
    Unverified {
        reason: Option<String>,
    },
    Unsigned,
    NotReported,
}

/// Forgejo's and Gitea's reason string for a commit that has no signature.
const REASON_NOT_SIGNED: &str = "gpg.error.not_signed_commit";

/// Decide the signature state from the server's `verification` object alone.
/// The verdict is the server's: a commit is `Verified`, and a signer is
/// named, only when `verified` is true. A signer the payload attaches to an
/// unverified signature is a claim, not an identity, and is dropped.
pub(crate) fn signature_state(verification: Option<&Verification>) -> SignatureState {
    let Some(v) = verification else {
        return SignatureState::NotReported;
    };
    if v.verified {
        return SignatureState::Verified {
            signer: v.signer.as_ref().and_then(signer_identity),
        };
    }
    let has_signature = v.signature.as_deref().is_some_and(|s| !s.trim().is_empty());
    let reason = v.reason.trim();
    // Any reason other than "not signed" describes a signature that failed
    // to verify, even on a server that omits the signature text itself.
    if has_signature || (!reason.is_empty() && reason != REASON_NOT_SIGNED) {
        SignatureState::Unverified {
            reason: (!reason.is_empty() && reason != REASON_NOT_SIGNED).then(|| reason.to_string()),
        }
    } else {
        SignatureState::Unsigned
    }
}

/// `Name <email>`, falling back to the username, then the bare email.
fn signer_identity(signer: &api::commit::Signer) -> Option<String> {
    let name = [signer.name.trim(), signer.username.trim()]
        .into_iter()
        .find(|s| !s.is_empty());
    let email = signer.email.trim();
    match (name, email.is_empty()) {
        (Some(name), false) => Some(format!("{name} <{email}>")),
        (Some(name), true) => Some(name.to_string()),
        (None, false) => Some(email.to_string()),
        (None, true) => None,
    }
}

/// The list cell for a state. Empty for `NotReported`: no state is shown.
fn signature_badge(state: &SignatureState) -> &'static str {
    match state {
        SignatureState::Verified { .. } => "verified",
        SignatureState::Unverified { .. } => "unverified",
        SignatureState::Unsigned => "unsigned",
        SignatureState::NotReported => "",
    }
}

/// The detail lines for a state. Empty for `NotReported`.
fn signature_lines(state: &SignatureState) -> Vec<String> {
    match state {
        SignatureState::Verified { signer } => {
            let mut lines = vec!["Signature: verified".to_string()];
            if let Some(signer) = signer {
                lines.push(format!("Signed by: {signer}"));
            }
            lines
        }
        SignatureState::Unverified { reason } => vec![match reason {
            Some(reason) => format!("Signature: unverified ({reason})"),
            None => "Signature: unverified".to_string(),
        }],
        SignatureState::Unsigned => vec!["Signature: not signed".to_string()],
        SignatureState::NotReported => Vec::new(),
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn identity_line(id: &CommitIdentity) -> String {
    if id.email.is_empty() {
        id.name.clone()
    } else {
        format!("{} <{}>", id.name, id.email)
    }
}

fn relative_date(date: Option<&str>) -> String {
    date.and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|d| output::relative_time(d.with_timezone(&chrono::Utc)))
        .unwrap_or_default()
}

/// Table for the commit list. The SIGNATURE column is dropped entirely when
/// no commit reported verification, rather than showing a column of blanks
/// that could be read as "unsigned".
fn commit_table(commits: &[RepoCommit]) -> (Vec<&'static str>, Vec<Vec<String>>) {
    let states: Vec<SignatureState> = commits
        .iter()
        .map(|c| signature_state(c.commit.verification.as_ref()))
        .collect();
    let any_reported = states.iter().any(|s| *s != SignatureState::NotReported);
    let mut headers = vec!["SHA"];
    if any_reported {
        headers.push("SIGNATURE");
    }
    headers.extend(["SUBJECT", "AUTHOR", "WHEN"]);
    let rows = commits
        .iter()
        .zip(&states)
        .map(|(c, state)| {
            let author = c.commit.author.as_ref();
            let mut row = vec![short_sha(&c.sha).to_string()];
            if any_reported {
                row.push(signature_badge(state).to_string());
            }
            row.push(output::truncate(
                c.commit.message.lines().next().unwrap_or(""),
                64,
            ));
            row.push(output::dim(
                &author.map(|a| a.name.clone()).unwrap_or_default(),
            ));
            row.push(output::dim(&relative_date(
                author.and_then(|a| a.date.as_deref()),
            )));
            row
        })
        .collect();
    (headers, rows)
}

/// The text body of `fj repo commit view`.
fn commit_detail(c: &RepoCommit) -> String {
    let mut out = vec![format!("commit {}", c.sha)];
    out.extend(signature_lines(&signature_state(
        c.commit.verification.as_ref(),
    )));
    if let Some(author) = &c.commit.author {
        out.push(format!("Author:    {}", identity_line(author)));
        if let Some(date) = author.date.as_deref().filter(|d| !d.is_empty()) {
            out.push(format!("Date:      {date}"));
        }
    }
    if let Some(committer) = &c.commit.committer {
        let same = c
            .commit
            .author
            .as_ref()
            .is_some_and(|a| a.name == committer.name && a.email == committer.email);
        if !same {
            out.push(format!("Committer: {}", identity_line(committer)));
        }
    }
    if !c.parents.is_empty() {
        let parents: Vec<&str> = c.parents.iter().map(|p| short_sha(&p.sha)).collect();
        out.push(format!("Parents:   {}", parents.join(" ")));
    }
    if !c.html_url.is_empty() {
        out.push(format!("URL:       {}", c.html_url));
    }
    out.push(String::new());
    for line in c.commit.message.trim_end().lines() {
        out.push(format!("    {line}"));
    }
    if let Some(files) = c.files.as_ref().filter(|f| !f.is_empty()) {
        out.push(String::new());
        let stats = c
            .stats
            .as_ref()
            .map(|s| format!(", +{} -{}", s.additions, s.deletions))
            .unwrap_or_default();
        let noun = if files.len() == 1 { "file" } else { "files" };
        out.push(format!("{} {noun} changed{stats}", files.len()));
        for f in files {
            out.push(format!("  {:<9} {}", f.status, f.filename));
        }
    }
    out.join("\n")
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn commit(cmd: CommitCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        CommitSub::List(args) => list(args, host).await,
        CommitSub::View(args) => view(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn list(args: CommitListArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let at = resolve_ref(&ctx, &args.at).await?;
    let commits = api::commit::list(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        &ListOptions {
            start: at.query.as_deref(),
            path: args.path.as_deref(),
            limit: args.limit.max(1) as usize,
        },
    )
    .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&commits)?);
    }
    if let Some(label) = &at.label {
        println!("{}", output::dim(label));
    }
    if commits.is_empty() {
        println!("(no commits)");
        return Ok(());
    }
    let (headers, rows) = commit_table(&commits);
    print!("{}", output::render_table(&headers, &rows));
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn view(args: CommitViewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let c = api::commit::get(&ctx.client, &ctx.owner, &ctx.name, &args.sha).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&c)?);
    }
    println!("{}", commit_detail(&c));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verification(v: serde_json::Value) -> Verification {
        serde_json::from_value(v).expect("verification payload")
    }

    fn commit(v: serde_json::Value) -> RepoCommit {
        serde_json::from_value(v).expect("commit payload")
    }

    // Shape Forgejo 16 returns for a commit signed with a registered key.
    fn forgejo_verified() -> serde_json::Value {
        json!({
            "verified": true,
            "reason": "mara@example.com / 1A2B3C4D5E6F7A8B",
            "signature": "-----BEGIN PGP SIGNATURE-----\n...\n-----END PGP SIGNATURE-----",
            "signer": { "name": "Mara Lind", "email": "mara@example.com", "username": "mara" },
            "payload": "tree abc\n"
        })
    }

    // Shape Gitea 1.2x returns for a commit with no signature.
    fn gitea_unsigned() -> serde_json::Value {
        json!({
            "verified": false,
            "reason": "gpg.error.not_signed_commit",
            "signature": "",
            "signer": null,
            "payload": ""
        })
    }

    // Shape Gitea 1.2x returns for a commit signed with an unregistered key.
    fn gitea_unverified() -> serde_json::Value {
        json!({
            "verified": false,
            "reason": "gpg.error.no_gpg_keys_found",
            "signature": "-----BEGIN SSH SIGNATURE-----\n...\n-----END SSH SIGNATURE-----",
            "signer": null,
            "payload": "tree abc\n"
        })
    }

    #[test]
    fn verified_commit_names_the_signer() {
        assert_eq!(
            signature_state(Some(&verification(forgejo_verified()))),
            SignatureState::Verified {
                signer: Some("Mara Lind <mara@example.com>".into())
            }
        );
    }

    #[test]
    fn signed_but_unverified_commit_is_unverified_with_the_server_reason() {
        assert_eq!(
            signature_state(Some(&verification(gitea_unverified()))),
            SignatureState::Unverified {
                reason: Some("gpg.error.no_gpg_keys_found".into())
            }
        );
    }

    #[test]
    fn unsigned_commit_is_unsigned() {
        assert_eq!(
            signature_state(Some(&verification(gitea_unsigned()))),
            SignatureState::Unsigned
        );
        // A bare `{verified: false}` with nothing else is also unsigned.
        assert_eq!(
            signature_state(Some(&verification(json!({ "verified": false })))),
            SignatureState::Unsigned
        );
    }

    #[test]
    fn missing_verification_object_is_not_reported_rather_than_unsigned() {
        assert_eq!(signature_state(None), SignatureState::NotReported);
        let c = commit(json!({ "sha": "abc1234def", "commit": { "message": "m" } }));
        assert_eq!(
            signature_state(c.commit.verification.as_ref()),
            SignatureState::NotReported
        );
    }

    #[test]
    fn unverified_signature_never_names_a_claimed_signer() {
        let mut payload = gitea_unverified();
        payload["signer"] = json!({ "name": "Mallory", "email": "mallory@example.com" });
        let state = signature_state(Some(&verification(payload)));
        assert!(matches!(state, SignatureState::Unverified { .. }));
        let text = signature_lines(&state).join("\n");
        assert!(!text.contains("Mallory"), "{text}");
        assert!(!text.contains("Signed by"), "{text}");
        assert!(!text.contains("Signature: verified"), "{text}");
    }

    #[test]
    fn unverified_without_signature_text_still_reads_as_unverified() {
        let state = signature_state(Some(&verification(json!({
            "verified": false, "reason": "gpg.error.no_committer_account"
        }))));
        assert!(matches!(state, SignatureState::Unverified { .. }));
    }

    #[test]
    fn signer_identity_falls_back_to_username_then_email_then_nothing() {
        let state = |signer: serde_json::Value| {
            signature_state(Some(&verification(
                json!({ "verified": true, "signer": signer }),
            )))
        };
        assert_eq!(
            state(json!({ "username": "mara", "email": "m@example.com" })),
            SignatureState::Verified {
                signer: Some("mara <m@example.com>".into())
            }
        );
        assert_eq!(
            state(json!({ "email": "m@example.com" })),
            SignatureState::Verified {
                signer: Some("m@example.com".into())
            }
        );
        assert_eq!(
            state(json!(null)),
            SignatureState::Verified { signer: None }
        );
        assert_eq!(
            signature_lines(&SignatureState::Verified { signer: None }),
            vec!["Signature: verified".to_string()]
        );
    }

    #[test]
    fn signature_lines_cover_each_state() {
        assert_eq!(
            signature_lines(&signature_state(Some(&verification(forgejo_verified())))),
            vec![
                "Signature: verified".to_string(),
                "Signed by: Mara Lind <mara@example.com>".to_string()
            ]
        );
        assert_eq!(
            signature_lines(&SignatureState::Unsigned),
            vec!["Signature: not signed".to_string()]
        );
        assert!(signature_lines(&SignatureState::NotReported).is_empty());
    }

    fn list_fixture(verifications: &[Option<serde_json::Value>]) -> Vec<RepoCommit> {
        verifications
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut c = json!({
                    "sha": format!("{i}aaaaaaaaaaaa"),
                    "commit": {
                        "message": format!("subject {i}\n\nbody"),
                        "author": { "name": "Ada", "email": "a@example.com",
                                    "date": "2026-01-01T00:00:00Z" }
                    }
                });
                if let Some(v) = v {
                    c["commit"]["verification"] = v.clone();
                }
                commit(c)
            })
            .collect()
    }

    #[test]
    fn commit_table_marks_each_row_and_leaves_unreported_rows_blank() {
        let commits = list_fixture(&[
            Some(forgejo_verified()),
            Some(gitea_unverified()),
            Some(gitea_unsigned()),
            None,
        ]);
        let (headers, rows) = commit_table(&commits);
        assert_eq!(headers, ["SHA", "SIGNATURE", "SUBJECT", "AUTHOR", "WHEN"]);
        let badges: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(badges, ["verified", "unverified", "unsigned", ""]);
        assert_eq!(rows[0][0], "0aaaaaa");
        assert_eq!(rows[0][2], "subject 0");
    }

    #[test]
    fn commit_table_drops_the_signature_column_when_nothing_was_reported() {
        let (headers, rows) = commit_table(&list_fixture(&[None, None]));
        assert_eq!(headers, ["SHA", "SUBJECT", "AUTHOR", "WHEN"]);
        assert_eq!(rows[0].len(), 4);
    }

    #[test]
    fn commit_detail_shows_signature_stats_and_files() {
        let c = commit(json!({
            "sha": "abcdef0123456789",
            "html_url": "https://forge.example/o/r/commit/abcdef0123456789",
            "commit": {
                "message": "Fix the thing\n\nLonger body.\n",
                "author": { "name": "Ada", "email": "a@example.com",
                            "date": "2026-01-01T00:00:00Z" },
                "committer": { "name": "Bot", "email": "bot@example.com" },
                "verification": forgejo_verified()
            },
            "parents": [{ "sha": "1111111aaaa" }],
            "stats": { "total": 3, "additions": 2, "deletions": 1 },
            "files": [{ "filename": "src/a.rs", "status": "modified" }]
        }));
        let text = commit_detail(&c);
        assert!(text.starts_with("commit abcdef0123456789\nSignature: verified\n"));
        assert!(text.contains("Signed by: Mara Lind <mara@example.com>"));
        assert!(text.contains("Author:    Ada <a@example.com>"));
        assert!(text.contains("Committer: Bot <bot@example.com>"));
        assert!(text.contains("Parents:   1111111"));
        assert!(text.contains("    Fix the thing"));
        assert!(text.contains("1 file changed, +2 -1"));
        assert!(text.contains("modified  src/a.rs"));
    }

    #[test]
    fn commit_detail_shows_no_signature_line_when_not_reported() {
        let c = commit(json!({ "sha": "abc", "commit": { "message": "m" } }));
        let text = commit_detail(&c);
        assert!(!text.contains("Signature"), "{text}");
        assert!(!text.contains("signed"), "{text}");
    }

    #[test]
    fn ref_selection_prefers_the_explicit_kind() {
        let args = |r: Option<&str>, b: Option<&str>, t: Option<&str>| RefArgs {
            git_ref: r.map(Into::into),
            branch: b.map(Into::into),
            tag: t.map(Into::into),
        };
        assert_eq!(
            args(None, None, None).selection(),
            RefSelection::DefaultBranch
        );
        assert_eq!(
            args(Some("abc"), None, None).selection(),
            RefSelection::Any("abc".into())
        );
        assert_eq!(
            args(None, Some("main"), None).selection(),
            RefSelection::Branch("main".into())
        );
        assert_eq!(
            args(None, None, Some("v1")).selection(),
            RefSelection::Tag("v1".into())
        );
    }

    #[test]
    fn a_tag_is_labelled_as_a_tag_and_pinned_to_its_commit() {
        let tag = resolved_ref(&RefSelection::Tag("v1.0.0".into()), Some("abcdef0123456"));
        assert_eq!(tag.query.as_deref(), Some("abcdef0123456"));
        assert_eq!(
            tag.label.as_deref(),
            Some("Tag v1.0.0 (commit abcdef0), read-only")
        );
        // No commit reported: fall back to the tag name, still labelled a tag.
        let bare = resolved_ref(&RefSelection::Tag("v1.0.0".into()), Some(""));
        assert_eq!(bare.query.as_deref(), Some("v1.0.0"));
        assert_eq!(bare.label.as_deref(), Some("Tag v1.0.0, read-only"));

        let branch = resolved_ref(&RefSelection::Branch("main".into()), None);
        assert_eq!(branch.query.as_deref(), Some("main"));
        assert_eq!(branch.label.as_deref(), Some("Branch main"));
        assert_ne!(tag.label, branch.label);

        let default = resolved_ref(&RefSelection::DefaultBranch, None);
        assert_eq!(default.query, None);
        assert_eq!(default.label, None);
    }

    #[test]
    fn ref_flags_are_mutually_exclusive() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            let mut argv = vec!["fj", "repo", "commit", "list"];
            argv.extend_from_slice(extra);
            crate::cli::Cli::try_parse_from(argv)
        };
        assert!(parse(&["--tag", "v1"]).is_ok());
        assert!(parse(&["--branch", "main"]).is_ok());
        assert!(parse(&["--ref", "abc123"]).is_ok());
        assert!(parse(&["--tag", "v1", "--branch", "main"]).is_err());
        assert!(parse(&["--ref", "x", "--tag", "v1"]).is_err());
        assert!(parse(&["--ref", "x", "--branch", "main"]).is_err());
    }
}
