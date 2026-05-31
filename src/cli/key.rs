use anyhow::Result;
use clap::{Args, Subcommand};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::output;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKey {
    pub id: u64,
    pub key: String,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpgKey {
    pub id: u64,
    pub key_id: String,
    pub public_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub primary_key_id: String,
}

#[derive(Debug, Args)]
pub struct SshKeyCmd {
    #[command(subcommand)]
    pub command: SshKeySub,
}

#[derive(Debug, Subcommand)]
pub enum SshKeySub {
    /// List SSH keys on your account.
    List(ListArgs),
    /// Add an SSH key.
    Add(AddArgs),
    /// Remove an SSH key by id.
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct GpgKeyCmd {
    #[command(subcommand)]
    pub command: GpgKeySub,
}

#[derive(Debug, Subcommand)]
pub enum GpgKeySub {
    /// List GPG keys on your account.
    List(ListArgs),
    /// Add a GPG key.
    Add(GpgAddArgs),
    /// Remove a GPG key by id.
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    pub title: String,
    /// SSH public key. Pass `--from-file ~/.ssh/id_ed25519.pub` instead for files.
    #[arg(long)]
    pub key: Option<String>,
    #[arg(long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct GpgAddArgs {
    /// Armored public GPG key. Use `--from-file -` for stdin.
    #[arg(long)]
    pub key: Option<String>,
    #[arg(long = "from-file")]
    pub from_file: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    pub id: u64,
    #[arg(short = 'y', long)]
    pub yes: bool,
}

pub async fn run_ssh(cmd: SshKeyCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        SshKeySub::List(args) => list_ssh(&client, args).await,
        SshKeySub::Add(args) => add_ssh(&client, args).await,
        SshKeySub::Delete(args) => delete_ssh(&client, args).await,
    }
}

async fn list_ssh(client: &Client, args: ListArgs) -> Result<()> {
    let keys: Vec<PublicKey> = client
        .json(Method::GET, "/api/v1/user/keys", &[], None::<&()>)
        .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&keys)?);
    }
    if keys.is_empty() {
        println!("(no SSH keys)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|k| {
            vec![
                k.id.to_string(),
                k.title.clone(),
                output::dim(&fingerprint_preview(&k.key)),
                output::dim(&output::relative_time(k.created_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["ID", "TITLE", "KEY", "CREATED"], &rows)
    );
    Ok(())
}

async fn add_ssh(client: &Client, args: AddArgs) -> Result<()> {
    let key = read_key(args.key.as_deref(), args.from_file.as_deref())?;
    #[derive(Serialize)]
    struct Body<'a> {
        title: &'a str,
        key: &'a str,
    }
    let body = Body {
        title: &args.title,
        key: &key,
    };
    let k: PublicKey = client
        .json(Method::POST, "/api/v1/user/keys", &[], Some(&body))
        .await?;
    println!("✓ Added SSH key '{}' (id {})", k.title, k.id);
    Ok(())
}

async fn delete_ssh(client: &Client, args: DeleteArgs) -> Result<()> {
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete SSH key {}? Type '{}' to confirm",
            args.id, args.id
        ))?;
        if ans != args.id.to_string() {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    let path = format!("/api/v1/user/keys/{}", args.id);
    client
        .request(Method::DELETE, &path, &[], None)
        .await?
        .error_for_status()?;
    println!("✓ Deleted SSH key {}", args.id);
    Ok(())
}

pub async fn run_gpg(cmd: GpgKeyCmd, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    match cmd.command {
        GpgKeySub::List(args) => list_gpg(&client, args).await,
        GpgKeySub::Add(args) => add_gpg(&client, args).await,
        GpgKeySub::Delete(args) => delete_gpg(&client, args).await,
    }
}

async fn list_gpg(client: &Client, args: ListArgs) -> Result<()> {
    let keys: Vec<GpgKey> = client
        .json(Method::GET, "/api/v1/user/gpg_keys", &[], None::<&()>)
        .await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&keys)?);
    }
    if keys.is_empty() {
        println!("(no GPG keys)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|k| {
            vec![
                k.id.to_string(),
                k.key_id.clone(),
                output::dim(&output::relative_time(k.created_at)),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["ID", "KEY ID", "CREATED"], &rows)
    );
    Ok(())
}

async fn add_gpg(client: &Client, args: GpgAddArgs) -> Result<()> {
    let key = read_key(args.key.as_deref(), args.from_file.as_deref())?;
    let k: GpgKey = client
        .json(
            Method::POST,
            "/api/v1/user/gpg_keys",
            &[],
            Some(&serde_json::json!({ "armored_public_key": key })),
        )
        .await?;
    println!("✓ Added GPG key {} (id {})", k.key_id, k.id);
    Ok(())
}

async fn delete_gpg(client: &Client, args: DeleteArgs) -> Result<()> {
    if !args.yes {
        let ans = crate::cli::editor::prompt_line(&format!(
            "Delete GPG key {}? Type '{}' to confirm",
            args.id, args.id
        ))?;
        if ans != args.id.to_string() {
            return Err(anyhow::anyhow!("aborted"));
        }
    }
    let path = format!("/api/v1/user/gpg_keys/{}", args.id);
    client
        .request(Method::DELETE, &path, &[], None)
        .await?
        .error_for_status()?;
    println!("✓ Deleted GPG key {}", args.id);
    Ok(())
}

fn read_key(value: Option<&str>, from_file: Option<&str>) -> Result<String> {
    if let Some(v) = value {
        return Ok(v.trim().to_string());
    }
    if let Some(path) = from_file {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            return Ok(buf.trim().to_string());
        }
        return Ok(std::fs::read_to_string(path)?.trim().to_string());
    }
    Err(anyhow::anyhow!("pass --key or --from-file"))
}

/// Compact preview of an SSH public key: `ssh-ed25519 …abc12`
fn fingerprint_preview(key: &str) -> String {
    let mut parts = key.split_whitespace();
    let kind = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    let tail = body
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{kind} …{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_key_trims_inline_value() {
        let got = read_key(Some("  ssh-ed25519 AAAA user@host \n"), None).unwrap();
        assert_eq!(got, "ssh-ed25519 AAAA user@host");
    }

    #[test]
    fn read_key_reads_and_trims_a_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "ssh-rsa BBBB host\n").unwrap();
        let got = read_key(None, Some(f.path().to_str().unwrap())).unwrap();
        assert_eq!(got, "ssh-rsa BBBB host");
    }

    #[test]
    fn read_key_requires_a_source() {
        assert!(read_key(None, None).is_err());
    }

    #[test]
    fn read_key_prefers_inline_over_file() {
        // With both supplied, the inline value wins and the file is untouched.
        let got = read_key(Some("inline-key"), Some("/nonexistent/path")).unwrap();
        assert_eq!(got, "inline-key");
    }

    #[test]
    fn fingerprint_preview_shows_kind_and_last_eight() {
        assert_eq!(
            fingerprint_preview("ssh-ed25519 AAAAAAAA12345678 me@host"),
            "ssh-ed25519 …12345678",
        );
    }

    #[test]
    fn fingerprint_preview_handles_short_and_empty_input() {
        assert_eq!(fingerprint_preview("ssh-rsa AB"), "ssh-rsa …AB");
        assert_eq!(fingerprint_preview(""), " …");
    }
}
