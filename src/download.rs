use anyhow::{anyhow, Context, Result};
use std::{future::Future, io::Write, path::Path};

pub(crate) async fn verified_body<F, Fut>(
    mut request: F,
    known_size: Option<u64>,
    zip: bool,
    label: &str,
) -> Result<Vec<u8>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<reqwest::Response>>,
{
    for attempt in 1..=3 {
        let mut expected = known_size;
        let mut bytes = Vec::new();
        let result = match request().await {
            Ok(response) => {
                let mut response = response.error_for_status()?;
                crate::client::check_body_size(&response)?;
                let content_length = response.content_length();
                expected = known_size.or(content_length);
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            append_capped(&mut bytes, &chunk, crate::client::MAX_RESPONSE_BYTES)?
                        }
                        Ok(None) => {
                            break validate(&bytes, content_length, zip)
                                .and_then(|()| validate_size(&bytes, known_size));
                        }
                        Err(error) => break Err(anyhow!("reading body: {error}")),
                    }
                }
            }
            Err(error) => {
                if !error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
                    error.is_connect() || error.is_timeout() || error.is_request()
                }) {
                    return Err(error);
                }
                Err(error)
            }
        };
        match result {
            Ok(()) => return Ok(bytes),
            Err(error) => {
                let size = match expected {
                    Some(expected) => {
                        format!("expected {expected} bytes, received {} bytes", bytes.len())
                    }
                    None => format!("received {} bytes", bytes.len()),
                };
                if attempt == 3 {
                    return Err(anyhow!(
                        "{label} download incomplete after 3 attempts ({size}): {error}"
                    ));
                }
                eprintln!(
                    "{label} was cut short ({} bytes); retrying {}/3",
                    bytes.len(),
                    attempt + 1
                );
            }
        }
    }
    unreachable!()
}

