//! Editor + interactive prompt helpers used by `issue create`, `pr create`,
//! and `*-comment` commands.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use tempfile::Builder;

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
    // Create the scratch file with O_EXCL + mode 0600 (via tempfile) and a
    // random name, instead of a predictable `/tmp/fj-<pid>-<name>` path that a
    // local attacker could pre-create as a symlink. The handle is held until
    // we've read the buffer back, then dropped (which unlinks it).
    let mut tmp = Builder::new()
        .prefix("fj-")
        .suffix(&format!("-{template_name}"))
        .tempfile()
        .context("creating secure temp file")?;
    tmp.write_all(initial_contents.as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.path().to_path_buf();

    let editor = editor_command();
    // Split on whitespace so users can set `EDITOR="code -w"`.
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("editor command is empty"))?;
    let extra_args: Vec<&str> = parts.collect();
    let _foreground = crate::interrupt::ForegroundChild::enter();
    let status = Command::new(program)
        .args(&extra_args)
        .arg(&tmp_path)
        .status()
        .with_context(|| format!("launching editor `{editor}`"))?;
    if !status.success() {
        return Err(anyhow!("editor `{editor}` exited with status {status}"));
    }

    // Re-open by path: some editors replace the file (write new + rename)
    // rather than editing in place, so the original handle may be stale.
    let mut buf = String::new();
    fs::File::open(&tmp_path)?
        .read_to_string(&mut buf)
        .context("reading edited buffer back from temp file")?;
    drop(tmp);
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

/// Resolve mutually-exclusive `--body` and `--body-file` flags. `-` reads
/// from stdin for either flag. When `editor_template` is present and neither
/// flag was passed, open the editor using the supplied template name and seed.
pub fn resolve_body_input(
    body: Option<&str>,
    body_file: Option<&str>,
    editor_template: Option<(&str, &str)>,
) -> Result<Option<String>> {
    match (body, body_file) {
        (Some(_), Some(_)) => Err(anyhow!("--body and --body-file are mutually exclusive")),
        (Some(s), None) => read_body(Some(s)),
        (None, Some("-")) => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(Some(buf.trim_end().to_string()))
        }
        (None, Some(path)) => {
            let buf = fs::read_to_string(Path::new(path))
                .with_context(|| format!("reading body from {path}"))?;
            Ok(Some(buf.trim_end().to_string()))
        }
        (None, None) => match editor_template {
            Some((template_name, template_initial)) => {
                resolve_body(None, template_name, template_initial)
            }
            None => Ok(None),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    // Mutates process-global env so it's serial-only and ignored by default.
    // Run with `cargo test -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn editor_command_uses_visual_first() {
        let saved_visual = env::var("VISUAL").ok();
        let saved_editor = env::var("EDITOR").ok();
        // SAFETY: tests modifying env are intrinsically racy. The fj test suite
        // doesn't run them in parallel against editor_command, and we restore
        // on exit. Acceptable for a hermetic CI environment.
        unsafe {
            env::set_var("VISUAL", "code -w");
            env::set_var("EDITOR", "nano");
        }
        assert_eq!(editor_command(), "code -w");
        unsafe {
            env::remove_var("VISUAL");
        }
        assert_eq!(editor_command(), "nano");
        unsafe {
            env::remove_var("EDITOR");
        }
        assert_eq!(editor_command(), "vi");
        unsafe {
            if let Some(v) = saved_visual {
                env::set_var("VISUAL", v);
            }
            if let Some(v) = saved_editor {
                env::set_var("EDITOR", v);
            }
        }
    }

    // Marked #[ignore] because spawning a real subprocess from the test
    // harness occasionally hangs on macOS depending on how cargo wired up
    // stdin/stdout. Run with `cargo test -- --ignored` to exercise it.
    #[test]
    #[ignore]
    #[cfg(unix)]
    fn edit_text_with_true_returns_initial() {
        // `/usr/bin/true` is a no-op command that always exits 0. Using it as
        // the "editor" means we get the initial contents back unchanged.
        // We use the absolute path to avoid hangs when test runners have an
        // odd PATH that resolves `true` to something else.
        let saved = env::var("EDITOR").ok();
        unsafe { env::set_var("EDITOR", "/usr/bin/true") };
        let out = edit_text("FJ_TEST.md", "hello world\n").unwrap();
        assert_eq!(out, "hello world");
        unsafe {
            match saved {
                Some(v) => env::set_var("EDITOR", v),
                None => env::remove_var("EDITOR"),
            }
        }
    }

    #[test]
    fn read_body_passes_string_through() {
        let r = read_body(Some("hello")).unwrap();
        assert_eq!(r.as_deref(), Some("hello"));
        assert!(read_body(None).unwrap().is_none());
    }

    #[test]
    fn resolve_body_input_reads_file_and_trims_trailing_newline() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"hello from file\n\n").unwrap();

        let r = resolve_body_input(None, Some(file.path().to_str().unwrap()), None).unwrap();
        assert_eq!(r.as_deref(), Some("hello from file"));
    }

    #[test]
    fn resolve_body_input_rejects_body_and_body_file() {
        let err = resolve_body_input(Some("literal"), Some("body.md"), None).unwrap_err();
        assert!(err
            .to_string()
            .contains("--body and --body-file are mutually exclusive"));
    }
}
