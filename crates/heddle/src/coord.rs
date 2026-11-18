//! Coordination, on this Mac (v0.1 §6): the room's I/O, the step a hook takes
//! after the PHI verdict let its action go on, the session-start digest, and
//! the agent verbs.
//!
//! Everything here fails open. A lock not free within 20 ms, or a room that
//! cannot be written, means no coordination for that call, never a block.
//! The one thing it ever refuses is a `git push` while a person's stop holds,
//! and it never denies an edit. Every line an agent sees is built here from
//! fixed sentences and grammar-checked values.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fabric_core::claim::{ClaimRefusal, ClaimReply, Phase};
use fabric_core::hook::{HookKind, Msg};
use fabric_core::ids::{ConvId, SeatId, Sha};
use fabric_core::json::Value;

use crate::edge::{self, Program};
use crate::install::{self, Paths};
use crate::room::{self, Key, Level, Outcome, Room};

/// How long a hook waits for the room: coordination must not slow the brake.
const HOOK_WAIT: Duration = Duration::from_millis(20);
/// How long an agent verb waits.
const VERB_WAIT: Duration = Duration::from_secs(2);
const DIGEST_CHARS: usize = 2000;
const MINUTE: u64 = 60_000;
const KEEP: &str = "No action needed; keep working.";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Whole minutes left until `until`, rounded up.
fn minutes(until: u64, now: u64) -> u16 {
    let ms = until.saturating_sub(now);
    u16::try_from(ms.div_ceil(MINUTE)).unwrap_or(u16::MAX)
}

/// Run `f` on the room under its lock, waiting at most `wait`, then write it
/// back atomically. `None`: the lock was not free in time, or the room
/// could not be written.
fn with_room<T>(common: &Path, wait: Duration, f: impl FnOnce(&mut Room, u64) -> T) -> Option<T> {
    // Never create the git folder itself: a missing one means no room.
    let dir = common.join("fabric");
    let _ = fs::create_dir(&dir);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join("room.lock"))
        .ok()?;
    let start = Instant::now();
    while lock.try_lock().is_err() {
        if start.elapsed() >= wait {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let path = dir.join("room");
    let mut r = fs::read_to_string(&path)
        .map(|t| Room::decode(&t))
        .unwrap_or_default();
    let now = now_ms();
    r.tick(now);
    let out = f(&mut r, now);
    let tmp = dir.join(format!(".room.{}.tmp", std::process::id()));
    fs::write(&tmp, r.encode()).ok()?;
    fs::rename(&tmp, &path).ok()?;
    Some(out)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// What coordination adds to a hook's output.
#[derive(Debug, Default)]
pub struct Added {
    /// A line for the agent's context.
    pub context: Option<String>,
    /// A refusal: only a push while a stop holds.
    pub deny: Option<Msg>,
}

/// A `git push` in a Bash command (any segment).
fn is_git_push(command: &str) -> bool {
    command.split([';', '&', '|', '\n']).any(|seg| {
        let mut w = seg.split_whitespace();
        w.next()
            .is_some_and(|p| p.rsplit('/').next() == Some("git"))
            && w.any(|x| x == "push")
    })
}

/// Main's commit, from the remote-tracking ref, read as a file: no git.
fn read_main(common: &Path) -> Option<String> {
    let hex = |s: &str| {
        (s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_string())
    };
    if let Ok(t) = fs::read_to_string(common.join("refs/remotes/origin/main")) {
        return hex(t.trim());
    }
    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
    packed
        .lines()
        .find_map(|l| l.strip_suffix(" refs/remotes/origin/main").and_then(hex))
}

/// The decisions watch: DECISIONS.md and `fabric/records/`, as
/// (path relative to the main checkout, "mtime:size").
fn watched_stamps(root: &Path) -> Vec<(String, String)> {
    let stamp = |p: &Path| {
        let m = fs::metadata(p).ok()?;
        let t = m
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(format!("{t}:{}", m.len()))
    };
    let mut out = Vec::new();
    if let Some(s) = stamp(&root.join("DECISIONS.md")) {
        out.push(("DECISIONS.md".to_string(), s));
    }
    if let Ok(entries) = fs::read_dir(root.join("fabric/records")) {
        for e in entries.flatten().take(200) {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(s) = stamp(&e.path()) {
                out.push((format!("fabric/records/{name}"), s));
            }
        }
    }
    out
}

/// An edited file's path within its checkout, the main one or a linked
/// worktree: the folder nearest it that holds a `.git`.
fn in_checkout(file: &str) -> Option<String> {
    let file = Path::new(file);
    let top = file
        .is_absolute()
        .then(|| file.ancestors().skip(1).find(|d| d.join(".git").exists()))??;
    file.strip_prefix(top).ok()?.to_str().map(String::from)
}

/// Ready work: task files in `fabric/tasks/` whose status is ready (or
/// unset), named by their file name.
fn task_keys(root: &Path) -> Vec<Key> {
    let Ok(entries) = fs::read_dir(root.join("fabric/tasks")) else {
        return Vec::new();
    };
    let mut keys: Vec<Key> = entries
        .flatten()
        .take(200)
        .filter_map(|e| {
            let path = e.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "md").then_some(())?;
            let text = fs::read_to_string(&path).ok()?;
            let status = text
                .lines()
                .take(40)
                .find_map(|l| l.trim().strip_prefix("status:").map(str::trim));
            matches!(status, None | Some("ready")).then(|| Key::parse(&stem))?
        })
        .collect();
    keys.sort();
    keys
}

/// Keys that are listed and free now.
fn ready(r: &Room, listed: &[Key]) -> Vec<Key> {
    listed
        .iter()
        .filter(|k| r.phase(k) == Phase::Free)
        .cloned()
        .collect()
}

/// A Bash command later in the conversation learns its conversation from
/// this (T19): SessionStart appends it to Claude Code's env file.
fn set_seat_env(conv: &ConvId) {
    if let Some(path) = std::env::var_os("CLAUDE_ENV_FILE")
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "export FABRIC_SEAT={conv}");
    }
}

