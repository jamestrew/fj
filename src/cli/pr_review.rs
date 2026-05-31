//! `fj pr review` / `request-review` / `unrequest-review`.

use std::io::Read;

use anyhow::{anyhow, Result};

use crate::api;
use crate::cli::context::resolve_repo;
use crate::cli::editor;
use crate::cli::pr::{ReviewArgs, ReviewerArgs};
use crate::cli::pr_args::ReviewEventArg;

pub(crate) async fn review(args: ReviewArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let body_required = matches!(args.event, ReviewEventArg::RequestChanges);
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
    let r = api::pull::submit_review(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        args.number,
        args.event.into(),
        body.as_deref(),
    )
    .await?;
    println!("✓ Submitted {} review on #{}", r.state, args.number);
    println!("{}", r.html_url);
    Ok(())
}

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
    fn expand_reviewers_errors_when_empty() {
        assert!(expand_reviewers(vec![]).is_err());
        assert!(expand_reviewers(vec!["".into(), " , ".into()]).is_err());
    }
}
