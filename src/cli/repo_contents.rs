//! `fj repo contents [path]`: read a file or list a directory at a ref
//! (default branch, `--branch`, `--tag`, or `--ref`).
//!
//! Strictly read-only. fj has no server-side file edit at all, so reading at
//! a tag offers nothing that would only make sense on a branch.

use std::io::{IsTerminal, Write};

use anyhow::{anyhow, bail, Result};
use clap::Args;

use crate::api;
use crate::api::contents::{ContentEntry, Contents};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::cli::repo_commit::{resolve_ref, RefArgs};
use crate::output;

#[derive(Debug, Args)]
pub struct ContentsArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// File or directory path within the repo. Omit for the repo root.
    pub path: Option<String>,
    #[command(flatten)]
    pub at: RefArgs,
    /// Emit the API response as JSON instead of file text or a table
    #[arg(long)]
    pub json: bool,
}

/// Directory rows: folders first, then names ordered case-insensitively.
fn sorted_entries(mut entries: Vec<ContentEntry>) -> Vec<ContentEntry> {
    entries.sort_by(|a, b| {
        (a.kind != "dir", a.name.to_lowercase(), &a.name).cmp(&(
            b.kind != "dir",
            b.name.to_lowercase(),
            &b.name,
        ))
    });
    entries
}

fn entry_row(e: &ContentEntry) -> Vec<String> {
    let (name, detail) = match e.kind.as_str() {
        "dir" => (format!("{}/", e.name), String::new()),
        // A submodule is a pinned commit of another repo, not a readable file.
        "submodule" => (
            e.name.clone(),
            format!("@ {}", e.sha.get(..7).unwrap_or(&e.sha)),
        ),
        "symlink" => (
            e.name.clone(),
            e.target
                .as_deref()
                .map(|t| format!("-> {t}"))
                .unwrap_or_default(),
        ),
        _ => (e.name.clone(), output::human_size(e.size)),
    };
    vec![e.kind.clone(), name, detail]
}

/// What to do with a non-directory entry.
#[derive(Debug, PartialEq, Eq)]
enum FileOutput {
    Text(String),
    /// Not valid UTF-8: only written when stdout is not a terminal.
    Binary(Vec<u8>),
    /// A one-line description instead of content (symlink, submodule).
    Note(String),
}

