//! Redaction layer for `fj agent` — never ship secrets to an AI provider.
//!
//! Two defenses, both pure and hand-rolled (no regex dependency):
//!   1. [`is_sensitive_filename`] flags whole files (`.env`, private keys,
//!      credential stores) that should be withheld outright.
//!   2. [`redact_secrets`] scrubs secret-looking content (PEM private-key
//!      blocks, provider token prefixes, `secret = ...` assignments) from any
//!      text that *is* sent.
//!
//! [`redact_diff`] combines them over a unified diff: sensitive files are
//! dropped entirely, the rest are scrubbed. Callers also pass `extra_deny`
//! (e.g. paths git considers ignored) to widen the drop set.

/// Marker substituted for any redacted value.
pub const MASK: &str = "***REDACTED***";

/// Keywords that, when they name an assigned value, mark it as a secret.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "accesskey",
    "private_key",
    "client_secret",
    "auth_token",
    "authorization",
];

/// Token prefixes that uniquely identify a credential regardless of context.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",        // GitHub personal access token
    "gho_",        // GitHub OAuth
    "ghs_",        // GitHub app server
    "ghr_",        // GitHub refresh
    "github_pat_", // GitHub fine-grained PAT
    "xoxb-",       // Slack bot
    "xoxp-",       // Slack user
    "xoxa-",       // Slack app
    "xapp-",       // Slack app-level
    "sk-",         // OpenAI / Stripe-style secret keys
    "AKIA",        // AWS access key id
    "ASIA",        // AWS temporary access key id
    "AIza",        // Google API key
    "glpat-",      // GitLab PAT
];

/// True if a path's basename or extension marks it as secret-bearing and it
/// should never be sent to a provider. Conservative by design.
pub fn is_sensitive_filename(path: &str) -> bool {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = base.to_ascii_lowercase();

    // Dotenv files: `.env`, `.env.local`, `.env.production`, …
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    // Credential / secret stores by exact name.
    const SENSITIVE_NAMES: &[&str] = &[
        ".npmrc",
        ".pypirc",
        ".netrc",
        ".pgpass",
        ".htpasswd",
        "credentials",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "secrets.yml",
        "secrets.yaml",
    ];
    if SENSITIVE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    // Key-material extensions.
    const SENSITIVE_EXTS: &[&str] = &[
        "pem", "key", "p12", "pfx", "keystore", "jks", "ppk", "asc", "pkcs12",
    ];
    if let Some((_, ext)) = lower.rsplit_once('.') {
        if SENSITIVE_EXTS.contains(&ext) {
            return true;
        }
    }
    false
}

/// Scrub secret-looking content from `text`. Returns the scrubbed text and the
/// number of distinct redactions applied.
pub fn redact_secrets(text: &str) -> (String, usize) {
    let mut count = 0usize;
    let mut out = String::with_capacity(text.len());
    let mut in_pem = false;

    for line in text.split_inclusive('\n') {
        // Preserve a leading diff marker (`+`/`-`/space) so scrubbed diffs stay
        // valid; scan the rest of the line. PEM boundaries are detected on the
        // full line, since a `-`-led boundary would lose a dash to the marker.
        let (marker, body) = split_diff_marker(line);
        let key_boundary = line.contains("PRIVATE KEY");

        if in_pem {
            if key_boundary && line.contains("END") {
                in_pem = false;
            }
            push_masked_line(&mut out, marker);
            continue;
        }
        if key_boundary && line.contains("BEGIN") {
            in_pem = true;
            count += 1;
            push_masked_line(&mut out, marker);
            continue;
        }

        if let Some(masked) = mask_assignment(body) {
            count += 1;
            out.push_str(marker);
            out.push_str(&masked);
        } else if let Some(masked) = mask_known_token(body) {
            count += 1;
            out.push_str(marker);
            out.push_str(&masked);
        } else {
            out.push_str(line);
        }
    }
    (out, count)
}

/// A redacted unified diff plus what redaction did.
#[derive(Debug, Default, PartialEq)]
pub struct RedactedDiff {
    pub text: String,
    /// Files whose hunks were dropped wholesale (sensitive or denied).
    pub dropped_files: Vec<String>,
    /// Count of inline secrets masked in the surviving hunks.
    pub secrets_masked: usize,
}

