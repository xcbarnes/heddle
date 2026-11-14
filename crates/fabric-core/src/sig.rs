//! Hashes and the admin key's signatures: SHA-256, and Ed25519 to verify only.
//!
//! Every signed payload starts with a context tag, so a signature made for
//! one purpose can never be replayed as another (a release record is not an
//! admin command, and neither is a registry version).

use alloc::string::String;
use core::fmt::Write;

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::ids::from_hex;

/// SHA-256.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let h = Sha256::digest(data);
    <[u8; 32]>::try_from(h.as_slice()).unwrap_or([0u8; 32])
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The admin user's Ed25519 public key. `setup` pins it on the first install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdminPub([u8; 32]);

impl AdminPub {
    /// The checked constructor: 64 lowercase hex characters naming a valid key.
    pub fn parse(s: &[u8]) -> Option<AdminPub> {
        let bytes = from_hex::<32>(s)?;
        VerifyingKey::from_bytes(&bytes).ok()?;
        Some(AdminPub(bytes))
    }
    /// From raw bytes, if they name a valid key.
    pub fn from_bytes(bytes: [u8; 32]) -> Option<AdminPub> {
        VerifyingKey::from_bytes(&bytes)
            .ok()
            .map(|_| AdminPub(bytes))
    }
    /// The key's bytes.
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
    /// What a person compares between the admin user and `setup`: the first
    /// eight bytes of the key's SHA-256, in hex.
    pub fn fingerprint(&self) -> String {
        hex(sha256(&self.0).get(..8).unwrap_or(&[]))
    }
    /// Whether `sig` is this key's signature over `context ‖ body`. Strict:
    /// no malleable or small-order signatures.
    pub fn verify(&self, context: &[u8], body: &[u8], sig: &[u8; 64]) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        let mut msg = alloc::vec::Vec::with_capacity(context.len().saturating_add(body.len()));
        msg.extend_from_slice(context);
        msg.extend_from_slice(body);
        key.verify_strict(&msg, &Signature::from_bytes(sig)).is_ok()
    }
}

/// A heddle release version: three dot-separated numbers of 1 to 4 digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version([u16; 3]);

impl Version {
    /// The checked constructor.
    pub fn parse(s: &[u8]) -> Option<Version> {
        let mut out = [0u16; 3];
        let mut parts = s.split(|b| *b == b'.');
        for slot in out.iter_mut() {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 4 || !p.iter().all(u8::is_ascii_digit) {
                return None;
            }
            *slot = core::str::from_utf8(p).ok()?.parse().ok()?;
        }
        if parts.next().is_some() {
            None
        } else {
            Some(Version(out))
        }
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c] = self.0;
        write!(f, "{a}.{b}.{c}")
    }
}

/// The context tag of a release record.
pub const RECORD_CONTEXT: &[u8] = b"fabric-release-v1\n";
const RECORD_HEADER: &[u8] = b"fabric-release-record 1";

/// The release record (v01_architecture.md §5.10), signed by the admin user.
/// It is single-use: `setup` refuses a serial it has already used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseRecord {
    /// The release.
    pub version: Version,
    /// The SHA-256 of the binary.
    pub sha256: [u8; 32],
    /// Rises with every record the admin user signs.
    pub serial: u64,
}