fn file_output(e: &ContentEntry) -> Result<FileOutput> {
    match e.kind.as_str() {
        "submodule" => Ok(FileOutput::Note(format!(
            "{} is a submodule pinned at {}{}",
            e.path,
            e.sha.get(..7).unwrap_or(&e.sha),
            e.submodule_git_url
                .as_deref()
                .filter(|u| !u.is_empty())
                .map(|u| format!(" ({u})"))
                .unwrap_or_default()
        ))),
        "symlink" => Ok(FileOutput::Note(format!(
            "{} is a symlink to {}",
            e.path,
            e.target.as_deref().unwrap_or("(unknown target)")
        ))),
        _ => {
            let bytes = api::contents::file_bytes(e)?.ok_or_else(|| {
                anyhow!(
                    "the server returned no content for {} ({}); it is over the API size cap{}",
                    e.path,
                    output::human_size(e.size),
                    e.download_url
                        .as_deref()
                        .filter(|u| !u.is_empty())
                        .map(|u| format!(". Download it from {u}"))
                        .unwrap_or_default()
                )
            })?;
            Ok(match String::from_utf8(bytes) {
                Ok(text) => FileOutput::Text(text),
                Err(e) => FileOutput::Binary(e.into_bytes()),
            })
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) async fn contents(args: ContentsArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let at = resolve_ref(&ctx, &args.at).await?;
    let path = args.path.as_deref().unwrap_or("");
    let found = api::contents::get(
        &ctx.client,
        &ctx.owner,
        &ctx.name,
        path,
        at.query.as_deref(),
    )
    .await?;
    match found {
        Contents::Dir(entries) => {
            let entries = sorted_entries(entries);
            if args.json {
                return output::print_json(&serde_json::to_value(&entries)?);
            }
            if let Some(label) = &at.label {
                println!("{}", output::dim(label));
            }
            if entries.is_empty() {
                println!("(empty directory)");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = entries.iter().map(entry_row).collect();
            print!("{}", output::render_table(&["TYPE", "NAME", "SIZE"], &rows));
        }
        Contents::Entry(entry) => {
            if args.json {
                return output::print_json(&serde_json::to_value(&*entry)?);
            }
            // The ref label goes to stderr so stdout stays exactly the file.
            if let Some(label) = &at.label {
                eprintln!("{}", output::dim(label));
            }
            match file_output(&entry)? {
                FileOutput::Text(text) => print!("{text}"),
                FileOutput::Note(note) => println!("{note}"),
                FileOutput::Binary(bytes) => {
                    let mut out = std::io::stdout();
                    if out.is_terminal() {
                        bail!(
                            "{} is binary ({}); redirect stdout to a file to save it",
                            entry.path,
                            output::human_size(entry.size)
                        );
                    }
                    out.write_all(&bytes)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, name: &str) -> ContentEntry {
        ContentEntry {
            kind: kind.into(),
            name: name.into(),
            path: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn directories_sort_first_then_case_insensitive_names() {
        let sorted = sorted_entries(vec![
            entry("file", "b.txt"),
            entry("dir", "src"),
            entry("file", "Cargo.toml"),
            entry("dir", "Docs"),
            entry("file", "a.txt"),
        ]);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Docs", "src", "a.txt", "b.txt", "Cargo.toml"]);
    }

    #[test]
    fn rows_mark_directories_submodules_and_symlinks() {
        assert_eq!(entry_row(&entry("dir", "src")), ["dir", "src/", ""]);
        let mut sub = entry("submodule", "vendor");
        sub.sha = "abcdef0123456".into();
        assert_eq!(entry_row(&sub), ["submodule", "vendor", "@ abcdef0"]);
        let mut link = entry("symlink", "latest");
        link.target = Some("v2".into());
        assert_eq!(entry_row(&link), ["symlink", "latest", "-> v2"]);
        let mut file = entry("file", "a.txt");
        file.size = 2048;
        assert_eq!(entry_row(&file), ["file", "a.txt", "2.0 KB"]);
    }

    #[test]
    fn file_output_decodes_text_and_keeps_binary_as_bytes() {
        let mut text = entry("file", "a.txt");
        text.content = Some("aGVsbG8K".into());
        text.encoding = Some("base64".into());
        assert_eq!(
            file_output(&text).unwrap(),
            FileOutput::Text("hello\n".into())
        );

        let mut bin = entry("file", "a.bin");
        bin.content = Some("//79".into());
        bin.encoding = Some("base64".into());
        assert_eq!(
            file_output(&bin).unwrap(),
            FileOutput::Binary(vec![0xff, 0xfe, 0xfd])
        );
    }

    #[test]
    fn file_output_explains_missing_content_and_non_files() {
        let mut big = entry("file", "big.iso");
        big.size = 20 * 1024 * 1024;
        big.download_url = Some("https://forge.example/raw/big.iso".into());
        let err = file_output(&big).unwrap_err().to_string();
        assert!(err.contains("over the API size cap"), "{err}");
        assert!(err.contains("https://forge.example/raw/big.iso"), "{err}");

        let mut sub = entry("submodule", "vendor");
        sub.sha = "abcdef0123456".into();
        assert_eq!(
            file_output(&sub).unwrap(),
            FileOutput::Note("vendor is a submodule pinned at abcdef0".into())
        );
        let mut link = entry("symlink", "latest");
        link.target = Some("v2".into());
        assert_eq!(
            file_output(&link).unwrap(),
            FileOutput::Note("latest is a symlink to v2".into())
        );
    }

    #[test]
    fn contents_accepts_a_tag_as_the_ref() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "fj",
            "repo",
            "contents",
            "src/main.rs",
            "--tag",
            "v1.0.0",
            "-R",
            "o/r",
        ])
        .expect("parses");
        let crate::cli::Command::Repo(repo) = cli.command else {
            panic!("expected repo");
        };
        let crate::cli::repo::RepoSub::Contents(args) = repo.command else {
            panic!("expected contents");
        };
        assert_eq!(args.path.as_deref(), Some("src/main.rs"));
        assert_eq!(args.at.tag.as_deref(), Some("v1.0.0"));
    }
}
