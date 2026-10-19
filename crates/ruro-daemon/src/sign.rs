//! Signing keys and signatures for daemon updates.
//!
//! An update binary is signed with an Ed25519 key. The signature covers the
//! binary's SHA-256 and what it claims to be: its role (the daemon or its
//! host), its version and the target triple it was built for, so none of
//! them can be changed without the key. Daemons accept updates signed by
//! the public keys listed in their `--update-keys` file.
//!
//! Key files are text:
//!
//! - a secret key (`ruro keygen` writes it readable only by its owner):
//!   `ruro-update-secret-key <base64 PKCS#8>`;
//! - a public key: `ruro-update-key <base64> [comment]`. A trusted-keys file
//!   lists one per line; blank lines and lines starting with `#` are ignored.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use ruro_proto::{SignedBinary, UpdateRole};

const SECRET_PREFIX: &str = "ruro-update-secret-key";
const PUBLIC_PREFIX: &str = "ruro-update-key";

/// The bytes a signature covers.
pub fn message(role: UpdateRole, version: &str, target: &str, sha256: &str) -> Vec<u8> {
    format!(
        "ruro-update-v1\nrole={}\nversion={version}\ntarget={target}\nsha256={}\n",
        role.as_str(),
        sha256.to_ascii_lowercase()
    )
    .into_bytes()
}

/// An Ed25519 public key a daemon may trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// Parses `ruro-update-key <base64> [comment]`, or the bare base64.
    pub fn parse(line: &str) -> Result<Self> {
        let mut words = line.split_whitespace();
        let mut b64 = words.next().context("empty key")?;
        if b64 == PUBLIC_PREFIX {
            b64 = words.next().context("no key after `ruro-update-key`")?;
        }
        Self::from_base64(b64)
    }

    fn from_base64(b64: &str) -> Result<Self> {
        let bytes = BASE64
            .decode(b64)
            .map_err(|e| anyhow!("malformed key: {e}"))?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("malformed key: an Ed25519 public key is 32 bytes"))?;
        Ok(Self(key))
    }

    pub fn to_base64(&self) -> String {
        BASE64.encode(self.0)
    }

    /// The line `ruro keygen` writes to the `.pub` file.
    pub fn to_line(&self, comment: &str) -> String {
        format!("{PUBLIC_PREFIX} {} {comment}", self.to_base64())
            .trim_end()
            .to_owned()
    }

    /// A short form for messages: the start of the base64.
    pub fn short(&self) -> String {
        self.to_base64()[..12].to_owned()
    }
}

/// Reads the public keys a daemon trusts.
pub fn read_trusted(path: &Path) -> Result<Vec<PublicKey>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the update keys {}", path.display()))?;
    let mut keys = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = PublicKey::parse(line)
            .with_context(|| format!("{}, line {}", path.display(), i + 1))?;
        keys.push(key);
    }
    ensure!(
        !keys.is_empty(),
        "the update keys file {} lists no key",
        path.display()
    );
    Ok(keys)
}

/// A signing key.
pub struct SecretKey(Ed25519KeyPair);

impl SecretKey {
    /// A new random key, and the text of its secret key file.
    pub fn generate() -> Result<(Self, String)> {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| anyhow!("generating a key failed"))?;
        let text = format!("{SECRET_PREFIX} {}\n", BASE64.encode(pkcs8.as_ref()));
        Ok((Self::from_pkcs8(pkcs8.as_ref())?, text))
    }

    fn from_pkcs8(bytes: &[u8]) -> Result<Self> {
        Ed25519KeyPair::from_pkcs8(bytes)
            .map(Self)
            .map_err(|e| anyhow!("malformed secret key: {e}"))
    }

    /// Reads a secret key file written by [`SecretKey::generate`].
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the secret key {}", path.display()))?;
        let mut words = text.split_whitespace();
        if words.next() != Some(SECRET_PREFIX) {
            bail!(
                "{} is not a ruro update secret key (made with `ruro keygen`)",
                path.display()
            );
        }
        let bytes = BASE64
            .decode(words.next().unwrap_or_default())
            .map_err(|e| anyhow!("{}: malformed secret key: {e}", path.display()))?;
        Self::from_pkcs8(&bytes).with_context(|| path.display().to_string())
    }

    pub fn public(&self) -> PublicKey {
        let bytes: [u8; 32] = self
            .0
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public keys are 32 bytes");
        PublicKey(bytes)
    }

    /// Signs what a binary claims to be.
    pub fn sign(
        &self,
        role: UpdateRole,
        version: &str,
        target: &str,
        sha256: &str,
    ) -> SignedBinary {
        let sig = self.0.sign(&message(role, version, target, sha256));
        SignedBinary {
            role,
            version: version.to_owned(),
            target: target.to_owned(),
            sha256: sha256.to_ascii_lowercase(),
            key: self.public().to_base64(),
            signature: BASE64.encode(sig.as_ref()),
        }
    }
}

