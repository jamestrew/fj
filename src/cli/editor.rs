//! Editor + interactive prompt helpers used by `issue create`, `pr create`,
//! and `*-comment` commands.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::process::Command;

use anyhow::{anyhow, Context, Result};

/// Default editor binary, in order: $VISUAL, $EDITOR, then `vi`.
fn editor_command() -> String {
    env::var("VISUAL")
        .ok()
        .or_else(|| env::var("EDITOR").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// Open `$EDITOR` with `initial_contents` and return the saved buffer with
/// trailing whitespace trimmed. `template_name` is just the suffix of the
/// temp file (helps editors pick syntax: `ISSUE_BODY.md`, etc.).
pub fn edit_text(template_name: &str, initial_contents: &str) -> Result<String> {
    let mut tmp = env::temp_dir();
    tmp.push(format!("fj-{}-{}", std::process::id(), template_name));
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("creating temp file at {}", tmp.display()))?;
        file.write_all(initial_contents.as_bytes())?;
    }

    let editor = editor_command();
    // Split on whitespace so users can set `EDITOR="code -w"`.
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("editor command is empty"))?;
    let extra_args: Vec<&str> = parts.collect();
    let status = Command::new(program)
        .args(&extra_args)
        .arg(&tmp)
        .status()
        .with_context(|| format!("launching editor `{editor}`"))?;
    if !status.success() {
        return Err(anyhow!("editor `{editor}` exited with status {status}"));
    }

    let mut buf = String::new();
    fs::File::open(&tmp)?
        .read_to_string(&mut buf)
        .context("reading edited buffer back from temp file")?;
    let _ = fs::remove_file(&tmp);
    Ok(buf.trim_end().to_string())
}

/// For body-style arguments. Resolves the value of a `--body` flag with three
/// modes: literal string, `-` for stdin, or `None` to open `$EDITOR` seeded
/// with `template_initial`. Returns `Ok(None)` only if the user closes the
/// editor with an empty buffer AND the field is optional.
pub fn resolve_body(
    flag: Option<&str>,
    template_name: &str,
    template_initial: &str,
) -> Result<Option<String>> {
    match flag {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let trimmed = buf.trim_end().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Some(s) => Ok(Some(s.to_string())),
        None => {
            // If stdin isn't a TTY we can't reasonably open an editor; default
            // to "no body" rather than hanging.
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                return Ok(None);
            }
            let text = edit_text(template_name, template_initial)?;
            if text.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
    }
}

/// Resolve a `--body` flag without launching the editor on omission. Used by
/// commands where the body is optional and we don't want a surprise editor.
pub fn read_body(flag: Option<&str>) -> Result<Option<String>> {
    match flag {
        None => Ok(None),
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(Some(buf.trim_end().to_string()))
        }
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// Prompt for a single line on stderr, returning a trimmed string.
pub fn prompt_line(label: &str) -> Result<String> {
    use std::io::{stderr, stdin};
    eprint!("{label}: ");
    stderr().flush().ok();
    let mut buf = String::new();
    stdin().read_line(&mut buf).context("reading line")?;
    Ok(buf.trim().to_string())
}
