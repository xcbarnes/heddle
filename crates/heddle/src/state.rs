//! The state folder (L0), named by the hook line's `--state` argument; setup
//! makes it `~/Library/Application Support/fabric/machine`. heddle reads no
//! environment variable to find it, and a folder that does not exist is a
//! fault (R-00), never an empty store.
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

/// A conversation's entry. The default is a clean, never-seen conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvState {
    /// The conversation's label.
    pub phi: Phi,
    /// A subagent's hook has run since the last prompt, so the next prompt is
    /// not counted as typed by a person (the conservative rule of M13).
    pub sub_since_prompt: bool,
    /// Patient-shaped tool inputs denied in this conversation (decision 3(iii)).
    pub strikes: u8,
    /// Results withheld since the last prompt (decision 3(ii)).
    pub withheld: u8,
}

impl Default for ConvState {
    fn default() -> ConvState {
        ConvState::CLEAN
    }
}

impl ConvState {
    const CLEAN: ConvState = ConvState {
        phi: Phi::Clean,
        sub_since_prompt: false,
        strikes: 0,
        withheld: 0,
    };

    fn braked(incident: IncidentId) -> ConvState {
        ConvState {
            phi: Phi::Exposed {
                incident,
                cause: Cause::FoundLater,
                hits: 1,
                acked: false,
            },
            sub_since_prompt: true,
            strikes: 0,
            withheld: 0,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(FORMAT);
        out.extend_from_slice(b"\nphi ");
        out.extend_from_slice(phi::encode_phi(&self.phi).as_bytes());
        out.extend_from_slice(if self.sub_since_prompt {
            b"\nsub 1"
        } else {
            b"\nsub 0"
        });
        out.extend_from_slice(
            format!("\nstrikes {}\nwithheld {}\n", self.strikes, self.withheld).as_bytes(),
        );
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
        let mut count = |key: &[u8]| -> Option<u8> {
            let digits = lines.next()?.strip_prefix(key)?;
            if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            core::str::from_utf8(digits).ok()?.parse().ok()
        };
        let strikes = count(b"strikes ")?;
        let withheld = count(b"withheld ")?;
        let rest_empty = lines.all(|l| l.is_empty());
        rest_empty.then_some(ConvState {
            phi,
            sub_since_prompt: sub,
            strikes,
            withheld,
        })
    }
}

/// The machine folder.
#[derive(Debug)]
pub struct Machine {
    dir: PathBuf,
    /// The second label copy's folder, `<git common dir>/fabric/labels`, so
    /// one accidental `rm` does not clear a brake.
    second: Option<PathBuf>,
}

/// One copy of a label, as read.
enum Copy {
    Missing,
    Unreadable,
    Entry(ConvState),
}

fn read_copy(path: &std::path::Path) -> Copy {
    match fs::read(path) {
        Ok(bytes) => ConvState::decode(&bytes).map_or(Copy::Unreadable, Copy::Entry),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Copy::Missing,
        Err(_) => Copy::Unreadable,
    }
}

/// How far a label stops the conversation, for choosing between two copies.
fn rank(p: &Phi) -> u8 {
    match p {
        Phi::Clean => 0,
        Phi::Held => 1,
        Phi::Exposed { .. } => 2,
        Phi::Sealed { .. } => 3,
    }
}

impl Machine {
    /// The state folder at `dir`, which must exist. Its `quarantine/` is
    /// created 0700 if missing: a missing label file is "no entry" anyway.
    pub fn open(dir: PathBuf) -> io::Result<Machine> {
        if !fs::metadata(&dir)?.is_dir() {
            return Err(io::Error::other("the state folder is not a folder"));
        }
        match fs::DirBuilder::new()
            .mode(0o700)
            .create(dir.join("quarantine"))
        {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            Ok(()) | Err(_) => {}
        }
        Ok(Machine { dir, second: None })
    }