/// Checks that `signed` was signed by one of the `trusted` keys and that
/// none of its fields changed since.
pub fn verify(signed: &SignedBinary, trusted: &[PublicKey]) -> Result<()> {
    let key = PublicKey::from_base64(&signed.key).context("the signature names no valid key")?;
    if !trusted.contains(&key) {
        bail!(
            "the {} binary is signed with key {}…, which this daemon does not trust",
            signed.role.as_str(),
            key.short()
        );
    }
    let sig = BASE64
        .decode(&signed.signature)
        .map_err(|e| anyhow!("malformed signature: {e}"))?;
    let msg = message(signed.role, &signed.version, &signed.target, &signed.sha256);
    UnparsedPublicKey::new(&ED25519, key.0)
        .verify(&msg, &sig)
        .map_err(|_| {
            anyhow!(
                "the signature of the {} binary does not match it: the binary, its version or target was changed after signing",
                signed.role.as_str()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn signatures_verify_against_trusted_keys_only() {
        let (key, secret_text) = SecretKey::generate().unwrap();
        let signed = key.sign(UpdateRole::Daemon, "0.2.0", "x86_64-unknown-linux-gnu", SHA);
        verify(&signed, &[key.public()]).unwrap();

        // The secret key file reads back to the same key.
        let dir = std::env::temp_dir().join(format!("ruro-sign-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.key");
        std::fs::write(&path, &secret_text).unwrap();
        assert_eq!(SecretKey::read(&path).unwrap().public(), key.public());
        std::fs::write(&path, key.public().to_line("laptop")).unwrap();
        assert!(
            SecretKey::read(&path).is_err(),
            "a public key is no secret key"
        );
        assert_eq!(read_trusted(&path).unwrap(), [key.public()]);
        std::fs::write(&path, "# no keys\n\n").unwrap();
        assert!(read_trusted(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);

        // Another key, even with a valid signature of its own, is refused.
        let (other, _) = SecretKey::generate().unwrap();
        let err = verify(&signed, &[other.public()]).unwrap_err().to_string();
        assert!(err.contains("does not trust"), "{err}");
        let by_other = other.sign(UpdateRole::Daemon, "0.2.0", "x86_64-unknown-linux-gnu", SHA);
        assert!(verify(&by_other, &[key.public()]).is_err());
    }

    #[test]
    fn every_signed_field_is_covered() {
        let (key, _) = SecretKey::generate().unwrap();
        let trusted = [key.public()];
        let signed = key.sign(UpdateRole::Daemon, "0.2.0", "x86_64-unknown-linux-gnu", SHA);
        let tampered = [
            SignedBinary {
                role: UpdateRole::Host,
                ..signed.clone()
            },
            SignedBinary {
                version: "9.0.0".into(),
                ..signed.clone()
            },
            SignedBinary {
                target: "aarch64-unknown-linux-gnu".into(),
                ..signed.clone()
            },
            SignedBinary {
                sha256: "0".repeat(64),
                ..signed.clone()
            },
        ];
        for t in &tampered {
            let err = verify(t, &trusted).unwrap_err().to_string();
            assert!(err.contains("does not match"), "{t:?}: {err}");
        }
        let garbled = SignedBinary {
            signature: "not base64!".into(),
            ..signed
        };
        assert!(verify(&garbled, &trusted).is_err());
    }

    #[test]
    fn public_keys_parse_with_or_without_their_prefix() {
        let (key, _) = SecretKey::generate().unwrap();
        let line = key.public().to_line("ci signing key");
        assert!(line.starts_with("ruro-update-key "), "{line}");
        assert_eq!(PublicKey::parse(&line).unwrap(), key.public());
        assert_eq!(
            PublicKey::parse(&key.public().to_base64()).unwrap(),
            key.public()
        );
        assert!(PublicKey::parse("ruro-update-key AAAA").is_err());
    }
}
