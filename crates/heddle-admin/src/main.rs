//! heddle-admin: the admin user's commands.
//!
//! It runs only as `fabric-admin`, the person-only macOS user that holds the
//! admin key, so that key never sits where an agent can read it. v0.1 starts
//! with the install chain (v01_architecture.md §5.10):
//!
//! - `keygen` makes the admin key, once;
//! - `pubkey` prints the public key and its fingerprint;
//! - `release-record <tag>` downloads a CI release with the admin user's own
//!   GitHub login, verifies its build attestation and its checksum, stages it
//!   where others can read but not write, and signs a single-use record that
//!   `sudo heddle setup` checks before it installs anything.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ed25519_dalek::{Signer, SigningKey};
use fabric_core::sig::{self, AdminPub, RECORD_CONTEXT, ReleaseRecord, Version};

const REPO: &str = "xcbarnes/heddle";
const ASSET: &str = "heddle-aarch64-apple-darwin";
const STAGING: &str = "/Users/Shared/fabric-staging";

type Res<T> = Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let r = match args.get(1).map(String::as_str) {
        Some("keygen") => keygen(),
        Some("pubkey") => pubkey(),
        Some("release-record") => release_record(args.get(2).map(String::as_str)),
        _ => Err("usage: heddle-admin keygen | pubkey | release-record <tag>".to_string()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("heddle-admin: {e}");
            ExitCode::from(1)
        }
    }
}

fn admin_dir() -> Res<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".fabric-admin"))
}

fn random32() -> Res<[u8; 32]> {
    let mut b = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("reading /dev/urandom: {e}"))?;
    Ok(b)
}

/// Write `data` to `path` atomically, with `mode`.
fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Res<()> {
    let dir = path.parent().ok_or("no parent folder")?;
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(data)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn signing_key() -> Res<SigningKey> {
    let path = admin_dir()?.join("admin.key");
    let text = fs::read(&path)
        .map_err(|e| format!("{}: {e} (run heddle-admin keygen first)", path.display()))?;
    let secret =
        parse_hex32(text.trim_ascii()).ok_or("the admin key file is not 64 hex characters")?;
    Ok(SigningKey::from_bytes(&secret))
}

fn parse_hex32(s: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let mut it = s.iter();
    for slot in out.iter_mut() {
        let hi = char::from(*it.next()?).to_digit(16)?;
        let lo = char::from(*it.next()?).to_digit(16)?;
        *slot = u8::try_from(hi.checked_mul(16)?.checked_add(lo)?).ok()?;
    }
    if it.next().is_some() { None } else { Some(out) }
}

fn public(sk: &SigningKey) -> Res<AdminPub> {
    AdminPub::from_bytes(sk.verifying_key().to_bytes())
        .ok_or_else(|| "the admin key is not valid".to_string())
}

fn keygen() -> Res<()> {
    let dir = admin_dir()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    let path = dir.join("admin.key");
    if path.exists() {
        return Err(format!(
            "{} already exists; a second key would orphan the first",
            path.display()
        ));
    }
    let sk = SigningKey::from_bytes(&random32()?);
    write_atomic(
        &path,
        format!("{}\n", sig::hex(sk.as_bytes())).as_bytes(),
        0o600,
    )?;
    let pk = public(&sk)?;
    println!(
        "admin public key  {}\nfingerprint       {}",
        sig::hex(pk.bytes()),
        pk.fingerprint()
    );
    Ok(())
}

fn pubkey() -> Res<()> {
    let pk = public(&signing_key()?)?;
    println!(
        "admin public key  {}\nfingerprint       {}",
        sig::hex(pk.bytes()),
        pk.fingerprint()
    );
    Ok(())
}

fn gh(args: &[&str]) -> Res<()> {
    let status = Command::new("gh")
        .args(args)
        .status()
        .map_err(|e| format!("running gh: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("gh {} failed", args.first().unwrap_or(&"")))
    }
}

fn release_record(tag: Option<&str>) -> Res<()> {
    let tag = tag.ok_or("usage: heddle-admin release-record <tag>")?;
    let version = tag
        .strip_prefix('v')
        .and_then(|v| Version::parse(v.as_bytes()))
        .ok_or("a tag is v<major>.<minor>.<patch>")?;
    let sk = signing_key()?;
    let staging = Path::new(STAGING).join(version.to_string());
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&staging)
        .map_err(|e| e.to_string())?;
    let staging_s = staging.to_str().ok_or("the staging path is not UTF-8")?;

    gh(&[
        "release",
        "download",
        tag,
        "-R",
        REPO,
        "-p",
        ASSET,
        "-p",
        "SHA256SUMS",
        "-D",
        staging_s,
        "--clobber",
    ])?;
    let asset = staging.join(ASSET);
    let asset_s = asset.to_str().ok_or("the asset path is not UTF-8")?;
    gh(&["attestation", "verify", asset_s, "-R", REPO])?;

    let bytes = fs::read(&asset).map_err(|e| e.to_string())?;
    let hash = sig::sha256(&bytes);
    let sums = fs::read_to_string(staging.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let listed = sums
        .lines()
        .find_map(|l| l.strip_suffix(ASSET).map(str::trim))
        .ok_or("SHA256SUMS does not list the binary")?;
    if listed != sig::hex(&hash) {
        return Err("the binary's SHA-256 does not match SHA256SUMS".to_string());
    }

    // The serial rises before the record exists, so a crash never reuses one.
    let serial_path = admin_dir()?.join("serial");
    let last: u64 = fs::read_to_string(&serial_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let serial = last.checked_add(1).ok_or("the serial is exhausted")?;
    write_atomic(&serial_path, format!("{serial}\n").as_bytes(), 0o600)?;

    let bin = staging.join("heddle");
    fs::rename(&asset, &bin).map_err(|e| e.to_string())?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let record = ReleaseRecord {
        version,
        sha256: hash,
        serial,
    };
    let mut msg = RECORD_CONTEXT.to_vec();
    msg.extend_from_slice(record.body().as_bytes());
    let file = record.file(&sk.sign(&msg).to_bytes());
    write_atomic(&staging.join("release-record"), file.as_bytes(), 0o644)?;

    let pk = public(&sk)?;
    println!(
        "Release record for {version}: SHA-256 {}, serial {serial}.",
        sig::hex(&hash)
    );
    println!("Now, as yourself in the everyday user's terminal, run:");
    println!("  sudo {} setup --enroll <repo>", bin.display());
    println!(
        "On the very first install, add: --admin-key {}",
        sig::hex(pk.bytes())
    );
    println!(
        "and check that setup prints the fingerprint {}.",
        pk.fingerprint()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_file_format_round_trips() {
        let b = [0xa5u8; 32];
        assert_eq!(parse_hex32(sig::hex(&b).as_bytes()), Some(b));
        assert_eq!(parse_hex32(b"00"), None);
    }
}