/// Content-Length may be absent (chunked), so the cap is also kept while reading.
fn append_capped(bytes: &mut Vec<u8>, chunk: &[u8], cap: u64) -> Result<()> {
    if (bytes.len() + chunk.len()) as u64 > cap {
        return Err(anyhow!(
            "response body is over the {cap}-byte cap; refusing to buffer it"
        ));
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn validate_size(bytes: &[u8], known_size: Option<u64>) -> Result<()> {
    if known_size.is_some_and(|size| size != bytes.len() as u64) {
        return Err(anyhow!("release asset size mismatch"));
    }
    Ok(())
}

fn validate(bytes: &[u8], expected: Option<u64>, zip: bool) -> Result<()> {
    if expected.is_some_and(|size| size != bytes.len() as u64) {
        return Err(anyhow!("Content-Length mismatch"));
    }
    if !zip {
        return Ok(());
    }
    let start = bytes.len().saturating_sub(65_536 + 22);
    for offset in (start..bytes.len().saturating_sub(21)).rev() {
        let end = &bytes[offset..];
        if !end.starts_with(b"PK\x05\x06") {
            continue;
        }
        let comment = u16::from_le_bytes(end[20..22].try_into().unwrap()) as usize;
        if end.len() != 22 + comment {
            continue;
        }
        let mut size = u32::from_le_bytes(end[12..16].try_into().unwrap()) as u64;
        let mut directory = u32::from_le_bytes(end[16..20].try_into().unwrap()) as u64;
        let mut directory_end = offset as u64;
        if end[10..12] == [0xff, 0xff] || size == 0xffff_ffff || directory == 0xffff_ffff {
            // ZIP64: the real size and offset are in the ZIP64 end record,
            // found through the locator just before this record.
            let Some(locator) = offset.checked_sub(20).map(|at| &bytes[at..offset]) else {
                continue;
            };
            if !locator.starts_with(b"PK\x06\x07") {
                continue;
            }
            let record = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            let Some(fields) = usize::try_from(record)
                .ok()
                .filter(|&at| at + 56 <= offset - 20)
                .map(|at| &bytes[at..at + 56])
            else {
                continue;
            };
            if !fields.starts_with(b"PK\x06\x06") {
                continue;
            }
            size = u64::from_le_bytes(fields[40..48].try_into().unwrap());
            directory = u64::from_le_bytes(fields[48..56].try_into().unwrap());
            directory_end = record;
        }
        if directory
            .checked_add(size)
            .is_none_or(|end| end > directory_end)
        {
            continue;
        }
        // Empty archives have no first central-directory entry.
        if size == 0 && end[8..12] == [0, 0, 0, 0] {
            return Ok(());
        }
        let directory = directory as usize;
        if size >= 46 && bytes.get(directory..directory + 4) == Some(b"PK\x01\x02") {
            return Ok(());
        }
    }
    Err(anyhow!(
        "invalid zip: missing or incomplete central directory"
    ))
}

pub(crate) fn write_atomic(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder.tempfile_in(parent)?;
    match std::fs::metadata(destination) {
        Ok(metadata) => temporary
            .as_file()
            .set_permissions(metadata.permissions())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    temporary.write_all(bytes).context("writing download")?;
    temporary
        .persist(destination)
        .context("installing download")?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn zip() -> Vec<u8> {
        // One empty stored file named "a", followed by its directory and EOCD.
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[20, 0]);
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend_from_slice(&[1, 0, 0, 0, b'a']);
        let directory = bytes.len() as u32;
        bytes.extend_from_slice(b"PK\x01\x02");
        bytes.extend_from_slice(&[20, 0, 20, 0]);
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[0; 16]);
        bytes.push(b'a');
        let size = bytes.len() as u32 - directory;
        bytes.extend_from_slice(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&directory.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn zip_validation_checks_directory_bounds_signature_and_comment() {
        let valid = zip();
        validate(&valid, None, true).unwrap();
        let mut invalid = valid.clone();
        let end = invalid.len() - 22;
        invalid[end + 16..end + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate(&invalid, None, true).is_err());
        invalid = valid.clone();
        invalid[31] = 0;
        assert!(validate(&invalid, None, true).is_err());
        invalid = valid.clone();
        invalid[end + 20] = 1;
        assert!(validate(&invalid, None, true).is_err());
        for length in 0..valid.len() {
            assert!(validate(&valid[..length], None, true).is_err());
        }
        assert!(validate(&valid, Some(valid.len() as u64 + 1), false).is_err());
    }

    fn zip64(directory_offset: u64) -> Vec<u8> {
        let mut bytes = vec![0; 4];
        bytes.extend_from_slice(b"PK\x01\x02");
        bytes.extend_from_slice(&[0; 42]);
        let record = bytes.len() as u64;
        bytes.extend_from_slice(b"PK\x06\x06");
        bytes.extend_from_slice(&[0; 36]);
        bytes.extend_from_slice(&46u64.to_le_bytes());
        bytes.extend_from_slice(&directory_offset.to_le_bytes());
        bytes.extend_from_slice(b"PK\x06\x07\0\0\0\0");
        bytes.extend_from_slice(&record.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"PK\x05\x06\0\0\0\0\xff\xff\xff\xff");
        bytes.extend_from_slice(&[0xff; 8]);
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn reading_stops_at_the_cap() {
        let mut bytes = Vec::new();
        append_capped(&mut bytes, b"1234", 6).unwrap();
        assert!(append_capped(&mut bytes, b"567", 6).is_err());
        assert_eq!(bytes, b"1234");
    }

    #[test]
    fn zip_validation_reads_zip64_end_records() {
        validate(&zip64(4), None, true).unwrap();
        // The ZIP64 directory runs past its end record.
        assert!(validate(&zip64(40), None, true).is_err());
        // A sentinel without a locator is not a valid end record.
        let mut no_locator = zip64(4);
        let at = no_locator.len() - 42;
        no_locator[at..at + 4].copy_from_slice(b"XXXX");
        assert!(validate(&no_locator, None, true).is_err());
    }

    #[test]
    fn zip_validation_accepts_empty_archive_and_maximum_comment() {
        let mut empty = b"PK\x05\x06".to_vec();
        empty.extend_from_slice(&[0; 18]);
        validate(&empty, None, true).unwrap();
        let mut commented = zip();
        let end = commented.len();
        commented[end - 2..].copy_from_slice(&u16::MAX.to_le_bytes());
        commented.extend_from_slice(&vec![b'x'; u16::MAX as usize]);
        let end = commented.len();
        commented[end - 30..end - 26].copy_from_slice(b"PK\x05\x06");
        validate(&commented, None, true).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_uses_default_mode_and_preserves_replacement_mode() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let ordinary = directory.path().join("ordinary");
        std::fs::write(&ordinary, b"reference").unwrap();
        let destination = directory.path().join("asset");
        write_atomic(&destination, b"new").unwrap();
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode(),
            std::fs::metadata(&ordinary).unwrap().permissions().mode()
        );
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o751)).unwrap();
        write_atomic(&destination, b"replacement").unwrap();
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }

    #[test]
    fn atomic_write_preserves_existing_file_on_install_failure() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing-directory");
        std::fs::create_dir(&destination).unwrap();
        assert!(write_atomic(&destination, b"body").is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        std::fs::write(&destination, b"old").unwrap();
        write_atomic(&destination, b"new").unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