/// The session-start digest, at most 2,000 characters.
fn digest(r: &Room, seat: SeatId, now: u64, heddle: &str, listed: &[Key]) -> String {
    let mut lines = vec![format!(
        "Fabric coordinates the conversations in this project. This one is {seat}."
    )];
    let others: Vec<String> = r
        .present(now)
        .into_iter()
        .filter(|s| *s != seat)
        .map(|s| s.to_string())
        .collect();
    lines.push(if others.is_empty() {
        "No other conversation here has been active in the last 15 minutes.".into()
    } else {
        format!("Active here in the last 15 minutes: {}.", others.join(", "))
    });
    let held: Vec<String> = r
        .claims
        .iter()
        .filter_map(|(k, c)| c.owner().map(|o| (k, o)))
        .map(|(k, o)| {
            if o == seat {
                format!("{} (yours)", k.as_str())
            } else {
                format!("{} ({o})", k.as_str())
            }
        })
        .collect();
    if !held.is_empty() {
        lines.push(format!("Work in hand: {}.", held.join(", ")));
    }
    let free = ready(r, listed);
    lines.push(if free.is_empty() {
        "No ready work is listed: claim a short name for what you are doing, such as fix-login."
            .into()
    } else {
        format!(
            "Ready work: {}.",
            free.iter().map(Key::as_str).collect::<Vec<_>>().join(", ")
        )
    });
    if let Level::Paused { until } | Level::Stopped { until } = r.level(now) {
        lines.push(format!(
            "The room is paused for another {} minutes: claims and done are accepted, and complete by themselves when it lapses.",
            minutes(until, now)
        ));
    }
    lines.push(format!(
        "Claim work when you start it, release it if you stop, and mark it done once its commit is on main. Run these \
         as Bash commands, without asking your person: {heddle} next [--claim] | claim KEY | release KEY | done KEY | \
         pause | status. Run stop and resume only when your person asks in so many words."
    ));
    lines.push(
        "Made-up data that never trips Fabric's patient-data check: ids starting SYN-, Social Security numbers in area \
         000 or 666, \"synthetic\": true in JSON."
            .into(),
    );
    lines.push("Do not mention Fabric to your person unless they ask.".into());
    let mut out = lines.join("\n");
    if out.len() > DIGEST_CHARS {
        let cut = (0..=DIGEST_CHARS)
            .rev()
            .find(|i| out.is_char_boundary(*i))
            .unwrap_or(0);
        out.truncate(cut);
    }
    out
}