/// Redact a unified diff: drop sensitive / denied files entirely, scrub the
/// rest. `extra_deny` holds additional paths to drop (e.g. git-ignored files).
pub fn redact_diff(diff: &str, extra_deny: &[String]) -> RedactedDiff {
    let mut result = RedactedDiff::default();
    for section in split_diff_sections(diff) {
        let path = section_path(section);
        let denied = path
            .as_deref()
            .is_some_and(|p| is_sensitive_filename(p) || extra_deny.iter().any(|d| d == p));
        if denied {
            if let Some(p) = path {
                result.dropped_files.push(p);
            }
            continue;
        }
        let (scrubbed, n) = redact_secrets(section);
        result.secrets_masked += n;
        result.text.push_str(&scrubbed);
    }
    result
}

// ── internals ────────────────────────────────────────────────────────────

/// Peel a single leading diff column (`+`, `-`, or space) off a line so the
/// rest can be inspected. Lines that don't look like diff body pass through.
fn split_diff_marker(line: &str) -> (&str, &str) {
    let mut chars = line.char_indices();
    match chars.next() {
        Some((_, '+')) | Some((_, '-')) | Some((_, ' ')) => line.split_at(1),
        _ => ("", line),
    }
}

fn push_masked_line(out: &mut String, marker: &str) {
    out.push_str(marker);
    out.push_str(MASK);
    out.push('\n');
}

/// Mask `key = value` / `key: value` when the key names a secret. Returns the
/// masked body (without the diff marker) or None if nothing matched.
fn mask_assignment(body: &str) -> Option<String> {
    let sep = body.find(['=', ':'])?;
    let (key_part, _) = body.split_at(sep);
    let key = key_part
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        .to_ascii_lowercase();
    // Compare the final identifier segment so `app.client_secret` still hits.
    let ident = key
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .next()
        .unwrap_or(&key);
    let matched = SECRET_KEYS
        .iter()
        .any(|k| ident == *k || key.ends_with(k) || key == *k);
    if !matched {
        return None;
    }
    let sep_char = &body[sep..sep + 1];
    // Only mask when there is an actual value after the separator.
    let value = body[sep + 1..].trim();
    if value.is_empty() {
        return None;
    }
    let trailing = line_trailing(body);
    Some(format!("{key_part}{sep_char} {MASK}{trailing}"))
}

/// Mask a line that contains a recognizable credential token prefix.
fn mask_known_token(body: &str) -> Option<String> {
    let mut masked = body.to_string();
    let mut hit = false;
    for token in body
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '=' | ':' | ',' | '(' | ')'))
    {
        if token.len() < 8 {
            continue;
        }
        if TOKEN_PREFIXES.iter().any(|p| token.starts_with(p)) {
            masked = masked.replace(token, MASK);
            hit = true;
        }
    }
    if hit {
        Some(masked)
    } else {
        None
    }
}

/// Trailing structural characters (newline) to re-attach after masking.
fn line_trailing(body: &str) -> &str {
    if body.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// Split a unified diff into per-file sections, each starting at a
/// `diff --git` header. Any preamble before the first header is its own
/// leading section (kept verbatim).
fn split_diff_sections(diff: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0;
    let mut pos = 0;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") && pos != start {
            sections.push(&diff[start..pos]);
            start = pos;
        }
        pos += line.len();
    }
    if start < diff.len() {
        sections.push(&diff[start..]);
    }
    sections
}