    /// Keep a second copy of every label in `dir` too.
    pub fn with_second(self, dir: PathBuf) -> Machine {
        Machine {
            second: Some(dir),
            ..self
        }
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
    /// back so the brake holds at the next hook too. With a second copy, the
    /// more stopping of the two wins, either one unreadable is braked, and a
    /// copy that is missing or behind is written back.
    pub fn read_conv(&self, c: &ConvId, fresh: IncidentId) -> (ConvState, bool) {
        let first = read_copy(&self.conv_path(c));
        let second = match &self.second {
            Some(d) => read_copy(&d.join(c.to_string())),
            None => Copy::Missing,
        };
        match (first, second) {
            (Copy::Unreadable, _) | (_, Copy::Unreadable) => (ConvState::braked(fresh), true),
            (Copy::Missing, Copy::Missing) => (ConvState::CLEAN, false),
            (Copy::Entry(a), Copy::Missing) => (a, self.second.is_some()),
            (Copy::Missing, Copy::Entry(b)) => (b, true),
            (Copy::Entry(a), Copy::Entry(b)) if rank(&b.phi) > rank(&a.phi) => (b, true),
            (Copy::Entry(a), Copy::Entry(b)) => (a, a != b),
        }
    }

    /// Write the entry: temporary file, fsync, rename, fsync the folder.
    pub fn write_conv(&self, c: &ConvId, s: &ConvState) -> io::Result<()> {
        let dir = self.dir.join("quarantine");
        let name = c.to_string();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp);
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&s.encode())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, dir.join(&name))?;
        File::open(&dir)?.sync_all()?;
        // The second copy, best effort: the first one is the one that must land.
        if let Some(second) = &self.second {
            let _ = fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(second);
            let tmp = second.join(format!(".{name}.{}.tmp", std::process::id()));
            if fs::write(&tmp, s.encode())
                .and_then(|()| fs::rename(&tmp, second.join(&name)))
                .is_err()
            {
                let _ = fs::remove_file(&tmp);
            }
        }
        Ok(())
    }

    /// Note where a conversation's transcript lives, once its label is
    /// raised, so `status` can show it and `uninstall` can refuse while it
    /// exists. A path, never content. Best effort.
    pub fn note_transcript(&self, c: &ConvId, transcript: &str) {
        let _ = fs::write(
            self.dir.join("quarantine").join(format!("{c}.transcript")),
            transcript,
        );
    }

    /// Note the newest message id of a conversation that was just paused. A
    /// fork keeps its parent's message ids, so finding this one in a fork's
    /// transcript means the fork carries the paused conversation. Best effort.
    pub fn note_flagged(&self, c: &ConvId, message_id: &str) {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join("quarantine").join(format!("{c}.flagged")))
        {
            let _ = f.write_all(format!("{message_id}\n").as_bytes());
        }
    }

    /// The noted message ids of every conversation that is still paused.
    pub fn flagged_ids(&self) -> Vec<String> {
        let q = self.dir.join("quarantine");
        self.paused()
            .into_iter()
            .filter_map(|(conv, _)| fs::read_to_string(q.join(format!("{conv}.flagged"))).ok())
            .flat_map(|t| t.lines().map(String::from).collect::<Vec<_>>())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// A fork waiting for its check: marked at its SessionStart, checked at
    /// each blocking hook until its transcript can be read.
    pub fn mark_fork(&self, c: &ConvId, pending: bool) {
        let path = self.dir.join("quarantine").join(format!("{c}.fork"));
        let _ = if pending {
            fs::write(path, b"")
        } else {
            fs::remove_file(path)
        };
    }

    /// Whether a fork check is pending for this conversation.
    pub fn fork_pending(&self, c: &ConvId) -> bool {
        self.dir
            .join("quarantine")
            .join(format!("{c}.fork"))
            .exists()
    }

    /// Every conversation whose label pauses it (Exposed or Sealed, or a file
    /// that cannot be read), with its transcript path if one was noted.
    pub fn paused(&self) -> Vec<(String, Option<PathBuf>)> {
        let dir = self.dir.join("quarantine");
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, Option<PathBuf>)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let conv = ConvId::parse(name.as_bytes())?;
                let paused = fs::read(e.path())
                    .ok()
                    .and_then(|b| ConvState::decode(&b))
                    .is_none_or(|st| matches!(st.phi, Phi::Exposed { .. } | Phi::Sealed { .. }));
                let transcript = fs::read_to_string(dir.join(format!("{conv}.transcript")))
                    .ok()
                    .map(PathBuf::from);
                paused.then_some((name, transcript))
            })
            .collect();
        out.sort();
        out
    }

    /// Log a hit that decided nothing, for tuning and for count mode: one
    /// line of Unix seconds, mode, event and rule id. Never the text, never
    /// the conversation. Best effort: counting is not a safety property.
    pub fn count(&self, mode: &str, kind: HookKind, rule: RuleId) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = format!("{secs} {mode} {} {}\n", kind.name(), rule_name(rule));
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join("counts"))
        {
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
            ConvState {
                phi: Phi::Held,
                sub_since_prompt: true,
                strikes: 1,
                withheld: 255,
            },
            ConvState::braked(inc),
        ] {
            assert_eq!(ConvState::decode(&s.encode()), Some(s));
        }
        for bad in [
            &b""[..],
            b"fabric-conv 2\nphi clean\nsub 0\nstrikes 0\nwithheld 0\n",
            b"fabric-conv 1\nphi clean\nsub 0\n",
            b"fabric-conv 1\nphi clean\nsub 2\nstrikes 0\nwithheld 0\n",
            b"fabric-conv 1\nphi clean\nsub 0\nstrikes +1\nwithheld 0\n",
            b"fabric-conv 1\nphi clean\nsub 0\nstrikes 0\nwithheld 256\n",
            b"fabric-conv 1\nphi clean\nsub 0\nstrikes 0\nwithheld 0\nmore\n",
        ] {
            assert_eq!(ConvState::decode(bad), None);
        }
    }

    #[test]
    fn the_second_copy_survives_an_rm_and_the_more_stopping_copy_wins() {
        let dir = std::env::temp_dir().join(format!("heddle-second-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("state")).expect("folder");
        let m = Machine::open(dir.join("state"))
            .expect("folder")
            .with_second(dir.join("labels"));
        let c = ConvId::parse(b"00893aaf-19fa-41d2-8238-13269b9b3ca0").expect("conv");
        let fresh = IncidentId::parse(b"inc-bbbbbb").expect("id");
        let held = ConvState {
            phi: Phi::Held,
            ..ConvState::CLEAN
        };
        m.write_conv(&c, &held).expect("write");
        assert!(
            dir.join("labels").join(c.to_string()).exists(),
            "both copies written"
        );
        // One accidental rm: the other copy holds, and is written back.
        fs::remove_file(m.conv_path(&c)).expect("rm");
        assert_eq!(m.read_conv(&c, fresh), (held, true));
        // The copies disagree: the more stopping one wins.
        m.write_conv(&c, &ConvState::CLEAN).expect("write");
        fs::write(dir.join("labels").join(c.to_string()), held.encode()).expect("write");
        assert_eq!(m.read_conv(&c, fresh), (held, true));
        // Either copy unreadable is braked.
        fs::write(dir.join("labels").join(c.to_string()), b"garbage").expect("write");
        assert_eq!(m.read_conv(&c, fresh), (ConvState::braked(fresh), true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_land_whole_and_read_back() {
        let dir = std::env::temp_dir().join(format!("heddle-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(
            Machine::open(dir.clone()).is_err(),
            "a missing state folder is a fault"
        );
        fs::create_dir(&dir).expect("folder");
        let m = Machine::open(dir.clone()).expect("folder");
        let c = ConvId::parse(b"00893aaf-19fa-41d2-8238-13269b9b3ca0").expect("conv");
        let fresh = IncidentId::parse(b"inc-bbbbbb").expect("id");
        assert_eq!(m.read_conv(&c, fresh), (ConvState::CLEAN, false));
        let _lock = m.lock().expect("lock");
        let held = ConvState {
            phi: Phi::Held,
            ..ConvState::CLEAN
        };
        m.write_conv(&c, &held).expect("write");
        assert_eq!(m.read_conv(&c, fresh), (held, false));
        let _ = fs::remove_dir_all(dir);
    }
}
