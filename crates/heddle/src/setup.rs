//! `sudo <staging>/heddle setup`: install heddle from a verified release
//! (v01_architecture.md §5.10). It refuses unless all of these hold:
//!
//! 1. the staged binary, its release record and their folder belong to the
//!    admin user (`fabric-admin`) and nobody else can write them;
//! 2. the record's signature verifies against the pinned admin key (on the
//!    first install, the key given with `--admin-key`, whose fingerprint it
//!    prints for a person to compare);
//! 3. the binary's SHA-256 and version are the record's;
//! 4. the record's serial is higher than any `setup` has used, so a record
//!    works once and a repair always starts in the admin user.
//!
//! Then it writes, in this order: the binary under `bin/<version>/`; the
//! policy in `etc/`, the used serial first; and last, atomically, the managed
//! drop-in with the hook lines. The PHI lines carry `onFailure: "block"` and
//! `requiredMinimumVersion` is 2.1.295, the first build that honours it, so
//! the brake fails closed (B9). `--root` installs under a scratch folder, for
//! tests; a real install is `--root /` and needs root.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use fabric_core::sig::{self, AdminPub, ReleaseRecord, Version};
use serde_json::{Map, Value, json};

const MIN_CLAUDE_CODE: &str = "2.1.295";
const DROP_IN: &str = "Library/Application Support/ClaudeCode/managed-settings.d/50-fabric.json";
const FABRIC: &str = "usr/local/fabric";

type Res<T> = Result<T, String>;

/// What `setup` was asked to do.
#[derive(Debug)]
pub struct Opts {
    /// The filesystem root to install under: `/` for a real install.
    pub root: PathBuf,
    /// The admin public key, required on the first install.
    pub admin_key: Option<String>,
    /// Repos to enrol.
    pub enroll: Vec<PathBuf>,
    /// Pin Claude Code's side features off (decision new-19, default on).
    pub pins: bool,
    /// The user who must own the staged files.
    pub owner_uid: u32,
}

/// The entry point, from the command line.
pub fn run(args: &[String]) -> ExitCode {
    match parse(args).and_then(|o| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        install(&exe, &o)
    }) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("heddle setup: refused: {e}");
            ExitCode::from(1)
        }
    }
}

fn uid_of(user: &str) -> Res<u32> {
    let out = Command::new("/usr/bin/id")
        .args(["-u", user])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("no user named {user}"));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| "id printed no uid".to_string())
}

fn parse(args: &[String]) -> Res<Opts> {
    let mut root = PathBuf::from("/");
    let mut admin_key = None;
    let mut enroll = Vec::new();
    let mut pins = true;
    let mut owner = None;
    let mut it = args.iter().skip(2);
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--root" => root = PathBuf::from(value()?),
            "--admin-key" => admin_key = Some(value()?),
            "--enroll" => enroll.push(PathBuf::from(value()?)),
            "--pins" => {
                pins = match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("--pins is on or off".to_string()),
                }
            }
            "--owner" => owner = Some(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let real = root == Path::new("/");
    if real {
        if owner.is_some() {
            return Err(
                "--owner is for a test root only; a real install takes files from fabric-admin"
                    .to_string(),
            );
        }
        let me = Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .map_err(|e| e.to_string())?;
        if String::from_utf8_lossy(&me.stdout).trim() != "0" {
            return Err("a real install needs root: run it with sudo".to_string());
        }
    }
    let owner_uid = uid_of(owner.as_deref().unwrap_or("fabric-admin"))?;
    Ok(Opts {
        root,
        admin_key,
        enroll,
        pins,
        owner_uid,
    })
}

/// Staged files must belong to the admin user and be writable by nobody else.
fn check_staged(path: &Path, owner_uid: u32) -> Res<()> {
    let m = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if m.uid() != owner_uid {
        return Err(format!("{} is not owned by the admin user", path.display()));
    }
    if m.mode() & 0o022 != 0 {
        return Err(format!("{} is writable by others", path.display()));
    }
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Res<()> {
    let dir = path.parent().ok_or("no parent folder")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".heddle-setup.{}.tmp", std::process::id()));
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
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())
}