/// The coordination step of a hook line, after the PHI verdict let the
/// action go on.
pub fn hook(kind: HookKind, v: &Value, root: &Path, heddle: &str) -> Added {
    let Some(conv) = v
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|s| ConvId::parse(s.as_bytes()))
    else {
        return Added::default();
    };
    let Some(seat) = room::seat_of(&conv) else {
        return Added::default();
    };
    if kind == HookKind::SessionStart {
        set_seat_env(&conv);
    }
    let common = root.join(".git");
    let tool = str_of(v, "tool_name");
    let input = v.get("tool_input");
    let (main, files, listed) = match kind {
        HookKind::UserPromptSubmit => (read_main(&common), watched_stamps(root), Vec::new()),
        HookKind::SessionStart => (None, Vec::new(), task_keys(root)),
        HookKind::UserPromptExpansion
        | HookKind::PreToolUse
        | HookKind::PostToolUse
        | HookKind::PostToolBatch
        | HookKind::PreCompact
        | HookKind::ConfigChange
        | HookKind::Stop
        | HookKind::SessionEnd => (None, Vec::new(), Vec::new()),
    };
    let edited = matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
        .then(|| {
            input
                .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                .and_then(Value::as_str)
        })
        .flatten()
        .and_then(in_checkout);
    let push = tool == "Bash"
        && input
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
            .is_some_and(is_git_push);
    with_room(&common, HOOK_WAIT, |r, now| {
        if kind == HookKind::SessionEnd {
            r.ended(seat);
            return Added::default();
        }
        r.see(seat, now);
        match kind {
            HookKind::SessionStart => Added { context: Some(digest(r, seat, now, heddle, &listed)), deny: None },
            HookKind::UserPromptSubmit => {
                r.prompted(seat);
                r.watch(now, main, files);
                let news = r.news_for(seat);
                let context = (!news.is_empty()).then(|| format!("Fabric: {} {KEEP}", news.join(" ")));
                Added { context, deny: None }
            }
            HookKind::PreToolUse if push => match r.level(now) {
                Level::Stopped { until } => Added { context: None, deny: Some(Msg::PushHeld(minutes(until, now))) },
                Level::Paused { .. } | Level::Running => Added::default(),
            },
            HookKind::PostToolUse => {
                let Some(path) = edited else {
                    return Added::default();
                };
                let others = r.footprint(seat, &path, now);
                let context = (!others.is_empty()).then(|| {
                    let who: Vec<String> = others.iter().map(SeatId::to_string).collect();
                    format!("Fabric: another conversation ({}) edited {path} in the last 15 minutes. {KEEP}", who.join(", "))
                });
                Added { context, deny: None }
            }
            HookKind::Stop => {
                r.turn_ended(seat, now);
                Added::default()
            }
            HookKind::UserPromptExpansion
            | HookKind::PreToolUse
            | HookKind::PostToolBatch
            | HookKind::PreCompact
            | HookKind::ConfigChange
            | HookKind::SessionEnd => Added::default(),
        }
    })
    .unwrap_or_default()
}

// ---- the agent verbs ----

