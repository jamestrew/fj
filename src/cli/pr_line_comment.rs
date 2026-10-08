//! Inline (diff line) comments for `fj pr review`: reading them from flags or
//! a JSON file, and checking each anchor against the pull request's diff
//! before anything is sent.
//!
//! An anchor is a file path, a side and one line number. The side says which
//! version of the file the number counts in: `new` is the pull request's
//! version (added and unchanged lines), `old` is the base version (removed and
//! unchanged lines). The two numberings drift apart after the first hunk that
//! adds or removes lines, so the side is never guessed from the number.
//! Anchors are single lines only; ranges are not offered.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::api::pull::{DiffSide, LineComment};

/// Parse a LINE value: `N` or `new:N` for the pull request's version of the
/// file, `old:N` for the base version.
pub fn parse_line_spec(spec: &str) -> Result<(DiffSide, u64)> {
    let (side, digits) = match spec.split_once(':') {
        Some(("new", n)) => (DiffSide::New, n),
        Some(("old", n)) => (DiffSide::Old, n),
        Some(_) => bail!("invalid line \"{spec}\": use N, new:N or old:N"),
        None => (DiffSide::New, spec),
    };
    if digits.contains(['-', ',', '.']) {
        bail!("invalid line \"{spec}\": a line comment anchors to one line, not a range");
    }
    let line: u64 = digits
        .parse()
        .map_err(|_| anyhow!("invalid line \"{spec}\": use N, new:N or old:N"))?;
    if line == 0 {
        bail!("invalid line \"{spec}\": line numbers start at 1");
    }
    Ok((side, line))
}

fn checked(path: &str, side: DiffSide, line: u64, body: &str, origin: &str) -> Result<LineComment> {
    if path.trim().is_empty() {
        bail!("{origin}: the file path is empty");
    }
    if path.starts_with('/') {
        bail!("{origin}: \"{path}\" must be a path relative to the repository root");
    }
    if line == 0 {
        bail!("{origin}: line numbers start at 1");
    }
    if body.trim().is_empty() {
        bail!("{origin}: the comment body is empty");
    }
    Ok(LineComment {
        path: path.to_string(),
        side,
        line,
        body: body.to_string(),
    })
}