/// The managed drop-in. Hook lines only for events heddle handles; every PHI
/// line blocks when heddle fails, hangs or is missing (day 0 verified each).
pub fn drop_in(bin: &str, pins: bool) -> Value {
    let line = |ev: &str, block: bool| {
        let mut h = json!({"type": "command", "command": bin, "args": ["hook", "claude-code", ev], "timeout": 10});
        if block && let Some(o) = h.as_object_mut() {
            o.insert("onFailure".into(), json!("block"));
        }
        h
    };
    let mut hooks = Map::new();
    hooks.insert(
        "SessionStart".into(),
        json!([{"hooks": [line("SessionStart", false)]}]),
    );
    for ev in [
        "UserPromptSubmit",
        "UserPromptExpansion",
        "PostToolBatch",
        "PreCompact",
    ] {
        hooks.insert(ev.into(), json!([{"hooks": [line(ev, true)]}]));
    }
    for ev in ["PreToolUse", "PostToolUse"] {
        hooks.insert(
            ev.into(),
            json!([{"matcher": "*", "hooks": [line(ev, true)]}]),
        );
    }
    let mut env = Map::new();
    env.insert("DISABLE_AUTOUPDATER".into(), json!("1"));
    let mut v = Map::new();
    v.insert("hooks".into(), Value::Object(hooks));
    v.insert("requiredMinimumVersion".into(), json!(MIN_CLAUDE_CODE));
    if pins {
        // So a replaced result is not exported, and no prompt arrives before the hook.
        for k in [
            "DISABLE_TELEMETRY",
            "DISABLE_ERROR_REPORTING",
            "DISABLE_FEEDBACK_COMMAND",
        ] {
            env.insert(k.into(), json!("1"));
        }
        env.insert(
            "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION".into(),
            json!("false"),
        );
        v.insert("remoteControlAtStartup".into(), json!(false));
        v.insert("crossSessionInbound".into(), json!("refuse"));
    }
    v.insert("env".into(), Value::Object(env));
    Value::Object(v)
}

