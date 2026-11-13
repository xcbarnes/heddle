//! The machine folder (L0): `~/Library/Application Support/fabric/machine`.
//!
//! One file per conversation in `quarantine/` holds that conversation's PHI
//! label, so a thread that fired is braked wherever it is resumed, forked or
//! compacted. Every write follows one rule: take the kernel-held lock, read,
//! step, write a temporary file, fsync, rename, fsync the folder. A file that
//! is missing means "no entry"; one that cannot be read, or has a format this
//! heddle does not know, is read as braked (V5), never as Clean.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use fabric_core::hook::{HookKind, rule_name};
use fabric_core::ids::{ConvId, IncidentId};
use fabric_core::phi::{self, Cause, Phi, RuleId};

const FORMAT: &[u8] = b"fabric-conv 1";

/// A conversation's entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvState {
    /// The conversation's label.
    pub phi: Phi,
    /// A subagent's hook has run since the last prompt, so the next prompt is
    /// not counted as typed by a person (the conservative rule of M13).
    pub sub_since_prompt: bool,
}

impl ConvState {
    const CLEAN: ConvState = ConvState { phi: Phi::Clean, sub_since_prompt: false };

    fn braked(incident: IncidentId) -> ConvState {
        ConvState {
            phi: Phi::Exposed { incident, cause: Cause::FoundLater, hits: 1, acked: false },
            sub_since_prompt: true,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(FORMAT);
        out.extend_from_slice(b"\nphi ");
        out.extend_from_slice(phi::encode_phi(&self.phi).as_bytes());
        out.extend_from_slice(if self.sub_since_prompt { b"\nsub 1\n" } else { b"\nsub 0\n" });
        out
    }

    fn decode(bytes: &[u8]) -> Option<ConvState> {
        let mut lines = bytes.split(|b| *b == b'\n');
        if lines.next()? != FORMAT {
            return None;
        }
        let phi = phi::decode_phi(lines.next()?.strip_prefix(b"phi ")?)?;
        let sub = match lines.next()? {
            b"sub 0" => false,
            b"sub 1" => true,
            _ => return None,
        };
        let rest_empty = lines.all(|l| l.is_empty());
        rest_empty.then_some(ConvState { phi, sub_since_prompt: sub })
    }
}

/// The machine folder.
#[derive(Debug)]
pub struct Machine {
    dir: PathBuf,
}

impl Machine {
    /// The everyday user's folder, created 0700 if missing.
    pub fn open() -> io::Result<Machine> {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        Machine::at(PathBuf::from(home).join("Library/Application Support/fabric/machine"))
    }

    /// A folder at `dir`, created 0700 if missing.
    pub fn at(dir: PathBuf) -> io::Result<Machine> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir.join("quarantine"))?;
        Ok(Machine { dir })
    }

    /// The machine folder's one lock, held until the file is dropped. The
    /// kernel releases it if heddle dies.
    pub fn lock(&self) -> io::Result<File> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.dir.join(".lock"))?;
        f.lock()?;
        Ok(f)
    }

    /// Where a conversation's entry lives.
    pub fn conv_path(&self, c: &ConvId) -> PathBuf {
        self.dir.join("quarantine").join(c.to_string())
    }

    /// The conversation's entry, and whether it must be written back. Missing
    /// is Clean. Unreadable or unknown is braked under `fresh`, and written
    /// back so the brake holds at the next hook too.
    pub fn read_conv(&self, c: &ConvId, fresh: IncidentId) -> (ConvState, bool) {
        match fs::read(self.conv_path(c)) {
            Ok(bytes) => match ConvState::decode(&bytes) {
                Some(s) => (s, false),
                None => (ConvState::braked(fresh), true),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (ConvState::CLEAN, false),
            Err(_) => (ConvState::braked(fresh), true),
        }
    }

    /// Write the entry: temporary file, fsync, rename, fsync the folder.
    pub fn write_conv(&self, c: &ConvId, s: &ConvState) -> io::Result<()> {
        let dir = self.dir.join("quarantine");
        let name = c.to_string();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp);
        {
            let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(&s.encode())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join(name))?;
        File::open(&dir)?.sync_all()
    }

    /// Count a hit that does not block, by rule id, for tuning. Never the text,
    /// never the conversation. Best effort: counting is not a safety property.
    pub fn count(&self, kind: HookKind, rule: RuleId) {
        let line = format!("{} {}\n", kind.name(), rule_name(rule));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).mode(0o600).open(self.dir.join("counts")) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_round_trips_and_anything_else_is_none() {
        let inc = IncidentId::parse(b"inc-aaaaaa").expect("id");
        for s in [
            ConvState::CLEAN,
            ConvState { phi: Phi::Held, sub_since_prompt: true },
            ConvState::braked(inc),
        ] {
            assert_eq!(ConvState::decode(&s.encode()), Some(s));
        }
        for bad in [&b""[..], b"fabric-conv 2\nphi clean\nsub 0\n", b"fabric-conv 1\nphi clean\n", b"fabric-conv 1\nphi clean\nsub 2\n"] {
            assert_eq!(ConvState::decode(bad), None);
        }
    }

    #[test]
    fn writes_land_whole_and_read_back() {
        let dir = std::env::temp_dir().join(format!("heddle-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let m = Machine::at(dir.clone()).expect("folder");
        let c = ConvId::parse(b"00893aaf-19fa-41d2-8238-13269b9b3ca0").expect("conv");
        let fresh = IncidentId::parse(b"inc-bbbbbb").expect("id");
        assert_eq!(m.read_conv(&c, fresh), (ConvState::CLEAN, false));
        let _lock = m.lock().expect("lock");
        let held = ConvState { phi: Phi::Held, sub_since_prompt: false };
        m.write_conv(&c, &held).expect("write");
        assert_eq!(m.read_conv(&c, fresh), (held, false));
        let _ = fs::remove_dir_all(dir);
    }
}