/// Comments from repeated `--line-comment PATH LINE BODY`. clap hands the
/// values over flattened, three per occurrence, so no delimiter is ever
/// parsed out of a path or a body.
pub fn from_flags(values: &[String]) -> Result<Vec<LineComment>> {
    if !values.chunks_exact(3).remainder().is_empty() {
        bail!("--line-comment takes exactly three values: PATH LINE BODY");
    }
    values
        .chunks(3)
        .enumerate()
        .map(|(i, v)| {
            let origin = format!("--line-comment #{}", i + 1);
            let (side, line) = parse_line_spec(&v[1]).with_context(|| origin.clone())?;
            checked(&v[0], side, line, &v[2], &origin)
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    line: u64,
    #[serde(default)]
    side: Option<String>,
    body: String,
}

/// Comments from `--line-comments-file`: a JSON array of
/// `{"path", "line", "side", "body"}` with `side` either `"new"` (the default)
/// or `"old"`. Any other key is refused, which is what turns a range field
/// such as `end_line` into an error instead of a silently dropped range.
pub fn from_json(text: &str) -> Result<Vec<LineComment>> {
    let entries: Vec<FileEntry> = serde_json::from_str(text).map_err(|e| {
        anyhow!(
            "invalid line comments file: {e}\n  expected a JSON array of {{\"path\": \"...\", \"line\": N, \"side\": \"new\"|\"old\", \"body\": \"...\"}}; single-line anchors only"
        )
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let origin = format!("line comments file entry #{}", i + 1);
            let side = match e.side.as_deref() {
                None | Some("new") => DiffSide::New,
                Some("old") => DiffSide::Old,
                Some(other) => {
                    bail!("{origin}: side must be \"new\" or \"old\", not \"{other}\"")
                }
            };
            checked(&e.path, side, e.line, &e.body, &origin)
        })
        .collect()
}

/// One file of a unified diff: its paths and the line numbers each side shows.
#[derive(Debug, Default, PartialEq, Eq)]
struct DiffFile {
    old_path: Option<String>,
    new_path: Option<String>,
    old: BTreeSet<u64>,
    new: BTreeSet<u64>,
}

/// The commentable lines of a pull request, read from its unified diff.
#[derive(Debug, Default)]
pub struct DiffIndex(Vec<DiffFile>);

/// Undo git's C-style quoting of a path with unusual bytes (`"a\tb\303\251"`).
fn unquote(s: &str) -> String {
    let inner = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s);
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let c = bytes[i + 1];
        if (b'0'..=b'7').contains(&c) {
            let mut value: u32 = 0;
            let mut used = 0;
            while used < 3 && i + 1 + used < bytes.len() {
                let d = bytes[i + 1 + used];
                if !(b'0'..=b'7').contains(&d) {
                    break;
                }
                value = value * 8 + u32::from(d - b'0');
                used += 1;
            }
            out.push(value as u8);
            i += 1 + used;
            continue;
        }
        out.push(match c {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'v' => 0x0b,
            other => other,
        });
        i += 2;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path in a `---` / `+++` / `rename` header line. `None` for `/dev/null`.
fn header_path(raw: &str, strip_prefix: bool) -> Option<String> {
    let path = if raw.starts_with('"') {
        unquote(raw.trim_end())
    } else {
        // git appends a tab after a path that contains spaces.
        raw.split('\t').next().unwrap_or(raw).to_string()
    };
    if path == "/dev/null" {
        return None;
    }
    if strip_prefix {
        if let Some(rest) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
            return Some(rest.to_string());
        }
    }
    Some(path)
}

/// `@@ -a,b +c,d @@` as `(a, b, c, d)`; an omitted count is 1.
fn hunk_header(line: &str) -> Option<(u64, u64, u64, u64)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let pair = |s: &str| -> Option<(u64, u64)> {
        match s.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = pair(old)?;
    let (c, d) = pair(new)?;
    Some((a, b, c, d))
}

impl DiffIndex {
    /// Walk a unified diff. Hunk bodies are consumed by the counts in their
    /// `@@` header, so a removed line that itself begins with `-- ` is never
    /// mistaken for a file header.
    pub fn parse(diff: &str) -> DiffIndex {
        let mut files: Vec<DiffFile> = Vec::new();
        let (mut old_line, mut new_line, mut old_left, mut new_left) = (0u64, 0u64, 0u64, 0u64);
        for line in diff.lines() {
            if old_left > 0 || new_left > 0 {
                let Some(file) = files.last_mut() else {
                    continue;
                };
                match line.as_bytes().first() {
                    Some(b'\\') => {}
                    Some(b'+') => {
                        file.new.insert(new_line);
                        new_line += 1;
                        new_left = new_left.saturating_sub(1);
                    }
                    Some(b'-') => {
                        file.old.insert(old_line);
                        old_line += 1;
                        old_left = old_left.saturating_sub(1);
                    }
                    // A context line (a bare empty line is one whose single
                    // leading space was trimmed in transit).
                    _ => {
                        file.old.insert(old_line);
                        file.new.insert(new_line);
                        old_line += 1;
                        new_line += 1;
                        old_left = old_left.saturating_sub(1);
                        new_left = new_left.saturating_sub(1);
                    }
                }
                continue;
            }
            if line.starts_with("diff --git ") {
                files.push(DiffFile::default());
                continue;
            }
            if let Some((a, b, c, d)) = hunk_header(line) {
                if files.is_empty() {
                    files.push(DiffFile::default());
                }
                (old_line, old_left, new_line, new_left) = (a, b, c, d);
                continue;
            }
            let Some(file) = files.last_mut() else {
                continue;
            };
            if let Some(p) = line.strip_prefix("--- ") {
                file.old_path = header_path(p, true);
            } else if let Some(p) = line.strip_prefix("+++ ") {
                file.new_path = header_path(p, true);
            } else if let Some(p) = line.strip_prefix("rename from ") {
                // A pure rename has no `---` / `+++` lines at all.
                if file.old_path.is_none() {
                    file.old_path = header_path(p, false);
                }
            } else if let Some(p) = line.strip_prefix("rename to ") {
                if file.new_path.is_none() {
                    file.new_path = header_path(p, false);
                }
            }
        }
        DiffIndex(files)
    }

    /// Refuse an anchor the diff does not contain: a file the pull request
    /// does not change, or a line outside every hunk on the chosen side.
    pub fn check(&self, c: &LineComment, number: u64) -> Result<()> {
        let path = c.path.as_str();
        // Forgejo names a changed file by its new path; a deleted file keeps
        // its old one.
        let file = self.0.iter().find(|f| match &f.new_path {
            Some(p) => p == path,
            None => f.old_path.as_deref() == Some(path),
        });
        let Some(file) = file else {
            if let Some(renamed) = self
                .0
                .iter()
                .find(|f| f.old_path.as_deref() == Some(path))
                .and_then(|f| f.new_path.as_deref())
            {
                bail!(
                    "{path} was renamed to {renamed} in PR #{number}; anchor the comment to {renamed} (use old:N for a line of the base version)."
                );
            }
            bail!("{path} is not a changed file in PR #{number} (see: fj pr files {number}).");
        };
        let (lines, other, other_side) = match c.side {
            DiffSide::New => (&file.new, &file.old, DiffSide::Old),
            DiffSide::Old => (&file.old, &file.new, DiffSide::New),
        };
        if lines.contains(&c.line) {
            return Ok(());
        }
        let side = c.side.as_str();
        let mut msg = if lines.is_empty() {
            format!(
                "{path} has no {side}-side lines in PR #{number}'s diff, so {side}:{} cannot be commented on.",
                c.line
            )
        } else {
            format!(
                "{path} has no line {} on the {side} side of PR #{number}'s diff. Commentable {side}-side lines: {}.",
                c.line,
                ranges(lines)
            )
        };
        if other.contains(&c.line) {
            msg.push_str(&format!(
                " Line {} is in the diff on the {} side: use {}:{} if that is the line you mean.",
                c.line,
                other_side.as_str(),
                other_side.as_str(),
                c.line
            ));
        }
        bail!(msg)
    }
}

/// `3-9, 20, 31-34`, capped so a huge diff does not flood the terminal.
fn ranges(lines: &BTreeSet<u64>) -> String {
    let mut spans: Vec<(u64, u64)> = Vec::new();
    for &n in lines {
        match spans.last_mut() {
            Some((_, end)) if *end + 1 == n => *end = n,
            _ => spans.push((n, n)),
        }
    }
    let total = spans.len();
    let mut out: Vec<String> = spans
        .iter()
        .take(8)
        .map(|(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect();
    if total > 8 {
        out.push(format!("and {} more ranges", total - 8));
    }
    out.join(", ")
}

/// What `fj pr review` prints after the server accepted the review, plus a
/// warning when the server recorded a different number of line comments than
/// were sent.
pub fn submitted_lines(
    state: &str,
    number: u64,
    requested: usize,
    recorded: u64,
) -> (String, Option<String>) {
    if requested == 0 {
        return (format!("✓ Submitted {state} review on #{number}"), None);
    }
    let noun = if recorded == 1 {
        "line comment"
    } else {
        "line comments"
    };
    let headline = format!("✓ Submitted {state} review on #{number} with {recorded} {noun}");
    let warning = (recorded != requested as u64).then(|| {
        format!(
            "warning: {requested} line comment(s) were sent but the server reports {recorded} on the review; check it in the browser."
        )
    });
    (headline, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lc(path: &str, side: DiffSide, line: u64) -> LineComment {
        LineComment {
            path: path.into(),
            side,
            line,
            body: "b".into(),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn line_spec_defaults_to_new_and_names_old() {
        assert_eq!(parse_line_spec("12").unwrap(), (DiffSide::New, 12));
        assert_eq!(parse_line_spec("new:12").unwrap(), (DiffSide::New, 12));
        assert_eq!(parse_line_spec("old:7").unwrap(), (DiffSide::Old, 7));
    }

    #[test]
    fn line_spec_rejects_ranges_zero_and_junk() {
        for bad in [
            "0",
            "old:0",
            "",
            "abc",
            "left:3",
            "new:",
            "12-14",
            "old:3,5",
            "-4",
            "1.5",
            "new:old:1",
        ] {
            assert!(parse_line_spec(bad).is_err(), "{bad} should be rejected");
        }
        let err = parse_line_spec("12-14").unwrap_err().to_string();
        assert!(err.contains("not a range"), "{err}");
    }

    #[test]
    fn flags_keep_colons_in_paths_and_anything_in_bodies() {
        let got = from_flags(&s(&[
            "docs/a:b.md",
            "old:3",
            "old:9 is wrong: see \"x\"\n--- not a header",
            "src/main.rs",
            "40",
            "-starts with a dash",
        ]))
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "docs/a:b.md");
        assert_eq!((got[0].side, got[0].line), (DiffSide::Old, 3));
        assert_eq!(got[0].body, "old:9 is wrong: see \"x\"\n--- not a header");
        assert_eq!((got[1].side, got[1].line), (DiffSide::New, 40));
        assert_eq!(got[1].body, "-starts with a dash");
    }

    #[test]
    fn flags_are_validated_with_the_offending_occurrence() {
        let err = |v: &[&str]| format!("{:#}", from_flags(&s(v)).unwrap_err());
        assert!(err(&["a", "1"]).contains("exactly three values"));
        assert!(err(&["", "1", "b"]).contains("file path is empty"));
        assert!(err(&["/etc/x", "1", "b"]).contains("relative to the repository root"));
        assert!(err(&["a", "1", "  \n"]).contains("comment body is empty"));
        let e = err(&["a", "1", "ok", "b", "zero", "ok"]);
        assert!(e.contains("--line-comment #2"), "{e}");
        assert!(from_flags(&[]).unwrap().is_empty());
    }

    #[test]
    fn json_file_reads_sides_and_defaults_to_new() {
        let got = from_json(
            r#"[
              {"path": "a:b.rs", "line": 4, "body": "one"},
              {"path": "c.rs", "line": 9, "side": "old", "body": "two"},
              {"path": "c.rs", "line": 9, "side": "new", "body": "three"}
            ]"#,
        )
        .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].side, DiffSide::New);
        assert_eq!(got[1].side, DiffSide::Old);
        assert_eq!(got[2].side, DiffSide::New);
        assert_eq!(got[0].path, "a:b.rs");
        assert!(from_json("[]").unwrap().is_empty());
    }

    #[test]
    fn json_file_rejects_ranges_unknown_sides_and_bad_shapes() {
        let err = |t: &str| format!("{:#}", from_json(t).unwrap_err());
        // A range field is refused, not dropped.
        let e = err(r#"[{"path":"a","line":1,"end_line":3,"body":"x"}]"#);
        assert!(e.contains("end_line"), "{e}");
        assert!(e.contains("single-line anchors only"), "{e}");
        let e = err(r#"[{"path":"a","line":1,"extra_lines_count":2,"body":"x"}]"#);
        assert!(e.contains("extra_lines_count"), "{e}");
        assert!(err(r#"[{"path":"a","line":1,"side":"left","body":"x"}]"#).contains("\"left\""));
        assert!(err(r#"[{"path":"a","line":0,"body":"x"}]"#).contains("start at 1"));
        assert!(err(r#"[{"path":"a","line":-1,"body":"x"}]"#).contains("invalid line comments"));
        assert!(err(r#"[{"path":"a","line":1}]"#).contains("body"));
        assert!(err(r#"{"path":"a","line":1,"body":"x"}"#).contains("JSON array"));
        assert!(err(r#"[{"path":"a","line":1,"body":" "}]"#).contains("entry #1"));
    }

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,4 +10,5 @@ fn x() {
 ctx
-removed
+added one
+added two
 ctx
 ctx
@@ -40,3 +41,2 @@ fn y() {
 ctx
--- looks like a header
 ctx
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
--- a/old name.txt\t
+++ b/new name.txt\t
@@ -1 +1 @@
-x
+y
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
diff --git a/fresh.rs b/fresh.rs
new file mode 100644
--- /dev/null
+++ b/fresh.rs
@@ -0,0 +1,2 @@
+one
+two
diff --git a/moved.rs b/moved2.rs
similarity index 100%
rename from moved.rs
rename to moved2.rs
diff --git \"a/sp\\303\\251cial.rs\" \"b/sp\\303\\251cial.rs\"
--- \"a/sp\\303\\251cial.rs\"
+++ \"b/sp\\303\\251cial.rs\"
@@ -3 +3 @@
-a
+b
";

    #[test]
    fn old_and_new_sides_number_independently() {
        let idx = DiffIndex::parse(DIFF);
        let a = &idx.0[0];
        // First hunk: one line removed, two added. Old 10..=13, new 10..=14.
        assert_eq!(
            a.old.iter().copied().take(4).collect::<Vec<_>>(),
            [10, 11, 12, 13]
        );
        assert_eq!(
            a.new.iter().copied().take(5).collect::<Vec<_>>(),
            [10, 11, 12, 13, 14]
        );
        // Second hunk starts at old 40 but new 41: same text, different number.
        assert!(idx.check(&lc("src/a.rs", DiffSide::Old, 40), 5).is_ok());
        assert!(idx.check(&lc("src/a.rs", DiffSide::New, 40), 5).is_err());
        assert!(idx.check(&lc("src/a.rs", DiffSide::New, 42), 5).is_ok());
        assert!(idx.check(&lc("src/a.rs", DiffSide::Old, 43), 5).is_err());
        // The removed line that looks like a `---` header is old line 41 and
        // did not end the file or reset its path.
        assert!(a.old.contains(&41) && a.old.contains(&42));
        assert_eq!(a.old_path.as_deref(), Some("src/a.rs"));
        assert_eq!(a.new.iter().copied().skip(5).collect::<Vec<_>>(), [41, 42]);
        // Line 14 exists only on the new side.
        assert!(idx.check(&lc("src/a.rs", DiffSide::New, 14), 5).is_ok());
        assert!(idx.check(&lc("src/a.rs", DiffSide::Old, 14), 5).is_err());
    }

    #[test]
    fn anchor_errors_say_what_is_commentable_and_suggest_the_other_side() {
        let idx = DiffIndex::parse(DIFF);
        let e = idx
            .check(&lc("src/a.rs", DiffSide::Old, 14), 5)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("no line 14 on the old side of PR #5's diff"),
            "{e}"
        );
        assert!(
            e.contains("Commentable old-side lines: 10-13, 40-42."),
            "{e}"
        );
        assert!(e.contains("use new:14"), "{e}");
        let e = idx
            .check(&lc("src/a.rs", DiffSide::New, 99), 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("10-14, 41-42"), "{e}");
        assert!(!e.contains("use old:"), "{e}");
        let e = idx
            .check(&lc("nope.rs", DiffSide::New, 1), 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("nope.rs is not a changed file in PR #5"), "{e}");
    }

    #[test]
    fn added_deleted_and_renamed_files() {
        let idx = DiffIndex::parse(DIFF);
        // Deleted: only the old side exists.
        assert!(idx.check(&lc("gone.rs", DiffSide::Old, 2), 5).is_ok());
        let e = idx
            .check(&lc("gone.rs", DiffSide::New, 1), 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no new-side lines"), "{e}");
        assert!(e.contains("use old:1"), "{e}");
        // Added: only the new side exists (and `+0,0` adds no line 0).
        assert!(idx.check(&lc("fresh.rs", DiffSide::New, 2), 5).is_ok());
        assert!(idx.check(&lc("fresh.rs", DiffSide::Old, 1), 5).is_err());
        assert!(idx.check(&lc("gone.rs", DiffSide::New, 0), 5).is_err());
        // Renamed with a change: anchored by the new path, tab stripped.
        assert!(idx.check(&lc("new name.txt", DiffSide::New, 1), 5).is_ok());
        assert!(idx.check(&lc("new name.txt", DiffSide::Old, 1), 5).is_ok());
        let e = idx
            .check(&lc("old name.txt", DiffSide::Old, 1), 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("was renamed to new name.txt"), "{e}");
        // Renamed with no change: in the PR, but nothing to comment on.
        let e = idx
            .check(&lc("moved2.rs", DiffSide::New, 1), 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no new-side lines"), "{e}");
        // Quoted path with octal escapes.
        assert!(idx.check(&lc("spécial.rs", DiffSide::New, 3), 5).is_ok());
    }

    #[test]
    fn empty_or_garbage_diff_matches_nothing() {
        for d in ["", "not a diff\nat all\n", "@@ nonsense @@\n"] {
            let idx = DiffIndex::parse(d);
            assert!(idx.check(&lc("a", DiffSide::New, 1), 1).is_err());
        }
    }

    #[test]
    fn hunk_header_counts_default_to_one() {
        assert_eq!(hunk_header("@@ -3 +4,2 @@ ctx"), Some((3, 1, 4, 2)));
        assert_eq!(hunk_header("@@ -0,0 +1 @@"), Some((0, 0, 1, 1)));
        assert_eq!(hunk_header("@@ -x +1 @@"), None);
        assert_eq!(hunk_header("+@@ -1 +1 @@"), None);
    }

    #[test]
    fn unquote_handles_escapes() {
        assert_eq!(unquote(r#""a\tb\\c\"d""#), "a\tb\\c\"d");
        assert_eq!(unquote(r#""\303\251""#), "é");
        assert_eq!(unquote("plain"), "plain");
    }

    #[test]
    fn ranges_are_compressed_and_capped() {
        let set: BTreeSet<u64> = [1, 2, 3, 7, 9, 10].into_iter().collect();
        assert_eq!(ranges(&set), "1-3, 7, 9-10");
        let many: BTreeSet<u64> = (0..12).map(|n| n * 2 + 1).collect();
        assert!(ranges(&many).ends_with("and 4 more ranges"));
    }

    #[test]
    fn submitted_lines_report_the_server_count() {
        assert_eq!(
            submitted_lines("APPROVED", 4, 0, 0),
            ("✓ Submitted APPROVED review on #4".to_string(), None)
        );
        let (head, warn) = submitted_lines("COMMENT", 4, 2, 2);
        assert_eq!(
            head,
            "✓ Submitted COMMENT review on #4 with 2 line comments"
        );
        assert!(warn.is_none());
        let (head, warn) = submitted_lines("COMMENT", 4, 2, 1);
        assert_eq!(head, "✓ Submitted COMMENT review on #4 with 1 line comment");
        assert!(warn.unwrap().contains("2 line comment(s) were sent"));
    }
}