impl ReleaseRecord {
    /// The signed body.
    pub fn body(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "version {}\nsha256 {}\nserial {}\n",
            self.version,
            hex(&self.sha256),
            self.serial
        );
        s
    }

    /// The whole file: a header, the body, and the signature.
    pub fn file(&self, sig: &[u8; 64]) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "fabric-release-record 1\n{}signature {}\n",
            self.body(),
            hex(sig)
        );
        s
    }

    /// The record in `text`, if it parses and `key` signed it.
    pub fn verify(text: &[u8], key: &AdminPub) -> Option<ReleaseRecord> {
        let mut lines = text.split(|b| *b == b'\n');
        if lines.next()? != RECORD_HEADER {
            return None;
        }
        let field = |line: Option<&[u8]>, name: &[u8]| -> Option<alloc::vec::Vec<u8>> {
            let l = line?;
            let rest = l.strip_prefix(name)?.strip_prefix(b" ")?;
            Some(rest.to_vec())
        };
        let version = Version::parse(&field(lines.next(), b"version")?)?;
        let sha256 = from_hex::<32>(&field(lines.next(), b"sha256")?)?;
        let serial_text = field(lines.next(), b"serial")?;
        if serial_text.first() == Some(&b'0') && serial_text.len() > 1 {
            return None;
        }
        let serial = core::str::from_utf8(&serial_text)
            .ok()?
            .parse::<u64>()
            .ok()?;
        let sig = from_hex::<64>(&field(lines.next(), b"signature")?)?;
        if !lines.all(|l| l.is_empty()) {
            return None;
        }
        let record = ReleaseRecord {
            version,
            sha256,
            serial,
        };
        key.verify(RECORD_CONTEXT, record.body().as_bytes(), &sig)
            .then_some(record)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::string_slice)]
mod tests {
    use super::*;
    use alloc::format;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(seed: u8) -> (SigningKey, AdminPub) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = AdminPub::from_bytes(sk.verifying_key().to_bytes()).expect("a valid key");
        (sk, pk)
    }

    fn signed(sk: &SigningKey, r: &ReleaseRecord) -> String {
        let mut msg = RECORD_CONTEXT.to_vec();
        msg.extend_from_slice(r.body().as_bytes());
        r.file(&sk.sign(&msg).to_bytes())
    }

    fn record() -> ReleaseRecord {
        ReleaseRecord {
            version: Version::parse(b"0.1.0").expect("v"),
            sha256: sha256(b"a binary"),
            serial: 7,
        }
    }

    #[test]
    fn a_record_verifies_under_its_key_and_no_other() {
        let (sk, pk) = key(1);
        let (_, other) = key(2);
        let text = signed(&sk, &record());
        assert_eq!(ReleaseRecord::verify(text.as_bytes(), &pk), Some(record()));
        assert_eq!(ReleaseRecord::verify(text.as_bytes(), &other), None);
    }

    #[test]
    fn any_change_to_a_signed_record_is_refused() {
        let (sk, pk) = key(1);
        let text = signed(&sk, &record());
        for (from, to) in [
            ("serial 7", "serial 8"),
            ("version 0.1.0", "version 0.1.1"),
            ("sha256 ", "sha256 0"),
        ] {
            let tampered = text.replacen(from, to, 1);
            assert_eq!(
                ReleaseRecord::verify(tampered.as_bytes(), &pk),
                None,
                "{from}"
            );
        }
        assert_eq!(
            ReleaseRecord::verify(format!("{text}extra\n").as_bytes(), &pk),
            None
        );
        assert_eq!(
            ReleaseRecord::verify(text.replacen("serial 7", "serial 07", 1).as_bytes(), &pk),
            None
        );
    }

    #[test]
    fn a_signature_for_another_context_does_not_verify() {
        let (sk, pk) = key(3);
        let body = record().body();
        let sig = sk.sign(body.as_bytes()).to_bytes();
        assert!(!pk.verify(RECORD_CONTEXT, body.as_bytes(), &sig));
        assert!(pk.verify(b"", body.as_bytes(), &sig));
    }

    #[test]
    fn keys_versions_and_hashes() {
        let (_, pk) = key(4);
        assert_eq!(AdminPub::parse(hex(pk.bytes()).as_bytes()), Some(pk));
        assert_eq!(pk.fingerprint().len(), 16);
        assert!(AdminPub::parse(b"00").is_none());
        assert_eq!(
            Version::parse(b"12.0.3").map(|v| alloc::string::ToString::to_string(&v)),
            Some("12.0.3".into())
        );
        for bad in [&b"1.2"[..], b"1.2.3.4", b"1..3", b"1.2.x", b"12345.0.0"] {
            assert_eq!(Version::parse(bad), None);
        }
        assert_eq!(hex(&sha256(b"abc"))[..16], *"ba7816bf8f01cfea");
    }
}
