//! Host binaries uploaded by `ruro deploy --host-binary`.
//!
//! A binary arrives as a sequence of chunks, appended in order to
//! `<name>.part`. When the last chunk arrives the file's SHA-256 is checked
//! against the one the client computed, and only then is it made executable
//! and renamed to `<name>`, where groups with `host: <name>` find it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use ruro_proto::UploadChunk;
use sha2::{Digest, Sha256};

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Stores one chunk in `dir`. Returns a progress or completion message.
pub fn receive(dir: &Path, chunk: &UploadChunk) -> Result<String> {
    ensure!(
        ruro_graph::is_valid_ident(&chunk.name),
        "host name `{}` must match [A-Za-z0-9_-]+",
        chunk.name
    );
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let part = dir.join(format!("{}.part", chunk.name));
    let target = dir.join(&chunk.name);
    let label = format!("host `{}`", chunk.name);
    let reply = store(&part, &target, &label, chunk)?;
    if reply.starts_with("stored") {
        tracing::info!(
            "stored host binary `{}` ({} bytes)",
            chunk.name,
            chunk.total
        );
    }
    Ok(reply)
}

/// Appends one chunk to `part`; after the last one, checks the file's
/// SHA-256, makes it executable and durable, and renames it to `target`.
/// `label` names the binary in replies.
pub fn store(part: &Path, target: &Path, label: &str, chunk: &UploadChunk) -> Result<String> {
    ensure!(
        chunk.sha256.len() == 64 && chunk.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "malformed SHA-256"
    );
    let end = chunk
        .offset
        .checked_add(chunk.data.len() as u64)
        .context("chunk offset overflows")?;
    ensure!(end <= chunk.total, "chunk ends past the file size");
    let mut file = if chunk.offset == 0 {
        File::create(part)
    } else {
        OpenOptions::new().append(true).open(part)
    }
    .with_context(|| format!("opening {}", part.display()))?;
    let have = file.metadata()?.len();
    if have != chunk.offset {
        bail!(
            "chunk at offset {} does not follow the {have} bytes received so far; restart the upload",
            chunk.offset
        );
    }
    file.write_all(&chunk.data)?;
    if end < chunk.total {
        return Ok(format!("received {end}/{} bytes", chunk.total));
    }
    file.sync_all()?;
    drop(file);
    let sum = sha256_file(part)?;
    if !sum.eq_ignore_ascii_case(&chunk.sha256) {
        let _ = std::fs::remove_file(part);
        bail!(
            "checksum mismatch for {label}: expected {}, got {sum}; the upload was discarded",
            chunk.sha256
        );
    }
    std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(part, target)?;
    Ok(format!(
        "stored {label} ({} bytes, sha256 {sum})",
        chunk.total
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(name: &str, data: &[u8], sha: &str, size: usize) -> Vec<UploadChunk> {
        data.chunks(size)
            .scan(0u64, |off, c| {
                let chunk = UploadChunk {
                    name: name.into(),
                    offset: *off,
                    total: data.len() as u64,
                    sha256: sha.into(),
                    data: c.to_vec(),
                };
                *off += c.len() as u64;
                Some(chunk)
            })
            .collect()
    }

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ruro-upload-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stores_verified_binaries() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        let src = dir().join("source.bin");
        std::fs::write(&src, &data).unwrap();
        let sha = sha256_file(&src).unwrap();
        let hosts = dir().join("hosts-ok");
        let mut last = String::new();
        for c in chunks("my-host", &data, &sha, 4096) {
            last = receive(&hosts, &c).unwrap();
        }
        assert!(last.contains("stored host `my-host`"), "{last}");
        let stored = hosts.join("my-host");
        assert_eq!(std::fs::read(&stored).unwrap(), data);
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
    }

    #[test]
    fn rejects_bad_checksums_and_gaps() {
        let hosts = dir().join("hosts-bad");
        let data = vec![1u8; 100];
        let wrong = "0".repeat(64);
        let err = receive(&hosts, &chunks("h", &data, &wrong, 100)[0]).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"));
        assert!(!hosts.join("h").exists());
        assert!(!hosts.join("h.part").exists());

        let cs = chunks("g", &data, &wrong, 30);
        receive(&hosts, &cs[0]).unwrap();
        assert!(receive(&hosts, &cs[2]).is_err(), "out-of-order chunk");
        let mut bad = cs[0].clone();
        bad.name = "../escape".into();
        assert!(receive(&hosts, &bad).is_err());
    }
}