/// Extract the post-image path (`b/<path>`) from a diff section header.
fn section_path(section: &str) -> Option<String> {
    for line in section.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/path b/path` — take the b-side.
            if let Some(b) = rest.split(" b/").nth(1) {
                return Some(b.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_sensitive_filenames() {
        for p in [
            ".env",
            ".env.production",
            "config/.env.local",
            "deploy/id_rsa",
            "certs/server.pem",
            "secret.key",
            "store.p12",
            "app/.npmrc",
            "secrets.yaml",
        ] {
            assert!(is_sensitive_filename(p), "{p} should be sensitive");
        }
        for p in ["src/main.rs", "README.md", "env.example", "keyboard.rs"] {
            assert!(!is_sensitive_filename(p), "{p} should be allowed");
        }
    }

    #[test]
    fn masks_secret_assignments() {
        let (out, n) = redact_secrets("PASSWORD = hunter2\nname = alice\n");
        assert!(out.contains(&format!("PASSWORD = {MASK}")));
        assert!(out.contains("name = alice")); // non-secret untouched
        assert_eq!(n, 1);

        let (out, n) = redact_secrets("  client_secret: \"abc123\"\n");
        assert!(out.contains(MASK));
        assert_eq!(n, 1);
        // nested key path still resolves to the secret identifier
        let (out, _) = redact_secrets("app.api_key=zzz\n");
        assert!(out.contains(MASK));
        let _ = out;
    }

    #[test]
    fn empty_value_is_not_masked() {
        let (out, n) = redact_secrets("password =\n");
        assert_eq!(n, 0);
        assert_eq!(out, "password =\n");
    }

    #[test]
    fn masks_known_token_prefixes() {
        // Build the fake token at runtime so no full token literal sits in
        // source (the repo's secret-denylist scan flags long `ghp_…` strings).
        let fake = format!("ghp_{}", "fakeexample0123");
        let (out, n) = redact_secrets(&format!("export TOK={fake}\n"));
        // `TOK` isn't a secret key name, so the token prefix is what catches it.
        assert!(out.contains(MASK));
        assert!(!out.contains(&fake));
        assert_eq!(n, 1);

        let akia = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let (out, n) = redact_secrets(&format!("see {akia} in the logs\n"));
        assert!(out.contains(MASK));
        assert!(!out.contains(&akia));
        assert_eq!(n, 1);
    }

    #[test]
    fn masks_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEoAIB...line1\nline2...\n-----END RSA PRIVATE KEY-----\n";
        let (out, n) = redact_secrets(pem);
        assert!(!out.contains("MIIEoAIB"));
        assert_eq!(out.matches(MASK).count(), 4); // begin + 2 body + end
        assert_eq!(n, 1); // one block = one redaction
    }

    #[test]
    fn preserves_diff_markers_when_scrubbing() {
        let diff = "+password = hunter2\n unchanged\n-old = value\n";
        let (out, _) = redact_secrets(diff);
        assert!(out.starts_with('+'));
        assert!(out.contains(&format!("+password = {MASK}")));
        assert!(out.contains(" unchanged\n"));
    }

    #[test]
    fn redact_diff_drops_sensitive_files_and_scrubs_rest() {
        let diff = "\
diff --git a/.env b/.env
new file mode 100644
--- /dev/null
+++ b/.env
@@ -0,0 +1 @@
+SECRET_TOKEN=supersecret
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,1 +1,2 @@
 fn main() {}
+const PASSWORD: &str = \"hunter2\";
";
        let r = redact_diff(diff, &[]);
        assert_eq!(r.dropped_files, vec![".env".to_string()]);
        assert!(!r.text.contains("supersecret"));
        assert!(!r.text.contains(".env")); // whole section dropped
        assert!(r.text.contains("src/app.rs")); // kept
        assert!(r.text.contains(MASK)); // PASSWORD line scrubbed
        assert_eq!(r.secrets_masked, 1);
    }

    #[test]
    fn redact_diff_honors_extra_deny() {
        let diff = "\
diff --git a/build/out.log b/build/out.log
--- a/build/out.log
+++ b/build/out.log
@@ -0,0 +1 @@
+noise
";
        let r = redact_diff(diff, &["build/out.log".to_string()]);
        assert_eq!(r.dropped_files, vec!["build/out.log".to_string()]);
        assert!(r.text.is_empty());
    }

    #[test]
    fn section_path_reads_b_side() {
        assert_eq!(
            section_path("diff --git a/x/y.rs b/x/y.rs\n--- a/x/y.rs\n+++ b/x/y.rs\n"),
            Some("x/y.rs".to_string())
        );
        // renames: +++ b/ wins
        assert_eq!(
            section_path("diff --git a/old b/new\n--- a/old\n+++ b/new\n"),
            Some("new".to_string())
        );
    }
}