/// The install itself, given the staged binary.
pub fn install(exe: &Path, o: &Opts) -> Res<String> {
    let staging = exe.parent().ok_or("the binary has no folder")?;
    let record_path = staging.join("release-record");
    for p in [staging, exe, record_path.as_path()] {
        check_staged(p, o.owner_uid)?;
    }
    let fabric = o.root.join(FABRIC);
    let etc = fabric.join("etc");

    // The key: pinned, or given on the first install.
    let pinned = fs::read(etc.join("admin.pub"))
        .ok()
        .and_then(|b| AdminPub::parse(b.trim_ascii()));
    let given = match &o.admin_key {
        Some(k) => {
            Some(AdminPub::parse(k.trim().as_bytes()).ok_or("--admin-key is not a valid key")?)
        }
        None => None,
    };
    let key = match (pinned, given) {
        (Some(p), Some(g)) if p != g => {
            return Err("--admin-key differs from the pinned admin key".to_string());
        }
        (Some(p), _) => p,
        (None, Some(g)) => g,
        (None, None) => return Err("the first install needs --admin-key".to_string()),
    };

    let text = fs::read(&record_path).map_err(|e| e.to_string())?;
    let record = ReleaseRecord::verify(&text, &key)
        .ok_or("the release record does not verify against the admin key")?;
    let bytes = fs::read(exe).map_err(|e| e.to_string())?;
    if sig::sha256(&bytes) != record.sha256 {
        return Err("the binary's SHA-256 is not the record's".to_string());
    }
    let own =
        Version::parse(env!("CARGO_PKG_VERSION").as_bytes()).ok_or("this build has no version")?;
    if record.version != own {
        return Err(format!(
            "the record is for {}, this binary is {own}",
            record.version
        ));
    }
    let last: u64 = fs::read_to_string(etc.join("last-serial"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if record.serial <= last {
        return Err(format!(
            "this record (serial {}) was already used; make a fresh one in the admin user",
            record.serial
        ));
    }

    let mut enrolled: Vec<String> = fs::read_to_string(etc.join("enrolled"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with('/'))
        .map(String::from)
        .collect();
    for dir in &o.enroll {
        let real = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if !real.is_dir() {
            return Err(format!("{} is not a folder", real.display()));
        }
        enrolled.push(
            real.to_str()
                .ok_or("an enrolled path is not UTF-8")?
                .to_string(),
        );
    }
    enrolled.sort();
    enrolled.dedup();

    // 1. The binary, in its own versioned folder: an upgrade never replaces a running file.
    let bin = fabric
        .join("bin")
        .join(record.version.to_string())
        .join("heddle");
    write_atomic(&bin, &bytes, 0o755)?;
    // 2. The policy, the used serial first.
    write_atomic(
        &etc.join("last-serial"),
        format!("{}\n", record.serial).as_bytes(),
        0o644,
    )?;
    write_atomic(
        &etc.join("admin.pub"),
        format!("{}\n", sig::hex(key.bytes())).as_bytes(),
        0o644,
    )?;
    write_atomic(
        &etc.join("enrolled"),
        format!("{}\n", enrolled.join("\n")).as_bytes(),
        0o644,
    )?;
    write_atomic(&etc.join("pool"), b"open\n", 0o644)?;
    // 3. Last, the hook lines, validated JSON, renamed into place.
    let bin_s = bin.to_str().ok_or("the install path is not UTF-8")?;
    let json = serde_json::to_vec_pretty(&drop_in(bin_s, o.pins)).map_err(|e| e.to_string())?;
    serde_json::from_slice::<Value>(&json).map_err(|e| e.to_string())?;
    let drop_in_path = o.root.join(DROP_IN);
    write_atomic(&drop_in_path, &json, 0o644)?;

    Ok(format!(
        "heddle {} installed (SHA-256 {}, release record serial {}).\n\
         Admin key fingerprint: {} — compare it with what heddle-admin pubkey shows.\n\
         Enrolled: {}\n\
         Hook lines: {}\n\
         Side features pinned off: {}\n\
         To uninstall, which also removes the PHI brake: sudo rm \"{}\"",
        record.version,
        sig::hex(&record.sha256),
        record.serial,
        key.fingerprint(),
        if enrolled.is_empty() {
            "nothing".to_string()
        } else {
            enrolled.join(", ")
        },
        drop_in_path.display(),
        if o.pins {
            "yes (Remote Control, cross-session messages, telemetry, error reporting, feedback, prompt suggestions)"
        } else {
            "no"
        },
        drop_in_path.display(),
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use fabric_core::sig::RECORD_CONTEXT;
    use std::os::unix::fs::PermissionsExt;

    struct Scratch {
        dir: PathBuf,
        exe: PathBuf,
        sk: SigningKey,
    }

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("heddle-setup-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let staging = dir.join("staging");
            fs::create_dir_all(&staging).unwrap();
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
            let exe = staging.join("heddle");
            fs::write(&exe, b"a stand-in binary").unwrap();
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
            Scratch {
                dir,
                exe,
                sk: SigningKey::from_bytes(&[7; 32]),
            }
        }
        fn record(&self, serial: u64, hash: [u8; 32]) {
            let version = Version::parse(env!("CARGO_PKG_VERSION").as_bytes()).unwrap();
            let r = ReleaseRecord {
                version,
                sha256: hash,
                serial,
            };
            let mut msg = RECORD_CONTEXT.to_vec();
            msg.extend_from_slice(r.body().as_bytes());
            let path = self.exe.parent().unwrap().join("release-record");
            fs::write(&path, r.file(&self.sk.sign(&msg).to_bytes())).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        fn opts(&self, admin_key: Option<String>) -> Opts {
            let me = fs::metadata(&self.exe).unwrap().uid();
            Opts {
                root: self.dir.join("root"),
                admin_key,
                enroll: vec![self.dir.clone()],
                pins: true,
                owner_uid: me,
            }
        }
        fn key_hex(&self) -> String {
            sig::hex(&self.sk.verifying_key().to_bytes())
        }
    }

    #[test]
    fn a_verified_record_installs_once_and_a_reused_one_is_refused() {
        let s = Scratch::new("once");
        s.record(1, sig::sha256(b"a stand-in binary"));
        install(&s.exe, &s.opts(Some(s.key_hex()))).expect("installs");
        let v = env!("CARGO_PKG_VERSION");
        let root = s.dir.join("root");
        assert!(root.join(format!("{FABRIC}/bin/{v}/heddle")).is_file());
        let drop_in: Value =
            serde_json::from_slice(&fs::read(root.join(DROP_IN)).unwrap()).unwrap();
        assert_eq!(
            drop_in["hooks"]["PreToolUse"][0]["hooks"][0]["onFailure"],
            "block"
        );
        assert_eq!(drop_in["requiredMinimumVersion"], MIN_CLAUDE_CODE);
        // The same record again: refused, even with the key pinned now.
        let again = install(&s.exe, &s.opts(None));
        assert!(again.unwrap_err().contains("already used"));
        // A fresh record from the admin user: accepted.
        s.record(2, sig::sha256(b"a stand-in binary"));
        install(&s.exe, &s.opts(None)).expect("a fresh record installs");
        let _ = fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn an_unverified_binary_is_refused() {
        let s = Scratch::new("refusals");
        s.record(1, sig::sha256(b"some other binary"));
        assert!(
            install(&s.exe, &s.opts(Some(s.key_hex())))
                .unwrap_err()
                .contains("SHA-256")
        );
        s.record(1, sig::sha256(b"a stand-in binary"));
        let wrong_key = sig::hex(&SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes());
        assert!(
            install(&s.exe, &s.opts(Some(wrong_key)))
                .unwrap_err()
                .contains("does not verify")
        );
        assert!(
            install(&s.exe, &s.opts(None))
                .unwrap_err()
                .contains("--admin-key")
        );
        fs::set_permissions(&s.exe, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(
            install(&s.exe, &s.opts(Some(s.key_hex())))
                .unwrap_err()
                .contains("writable by others")
        );
        let mut not_mine = s.opts(Some(s.key_hex()));
        not_mine.owner_uid = not_mine.owner_uid.wrapping_add(1);
        assert!(
            install(&s.exe, &not_mine)
                .unwrap_err()
                .contains("not owned")
        );
        let _ = fs::remove_dir_all(&s.dir);
    }

    #[test]
    fn the_pins_can_be_left_off() {
        let on = drop_in("/x/heddle", true);
        let off = drop_in("/x/heddle", false);
        assert_eq!(on["crossSessionInbound"], "refuse");
        assert!(off.get("crossSessionInbound").is_none());
        assert_eq!(off["env"]["DISABLE_AUTOUPDATER"], "1");
        assert!(
            off["hooks"]["SessionStart"][0]["hooks"][0]
                .get("onFailure")
                .is_none()
        );
    }
}
