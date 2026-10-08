//! `fj pr review` / `request-review` / `unrequest-review`.

use std::io::Read;

use anyhow::{anyhow, Result};

use crate::api;
use crate::api::pull::LineComment;
use crate::cli::context::resolve_repo;
use crate::cli::editor;
use crate::cli::pr::{ReviewArgs, ReviewerArgs};
use crate::cli::pr_args::ReviewEventArg;
use crate::cli::pr_line_comment::{self, DiffIndex};

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    // Everything about the line comments that can be judged locally is judged
    // before the repo is even resolved, so a typo never reaches the server.
    if args.line_comments_file.as_deref() == Some("-") && args.body.as_deref() == Some("-") {
        return Err(anyhow!(
            "--body - and --line-comments-file - both read stdin; give one of them another source"
        ));
    }
    let file_text = match args.line_comments_file.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Some(buf)
        }
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| anyhow!("could not read line comments file {path}: {e}"))?,
        ),
        None => None,
    };
    let comments = collect_line_comments(&args.line_comment, file_text.as_deref())?;
    let event = resolve_event(args.event, comments.len())?;

    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body_required = matches!(event, ReviewEventArg::RequestChanges);
    let body = match args.body.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Some(buf.trim_end().to_string())
        }
        Some(s) => Some(s.to_string()),
        None => {
            if body_required {
                Some(editor::edit_text("PR_REVIEW.md", "")?)
            } else {
                None
            }
        }
    };
    let body = body.filter(|s| !s.trim().is_empty());
    check_has_content(event, body.is_some(), comments.len())?;

    // Check every anchor against the PR's own diff. This is a read; nothing
    // has been posted yet, and a bad anchor stops the whole review.
    if !comments.is_empty() {
        let diff = api::pull::diff_text(&ctx.client, &ctx.owner, &ctx.name, args.number)
            .await
            .map_err(|e| {
                anyhow!(
                    "could not read the diff of #{} to check the line comments: {e}",
                    args.number
                )
            })?;
        let index = DiffIndex::parse(&diff);
        for c in &comments {
            index.check(c, args.number)?;
        }
    }

    // One request either way: the line comments ride in the review itself.
    let r = if comments.is_empty() {
        api::pull::submit_review(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            event.into(),
            body.as_deref(),
        )
        .await?
    } else {
        api::pull::submit_review_with_comments(
            &ctx.client,
            &ctx.owner,
            &ctx.name,
            args.number,
            event.into(),
            body.as_deref(),
            &comments,
        )
        .await?
    };
    let (headline, warning) =
        pr_line_comment::submitted_lines(&r.state, args.number, comments.len(), r.comments_count);
    println!("{headline}");
    println!("{}", r.html_url);
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    Ok(())
}

/// Line comments from both sources, flags first, in the order given.
fn collect_line_comments(flags: &[String], file_text: Option<&str>) -> Result<Vec<LineComment>> {
    let mut out = pr_line_comment::from_flags(flags)?;
    if let Some(text) = file_text {
        out.extend(pr_line_comment::from_json(text)?);
    }
    Ok(out)
}

/// The review event to send. With no `--event`, line comments go out as a
/// plain `comment` review: comments on their own, no verdict.
fn resolve_event(event: Option<ReviewEventArg>, comments: usize) -> Result<ReviewEventArg> {
    match event {
        Some(e) => Ok(e),
        None if comments > 0 => Ok(ReviewEventArg::Comment),
        None => Err(anyhow!(
            "no line comments were given, so --event is required (approve, request-changes or comment)"
        )),
    }
}