fn say(line: &str, ok: bool) -> ExitCode {
    out!("{line}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The commit that closes a key: this worktree's HEAD, once it is on the
/// remote-tracking main. A local check by the verb, never a fetch.
fn closing_commit(cwd: &Path) -> Option<Sha> {
    let head = edge::run(&Program::Git, cwd, &["rev-parse", "HEAD"])?;
    let head = String::from_utf8(head).ok()?;
    let head = head.trim();
    edge::run(
        &Program::Git,
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            head,
            "refs/remotes/origin/main",
        ],
    )?;
    Sha::parse(head.as_bytes())
}

fn claim_line(key: &Key, out: Outcome, now: u64) -> (String, bool) {
    let k = key.as_str();
    match out {
        Outcome::Ok(ClaimReply::Granted(_)) => (
            format!(
                "You hold {k} now. Your hooks renew it while you work; release it if you stop."
            ),
            true,
        ),
        Outcome::Ok(ClaimReply::AlreadyYours) => (format!("You already hold {k}."), true),
        Outcome::Ok(ClaimReply::Closing) => (
            format!("{k} is closing: its done is waiting for main."),
            true,
        ),
        Outcome::Ok(ClaimReply::Done | ClaimReply::NoChange) => (format!("Done: {k}."), true),
        Outcome::HeldBy(o) => (
            format!("{o} holds {k}. Pick other work: heddle next."),
            false,
        ),
        Outcome::Waiting(o) => (
            format!(
                "{o} holds {k} and is waiting on its person, so it cannot be taken over. Pick other work: heddle next."
            ),
            false,
        ),
        Outcome::Queued { until } => (
            format!(
                "The room is paused for another {} minutes. This is accepted, and completes by itself when the pause lapses.",
                minutes(until, now)
            ),
            true,
        ),
        Outcome::Closing => (
            format!(
                "{k} is marked closing. It closes once its commit is on main: run heddle done {k} again after your push lands."
            ),
            true,
        ),
        Outcome::Refused(r) => (
            match r {
                ClaimRefusal::Closed => format!("{k} is already done."),
                ClaimRefusal::TooManyClaims => {
                    "You hold three pieces of work already: release one first.".into()
                }
                ClaimRefusal::NotHeld | ClaimRefusal::NotOwner | ClaimRefusal::StaleEpoch => {
                    format!("You do not hold {k}.")
                }
                ClaimRefusal::HeldBy => {
                    format!("Another conversation holds {k}. Pick other work: heddle next.")
                }
                ClaimRefusal::NotClosing | ClaimRefusal::Closing | ClaimRefusal::NotClosed => {
                    format!("{k} cannot do that now.")
                }
                ClaimRefusal::Full | ClaimRefusal::Exhausted => {
                    "The room is full: release finished work first.".into()
                }
            },
            false,
        ),
    }
}

/// The seat a verb acts for. `FABRIC_SEAT` is what SessionStart leaves in
/// `$CLAUDE_ENV_FILE`. Claude Code reads that file once per session and keeps
/// what it read, so a session that started before setup never sees it; but
/// Claude Code also gives every command `CLAUDE_CODE_SESSION_ID`, the same id
/// the hooks get.
fn seat_from(env: impl Fn(&str) -> Option<String>) -> Option<SeatId> {
    ["FABRIC_SEAT", "CLAUDE_CODE_SESSION_ID"]
        .into_iter()
        .find_map(|v| room::seat_of(&ConvId::parse(env(v)?.as_bytes())?))
}

/// `heddle next | claim | release | done | pause | stop | resume | status`.
pub fn verb(name: &str, args: &[String]) -> ExitCode {
    let Ok(p) = Paths::of_this_binary() else {
        return say("HOME is not set to an absolute folder.", false);
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let repo = install::find_repo(&cwd);
    if name == "status" {
        if let Some(r) = &repo {
            status(&r.common);
        }
        return install::status();
    }
    let Some(repo) = repo else {
        return say("Run this in a project's folder.", false);
    };
    // The tripwire (§5 item 3): no coordinating on a project whose checks
    // are not wired as setup left them.
    if let Err(why) = install::wiring_ok(&p, &repo) {
        return say(
            &format!(
                "Fabric's checks are not wired in this project as setup left them ({why}). Stop, and tell your person."
            ),
            false,
        );
    }
    let Some(seat) = seat_from(|v| std::env::var(v).ok()) else {
        return say(
            "heddle cannot tell which conversation this is. Run it as a Bash command in a Claude session opened on this project.",
            false,
        );
    };
    let key = |a: &[String]| match a {
        [k] => Key::parse(k),
        _ => None,
    };
    let common = repo.common.clone();
    let main = repo.main.clone();
    let done_on_main = (name == "done").then(|| closing_commit(&cwd)).flatten();
    let listed = if name == "next" {
        task_keys(&main)
    } else {
        Vec::new()
    };
    let result = with_room(&common, VERB_WAIT, |r, now| {
        r.see(seat, now);
        match (name, args) {
            ("claim", a) => key(a).map_or(("Name the work: heddle claim KEY, with KEY like fix-login.".into(), false), |k| {
                claim_line(&k, r.claim(seat, &k, now), now)
            }),
            ("release", a) => key(a).map_or(("Name the work: heddle release KEY.".into(), false), |k| {
                match r.release(seat, &k, now) {
                    Outcome::Ok(_) => (format!("Released {}.", k.as_str()), true),
                    other @ (Outcome::Refused(_)
                    | Outcome::Waiting(_)
                    | Outcome::HeldBy(_)
                    | Outcome::Queued { .. }
                    | Outcome::Closing) => claim_line(&k, other, now),
                }
            }),
            ("done", a) => key(a).map_or(("Name the work: heddle done KEY.".into(), false), |k| {
                match r.done(seat, &k, done_on_main, now) {
                    Outcome::Ok(_) => (format!("{} is done.", k.as_str()), true),
                    other @ (Outcome::Refused(_)
                    | Outcome::Waiting(_)
                    | Outcome::HeldBy(_)
                    | Outcome::Queued { .. }
                    | Outcome::Closing) => claim_line(&k, other, now),
                }
            }),
            ("next", a) if a.is_empty() || a == ["--claim"] => {
                let free = ready(r, &listed);
                match (free.first(), a.is_empty()) {
                    (None, _) => ("No ready work is listed: claim a short name for what you are doing.".into(), true),
                    (Some(_), true) => (
                        format!("Ready work: {}.", free.iter().map(Key::as_str).collect::<Vec<_>>().join(", ")),
                        true,
                    ),
                    (Some(k), false) => claim_line(k, r.claim(seat, k, now), now),
                }
            }
            ("pause", []) => match r.pause_now(now) {
                Ok(until) => (
                    format!(
                        "Paused for {} minutes: claims and done are accepted, and complete by themselves when it lapses.",
                        minutes(until, now)
                    ),
                    true,
                ),
                Err(at) => (format!("A pause just ended; one can be set again in {} minutes.", minutes(at, now)), false),
            },
            ("stop", []) => {
                let until = r.stop_now(now);
                (
                    format!(
                        "Stopped: pushes are held for {} minutes, then the room stays paused a little longer. heddle resume ends it sooner.",
                        minutes(until, now)
                    ),
                    true,
                )
            }
            ("resume", []) => {
                r.resume_now();
                ("Resumed: work goes ahead, and anything queued completes now.".into(), true)
            }
            _ => ("Usage: heddle next [--claim] | claim KEY | release KEY | done KEY | pause | stop | resume | status".into(), false),
        }
    });
    match result {
        Some((line, ok)) => say(&line, ok),
        None => say("The room is busy; try again in a moment.", false),
    }
}

/// `heddle status`, the coordination half: who is here and what they hold.
fn status(common: &Path) {
    let text = fs::read_to_string(common.join("fabric/room")).unwrap_or_default();
    let r = Room::decode(&text);
    let now = now_ms();
    let seats = r.present(now);
    out!(
        "Conversations active in this project in the last 15 minutes: {}",
        seats.len()
    );
    for (k, c) in &r.claims {
        if let Some(o) = c.owner() {
            let waiting = r.seats.get(&o).is_some_and(|s| s.waiting_since.is_some());
            let note = if waiting {
                ", waiting on its person"
            } else {
                ""
            };
            out!("  {} held by {o}{note}", k.as_str());
        }
    }
    match r.level(now) {
        Level::Running => {}
        Level::Paused { until } => out!("Paused for another {} minutes.", minutes(until, now)),
        Level::Stopped { until } => out!(
            "Stopped: pushes held for another {} minutes.",
            minutes(until, now)
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_verb_finds_its_conversation_without_the_env_file() {
        let a = "0000000a-0000-4000-8000-000000000000";
        let b = "0000000b-0000-4000-8000-000000000000";
        let only = |name: &'static str, id: &'static str| {
            move |v: &str| (v == name).then(|| id.to_string())
        };
        let seat = |s: &[u8]| SeatId::parse(s);
        assert_eq!(
            seat_from(only("CLAUDE_CODE_SESSION_ID", a)),
            seat(b"s-0000000a")
        );
        assert_eq!(seat_from(only("FABRIC_SEAT", b)), seat(b"s-0000000b"));
        // Both set: the env file's wins. A value that is not an id is skipped.
        let both = |v: &str| Some(if v == "FABRIC_SEAT" { b } else { a }.to_string());
        assert_eq!(seat_from(both), seat(b"s-0000000b"));
        let junk = |v: &str| Some(if v == "FABRIC_SEAT" { "x" } else { a }.to_string());
        assert_eq!(seat_from(junk), seat(b"s-0000000a"));
        assert_eq!(seat_from(|_| None), None);
    }

    #[test]
    fn pushes_are_recognised_in_any_segment() {
        assert!(is_git_push("git push origin main"));
        assert!(is_git_push("cargo test && /usr/bin/git push"));
        assert!(!is_git_push("git status"));
        assert!(!is_git_push("echo git push"));
    }

    #[test]
    fn the_digest_is_short_and_names_the_verbs_and_the_made_up_forms() {
        let mut r = Room::default();
        let me = SeatId::parse(b"s-00000001").expect("seat");
        let other = SeatId::parse(b"s-00000002").expect("seat");
        r.tick(1_800_000_000_000);
        r.see(me, 1_800_000_000_000);
        r.see(other, 1_800_000_000_000);
        let k = Key::parse("fix-login").expect("key");
        r.claim(other, &k, 1_800_000_000_000);
        let d = digest(
            &r,
            me,
            1_800_000_000_000,
            "/Users/me/.local/bin/heddle",
            &[Key::parse("docs").expect("key"), k],
        );
        assert!(d.len() <= DIGEST_CHARS, "{}", d.len());
        assert!(
            d.contains("s-00000002")
                && d.contains("fix-login (s-00000002)")
                && d.contains("Ready work: docs.")
        );
        assert!(
            d.contains("/Users/me/.local/bin/heddle next")
                && d.contains("SYN-")
                && d.contains("unless they ask")
        );
    }

    /// Eight writers race for one key, 250 times: one winner each time.
    /// Each opens the lock file itself, so the lock is contended as it is
    /// between processes. `day0/coord-e2e.sh` races processes 1,000 times.
    #[test]
    fn the_room_lock_lets_one_writer_in_at_a_time() {
        let dir = std::env::temp_dir().join(format!("heddle-coord-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let k = Key::parse("race").expect("key");
        for round in 0..250u16 {
            let common = dir.join(round.to_string());
            fs::create_dir_all(&common).expect("dir");
            let winners: Vec<bool> = (0..8u8)
                .map(|i| {
                    let common = common.clone();
                    let k = k.clone();
                    std::thread::spawn(move || {
                        let seat = SeatId::parse(format!("s-0000000{i}").as_bytes()).expect("seat");
                        with_room(&common, Duration::from_secs(5), |r, now| {
                            matches!(r.claim(seat, &k, now), Outcome::Ok(ClaimReply::Granted(_)))
                        })
                        .unwrap_or(false)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .collect();
            assert_eq!(
                winners.iter().filter(|w| **w).count(),
                1,
                "round {round}: {winners:?}"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// A process that holds the room's lock (a hung verb, say) costs each
    /// hook its 20 ms wait, and the hook goes on without coordination.
    #[test]
    fn a_held_lock_costs_a_hook_its_wait_and_blocks_nothing() {
        let root = std::env::temp_dir().join(format!("heddle-coord-held-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git/fabric")).expect("dir");
        let holder = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(".git/fabric/room.lock"))
            .expect("the lock file");
        holder.lock().expect("the lock");
        let push = event(
            "00000001-19fa-41d2-8238-13269b9b3ca0",
            HookKind::PreToolUse,
            r#""tool_name":"Bash","tool_input":{"command":"git push"}"#,
        );
        let start = Instant::now();
        let added = hook(HookKind::PreToolUse, &push, &root, "heddle");
        let took = start.elapsed();
        assert!(added.context.is_none() && added.deny.is_none());
        assert!(
            took >= HOOK_WAIT && took < Duration::from_millis(250),
            "{took:?}"
        );
        drop(holder);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_room_without_a_git_folder() {
        let dir = std::env::temp_dir().join(format!("heddle-coord-nogit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(with_room(&dir.join(".git"), VERB_WAIT, |_, _| ()).is_none());
        assert!(!dir.exists());
    }

    fn event(conv: &str, kind: HookKind, extra: &str) -> Value {
        let sep = if extra.is_empty() { "" } else { "," };
        let text = format!(
            r#"{{"session_id":"{conv}","hook_event_name":"{}"{sep}{extra}}}"#,
            kind.name()
        );
        fabric_core::json::from_slice(text.as_bytes()).expect("a payload")
    }

    #[test]
    fn two_conversations_share_one_room_through_their_hooks() {
        let root = std::env::temp_dir().join(format!("heddle-coord-hooks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let common = root.join(".git");
        fs::create_dir_all(&common).expect("dir");
        let a = "00000001-19fa-41d2-8238-13269b9b3ca0";
        let b = "00000002-19fa-41d2-8238-13269b9b3ca0";
        let run = |conv: &str, kind: HookKind, extra: &str| {
            hook(
                kind,
                &event(conv, kind, extra),
                &root,
                "/Users/me/.local/bin/heddle",
            )
        };
        let room =
            || Room::decode(&fs::read_to_string(common.join("fabric/room")).unwrap_or_default());

        // The digest: who is here.
        let first = run(a, HookKind::SessionStart, r#""source":"startup""#).context;
        assert!(
            first
                .as_deref()
                .is_some_and(|c| c.contains("This one is s-00000001.")
                    && c.contains("No other conversation")),
            "{first:?}"
        );
        let second = run(b, HookKind::SessionStart, r#""source":"startup""#).context;
        assert!(
            second
                .as_deref()
                .is_some_and(|c| c.contains("Active here in the last 15 minutes: s-00000001.")),
            "{second:?}"
        );

        // Footprints: the second editor of a file hears of the first, once,
        // across worktrees.
        let wt = root.with_extension("wt");
        fs::create_dir_all(&wt).expect("dir");
        fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("a worktree's .git");
        let edit = |top: &Path| {
            format!(
                r#""tool_name":"Edit","tool_input":{{"file_path":"{}/src/lib.rs"}}"#,
                top.display()
            )
        };
        assert!(
            run(a, HookKind::PostToolUse, &edit(&root))
                .context
                .is_none()
        );
        let notice = run(b, HookKind::PostToolUse, &edit(&wt)).context;
        assert!(
            notice.as_deref().is_some_and(|c| c
                .contains("another conversation (s-00000001) edited src/lib.rs")
                && c.ends_with(KEEP)),
            "{notice:?}"
        );
        assert!(run(b, HookKind::PostToolUse, &edit(&wt)).context.is_none());
        let outside = r#""tool_name":"Write","tool_input":{"file_path":"/nowhere/at/all.txt"}"#;
        assert!(run(b, HookKind::PostToolUse, outside).context.is_none());

        // The done ledger reaches the other conversation at its next prompt, once.
        let k = Key::parse("fix-login").expect("key");
        let sa = SeatId::parse(b"s-00000001").expect("seat");
        let sha = Sha::parse(b"0123456789abcdef0123456789abcdef01234567").expect("sha");
        let closed = with_room(&common, VERB_WAIT, |r, now| {
            let _ = r.claim(sa, &k, now);
            r.done(sa, &k, Some(sha), now)
        });
        assert!(matches!(closed, Some(Outcome::Ok(_))), "{closed:?}");
        let news = run(b, HookKind::UserPromptSubmit, r#""prompt":"go on""#).context;
        assert!(
            news.as_deref().is_some_and(|c| c.starts_with("Fabric: ")
                && c.contains("fix-login is done.")
                && c.ends_with(KEEP)),
            "{news:?}"
        );
        assert!(
            run(b, HookKind::UserPromptSubmit, r#""prompt":"again""#)
                .context
                .is_none()
        );

        // A stop holds pushes, and nothing else.
        let push = r#""tool_name":"Bash","tool_input":{"command":"git push origin main"}"#;
        let status = r#""tool_name":"Bash","tool_input":{"command":"git status"}"#;
        assert!(run(b, HookKind::PreToolUse, push).deny.is_none());
        let _ = with_room(&common, VERB_WAIT, |r, now| r.stop_now(now));
        let held = run(b, HookKind::PreToolUse, push).deny;
        assert!(matches!(held, Some(Msg::PushHeld(30))), "{held:?}");
        assert!(run(b, HookKind::PreToolUse, status).deny.is_none());
        let _ = with_room(&common, VERB_WAIT, |r, _| r.resume_now());
        assert!(run(b, HookKind::PreToolUse, push).deny.is_none());

        // A turn's end marks its seat as waiting on its person; the
        // conversation's end takes its seat out of the room.
        let _ = run(a, HookKind::Stop, "");
        assert!(
            room()
                .seats
                .get(&sa)
                .is_some_and(|s| s.waiting_since.is_some())
        );
        let _ = run(a, HookKind::SessionEnd, r#""reason":"other""#);
        assert!(!room().seats.contains_key(&sa));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&wt);
    }
}