/// Forgejo rejects a comment review that says nothing, and a change request
/// without a body. Refuse both here, before the request.
fn check_has_content(event: ReviewEventArg, has_body: bool, comments: usize) -> Result<()> {
    match event {
        ReviewEventArg::Comment if !has_body && comments == 0 => Err(anyhow!(
            "a comment review needs something to say: pass --body, or one or more --line-comment"
        )),
        ReviewEventArg::RequestChanges if !has_body => Err(anyhow!(
            "a request-changes review needs a body explaining what to change (--body)"
        )),
        _ => Ok(()),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn request_reviewers(args: ReviewerArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let reviewers = expand_reviewers(args.reviewers)?;
    api::pull::request_reviewers(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        reviewers.clone(),
    )
    .await?;
    println!(
        "✓ Requested review from {} on #{}",
        reviewers.join(", "),
        args.number
    );
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn unrequest_reviewers(args: ReviewerArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let reviewers = expand_reviewers(args.reviewers)?;
    api::pull::unrequest_reviewers(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        reviewers.clone(),
    )
    .await?;
    println!(
        "✓ Removed review request from {} on #{}",
        reviewers.join(", "),
        args.number
    );
    Ok(())
}

/// Accept reviewers as either repeated args or comma-separated within one
/// arg, since both forms come up naturally in shell invocations.
fn expand_reviewers(raw: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        for piece in r.split(',') {
            let p = piece.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            "no reviewers provided (pass one or more usernames)"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_reviewers_splits_commas_and_trims() {
        let out = expand_reviewers(vec!["alice, bob".into(), "carol".into()]).unwrap();
        assert_eq!(out, vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn no_event_means_a_plain_comment_only_with_line_comments() {
        assert!(matches!(
            resolve_event(None, 1).unwrap(),
            ReviewEventArg::Comment
        ));
        assert!(matches!(
            resolve_event(Some(ReviewEventArg::Approve), 2).unwrap(),
            ReviewEventArg::Approve
        ));
        assert!(resolve_event(None, 0).is_err());
    }

    #[test]
    fn empty_comment_and_bodiless_change_request_are_refused_locally() {
        use ReviewEventArg::{Approve, Comment, RequestChanges};
        assert!(check_has_content(Comment, false, 0).is_err());
        assert!(check_has_content(Comment, true, 0).is_ok());
        // Line comments alone are enough for a comment review.
        assert!(check_has_content(Comment, false, 1).is_ok());
        // A change request needs a body even with line comments.
        assert!(check_has_content(RequestChanges, false, 3).is_err());
        assert!(check_has_content(RequestChanges, true, 0).is_ok());
        assert!(check_has_content(Approve, false, 0).is_ok());
    }

    #[test]
    fn line_comments_merge_flags_then_file() {
        let flags: Vec<String> = ["a.rs", "old:2", "from flag"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = collect_line_comments(
            &flags,
            Some(r#"[{"path":"b.rs","line":5,"body":"from file"}]"#),
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].body, "from flag");
        assert_eq!(got[1].path, "b.rs");
        assert!(collect_line_comments(&[], None).unwrap().is_empty());
        assert!(collect_line_comments(&[], Some("nope")).is_err());
    }

    fn parse_review(argv: &[&str]) -> Result<ReviewArgs, clap::Error> {
        use crate::cli::pr::{PrCmd, PrSub};
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let mut full = vec!["fj", "pr", "review"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)?.command {
            Command::Pr(PrCmd {
                command: PrSub::Review(a),
            }) => Ok(a),
            other => panic!("expected pr review, got {other:?}"),
        }
    }

    #[test]
    fn review_flags_parse_repeated_line_comments() {
        // The pre-existing form is untouched.
        let a = parse_review(&["42", "--event", "approve", "--body", "LGTM"]).unwrap();
        assert!(matches!(a.event, Some(ReviewEventArg::Approve)));
        assert!(a.line_comment.is_empty());
        // --event stays required when there are no line comments.
        assert!(parse_review(&["42"]).is_err());
        assert!(parse_review(&["42", "--body", "x"]).is_err());

        let a = parse_review(&[
            "42",
            "--line-comment",
            "docs/a:b.md",
            "old:3",
            "-- why: was this removed?",
            "--event",
            "request-changes",
            "--line-comment",
            "src/x.rs",
            "10",
            "--event is not a flag here",
            "-b",
            "see inline",
        ])
        .unwrap();
        assert!(matches!(a.event, Some(ReviewEventArg::RequestChanges)));
        assert_eq!(a.body.as_deref(), Some("see inline"));
        let got = collect_line_comments(&a.line_comment, None).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "docs/a:b.md");
        assert_eq!(got[0].body, "-- why: was this removed?");
        assert_eq!(got[1].body, "--event is not a flag here");

        // A single comment on its own, with no verdict.
        let a = parse_review(&["42", "--line-comment", "a.rs", "7", "nit"]).unwrap();
        assert!(a.event.is_none());
        // Too few values is a usage error, not a guess.
        assert!(parse_review(&["42", "--line-comment", "a.rs", "7"]).is_err());
        let a = parse_review(&["42", "--line-comments-file", "-"]).unwrap();
        assert_eq!(a.line_comments_file.as_deref(), Some("-"));
    }

    #[test]
    fn expand_reviewers_errors_when_empty() {
        assert!(expand_reviewers(vec![]).is_err());
        assert!(expand_reviewers(vec!["".into(), " , ".into()]).is_err());
    }
}
