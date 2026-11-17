//! The local room (v0.1): coordination among one repo's conversations on this
//! Mac, kept in `<git common dir>/fabric/room`. Claims, seats, footprints,
//! the pause, queued work and news.
//!
//! Everything here fails open. Nothing in the room is safety state: a lost
//! or garbled file starts empty, and a line that does not parse is dropped.
//! Every value passes its grammar before it is shown. Claim semantics are
//! the core's (`claim_step`, M1); this module stores them, adds presence and
//! news, and keeps the design's timings (v0.1 §6).

use std::collections::BTreeMap;

use fabric_core::claim::{
    self, Claim, ClaimCtx, ClaimEvent, ClaimRefusal, ClaimReply, Closure, Epoch, Phase,
};
use fabric_core::config::Config;
use fabric_core::ids::{ConvId, SeatId, Sha};
use fabric_core::time::{self, Clocks, LeaseTime, RoomTime};

const MINUTE: u64 = 60_000;
/// A seat is present when seen in the last 15 minutes.
const PRESENT: u64 = 15 * MINUTE;
/// "Last seen" is stamped at most once a minute.
const STAMP_EVERY: u64 = MINUTE;
/// A claim waiting on a person cannot be taken over for 12 hours.
const WAIT_HOLD: u64 = 12 * 60 * MINUTE;
/// A seat with no claims, unseen this long, is forgotten.
const SEAT_FORGET: u64 = 24 * 60 * MINUTE;
const PAUSE_FOR: u64 = 15 * MINUTE;
const PAUSE_MAX: u64 = 60 * MINUTE;
const PAUSE_COOLDOWN: u64 = 15 * MINUTE;
const STOP_FOR: u64 = 30 * MINUTE;
/// News shown in one turn, in characters.
const NEWS_CHARS: usize = 400;
/// The decisions watch runs at most this often.
const WATCH_EVERY: u64 = 30_000;
/// Closed keys are remembered this long.
const CLOSED_KEEP: u64 = 30 * 24 * 60 * MINUTE;

const FORMAT: &str = "fabric-room 1";

/// Keep only the newest `n` items of `v`.
fn newest<T>(v: &mut Vec<T>, n: usize) {
    let skip = v.len().saturating_sub(n);
    if skip > 0 {
        *v = std::mem::take(v).into_iter().skip(skip).collect();
    }
}

fn cfg() -> &'static Config {
    &Config::V01
}

/// A work key: a task file's name or a short slug, `[a-z0-9-]{1,40}`, not
/// starting with `-`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// The one checked constructor.
    pub fn parse(s: &str) -> Option<Key> {
        let ok = !s.is_empty()
            && s.len() <= 40
            && !s.starts_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| Key(s.to_string()))
    }
    /// The key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A path as the room stores it: printable ASCII, absolute, at most 400
/// bytes. Others are not tracked.
/// A footprint: a file's path within its checkout, so edits to one file in
/// two worktrees meet.
fn path_ok(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && p.len() <= 400
        && p.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// The seat of a conversation: `s-` and the first eight hex digits of its id,
/// so a hook and a verb in the same conversation agree without storing
/// anything.
pub fn seat_of(conv: &ConvId) -> Option<SeatId> {
    let id = conv.to_string();
    let hex: String = id.chars().filter(char::is_ascii_hexdigit).take(8).collect();
    SeatId::parse(format!("s-{hex}").as_bytes())
}

/// One conversation's place in the room.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seat {
    /// When a hook last stamped it.
    pub last_seen: u64,
    /// Set by the Stop line, cleared by the next prompt.
    pub waiting_since: Option<u64>,
    /// The last news item this seat was told.
    pub seen: u64,
    /// Paths it edited, newest last, at most `max_paths`.
    pub paths: Vec<String>,
    /// Paths it was already told someone else also edited.
    pub noticed: Vec<String>,
}

/// A news item: a typed pointer, never prose from anyone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum News {
    /// A claim was taken over from `from` by `by`.
    Taken { key: Key, from: SeatId, by: SeatId },
    /// A key closed.
    Closed { key: Key },
    /// Main moved.
    Main { sha: String },
    /// A watched file changed.
    File { path: String },
    /// A claim queued during a pause did not go through.
    QueuedFailed { key: Key, seat: SeatId },
}

/// One news item, numbered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewsItem {
    pub seq: u64,
    pub news: News,
}

/// The pause, and a stop on top of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pause {
    pub first_at: u64,
    pub until: u64,
    pub stop_until: Option<u64>,
}

/// Work accepted during a pause, done when it lapses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub seat: SeatId,
    pub key: Key,
    /// `None` for a claim; the closing commit for a `done`.
    pub done: Option<Sha>,
}

/// The room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub clocks: Clocks,
    pub epoch: u64,
    pub seq: u64,
    pub seats: BTreeMap<SeatId, Seat>,
    pub claims: BTreeMap<Key, Claim>,
    pub news: Vec<NewsItem>,
    pub pause: Option<Pause>,
    pub cooldown_until: u64,
    pub queued: Vec<Queued>,
    pub main: Option<String>,
    pub watched: BTreeMap<String, String>,
    pub watch_checked: u64,
}

impl Default for Room {
    fn default() -> Room {
        Room {
            clocks: Clocks {
                last_now: RoomTime(0),
                lease_clock: LeaseTime(0),
            },
            epoch: 0,
            seq: 0,
            seats: BTreeMap::new(),
            claims: BTreeMap::new(),
            news: Vec::new(),
            pause: None,
            cooldown_until: 0,
            queued: Vec::new(),
            main: None,
            watched: BTreeMap::new(),
            watch_checked: 0,
        }
    }
}

/// How the room is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Running,
    Paused { until: u64 },
    Stopped { until: u64 },
}

/// What a claim or a `done` came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted.
    Ok(ClaimReply),
    /// Refused.
    Refused(ClaimRefusal),
    /// Held by a seat that is waiting on its person.
    Waiting(SeatId),
    /// Held by another seat inside its lease.
    HeldBy(SeatId),
    /// The room is paused: accepted, and done when the pause lapses.
    Queued { until: u64 },
    /// `done` began; it closes once the commit is on main.
    Closing,
}

impl Room {
    /// The lease clock now.
    fn t(&self) -> LeaseTime {
        self.clocks.lease_clock
    }

    /// The level at room time `now`.
    pub fn level(&self, now: u64) -> Level {
        match self.pause {
            Some(Pause {
                stop_until: Some(s),
                ..
            }) if s > now => Level::Stopped { until: s },
            Some(Pause { until, .. }) if until > now => Level::Paused { until },
            Some(_) | None => Level::Running,
        }
    }

    /// The first thing every request does: move the clocks (frozen while
    /// paused), let a pause lapse, run queued work, and forget old rows.
    pub fn tick(&mut self, now: u64) {
        let frozen = !matches!(self.level(self.clocks.last_now.0), Level::Running);
        self.clocks = time::advance(self.clocks, RoomTime(now), frozen, cfg());
        if let Some(p) = self.pause
            && p.until <= now
        {
            self.pause = None;
            self.cooldown_until = now.saturating_add(PAUSE_COOLDOWN);
        }
        if matches!(self.level(now), Level::Running) && !self.queued.is_empty() {
            for q in std::mem::take(&mut self.queued) {
                let out = match q.done {
                    None => self.claim(q.seat, &q.key, now),
                    Some(sha) => self.done(q.seat, &q.key, Some(sha), now),
                };
                if !matches!(out, Outcome::Ok(_) | Outcome::Closing) {
                    self.push(News::QueuedFailed {
                        key: q.key.clone(),
                        seat: q.seat,
                    });
                }
            }
        }
        self.claims.retain(|_, c| match c {
            Claim::Closed { at, .. } => now.saturating_sub(at.0) < CLOSED_KEEP,
            Claim::Held { .. } | Claim::Closing { .. } => true,
        });
        let owners: Vec<SeatId> = self.claims.values().filter_map(Claim::owner).collect();
        self.seats
            .retain(|id, s| owners.contains(id) || now.saturating_sub(s.last_seen) < SEAT_FORGET);
        newest(&mut self.news, usize::from(cfg().get().max_news));
    }

    fn push(&mut self, news: News) {
        self.seq = self.seq.saturating_add(1);
        self.news.push(NewsItem {
            seq: self.seq,
            news,
        });
    }

    /// A hook from `seat`: stamp it (at most once a minute) and renew its claims.
    pub fn see(&mut self, seat: SeatId, now: u64) {
        let seq = self.seq;
        let s = self.seats.entry(seat).or_insert(Seat {
            seen: seq,
            ..Seat::default()
        });
        if now.saturating_sub(s.last_seen) >= STAMP_EVERY {
            s.last_seen = now;
        }
        let t = self.t();
        for c in self.claims.values_mut() {
            *c = claim::sync_renew(Some(*c), seat, t, cfg()).unwrap_or(*c);
        }
    }

    /// The seats seen in the last 15 minutes.
    pub fn present(&self, now: u64) -> Vec<SeatId> {
        self.seats
            .iter()
            .filter(|(_, s)| now.saturating_sub(s.last_seen) < PRESENT)
            .map(|(id, _)| *id)
            .collect()
    }

    fn waiting(&self, seat: SeatId, now: u64) -> bool {
        self.seats
            .get(&seat)
            .and_then(|s| s.waiting_since)
            .is_some_and(|w| now.saturating_sub(w) < WAIT_HOLD)
    }

    /// The phase of a key's claim now.
    pub fn phase(&self, key: &Key) -> Phase {
        claim::phase(self.claims.get(key), self.t(), cfg())
    }

    fn ctx(&self, seat: SeatId) -> ClaimCtx {
        let open = |c: &&Claim| matches!(c, Claim::Held { .. } | Claim::Closing { .. });
        let by = self
            .claims
            .values()
            .filter(open)
            .filter(|c| c.owner() == Some(seat))
            .count();
        let all = self.claims.values().filter(open).count();
        ClaimCtx {
            open_by_caller: u16::try_from(by).unwrap_or(u16::MAX),
            live_rows: u16::try_from(all).unwrap_or(u16::MAX),
            next_epoch: self.epoch,
        }
    }

    /// `heddle claim <key>`.
    pub fn claim(&mut self, seat: SeatId, key: &Key, now: u64) -> Outcome {
        if let Level::Paused { until } | Level::Stopped { until } = self.level(now) {
            if !self
                .queued
                .iter()
                .any(|q| q.seat == seat && q.key == *key && q.done.is_none())
            {
                self.queued.push(Queued {
                    seat,
                    key: key.clone(),
                    done: None,
                });
            }
            return Outcome::Queued { until };
        }
        let existing = self.claims.get(key).copied();
        let owner = existing.as_ref().and_then(Claim::owner);
        // A claim whose owner waits on its person is not takeable (§6).
        if let Some(o) = owner
            && o != seat
            && self.waiting(o, now)
        {
            return Outcome::Waiting(o);
        }
        let step = claim::claim_step(
            existing,
            seat,
            ClaimEvent::Claim,
            self.t(),
            RoomTime(now),
            self.ctx(seat),
            cfg(),
        );
        self.store(key, step.claim);
        if let Ok(ClaimReply::Granted(e)) = step.outcome {
            self.epoch = e.get();
        }
        if let claim::ClaimNews::Takeover { from } = step.news {
            self.push(News::Taken {
                key: key.clone(),
                from,
                by: seat,
            });
        }
        match (step.outcome, owner) {
            (Err(ClaimRefusal::HeldBy), Some(o)) => Outcome::HeldBy(o),
            (Err(r), _) => Outcome::Refused(r),
            (Ok(r), _) => Outcome::Ok(r),
        }
    }

    fn store(&mut self, key: &Key, c: Option<Claim>) {
        match c {
            Some(c) => {
                self.claims.insert(key.clone(), c);
            }
            None => {
                self.claims.remove(key);
            }
        }
    }

    fn epoch_of(&self, key: &Key) -> Option<Epoch> {
        match self.claims.get(key) {
            Some(Claim::Held { epoch, .. }) | Some(Claim::Closing { epoch, .. }) => Some(*epoch),
            Some(Claim::Closed { .. }) | None => None,
        }
    }

    /// `heddle release <key>`.
    pub fn release(&mut self, seat: SeatId, key: &Key, now: u64) -> Outcome {
        self.queued.retain(|q| !(q.seat == seat && q.key == *key));
        let Some(epoch) = self.epoch_of(key) else {
            return Outcome::Refused(ClaimRefusal::NotHeld);
        };
        let existing = self.claims.get(key).copied();
        let step = claim::claim_step(
            existing,
            seat,
            ClaimEvent::Release { epoch },
            self.t(),
            RoomTime(now),
            self.ctx(seat),
            cfg(),
        );
        self.store(key, step.claim);
        match step.outcome {
            Ok(r) => Outcome::Ok(r),
            Err(r) => Outcome::Refused(r),
        }
    }

    /// `heddle done <key>`: begin, and close when `on_main` names the commit
    /// that is already on main.
    pub fn done(&mut self, seat: SeatId, key: &Key, on_main: Option<Sha>, now: u64) -> Outcome {
        if let Level::Paused { until } | Level::Stopped { until } = self.level(now) {
            self.queued.retain(|q| !(q.seat == seat && q.key == *key));
            match on_main {
                Some(sha) => self.queued.push(Queued {
                    seat,
                    key: key.clone(),
                    done: Some(sha),
                }),
                None => return Outcome::Refused(ClaimRefusal::NotClosing),
            }
            return Outcome::Queued { until };
        }
        let Some(epoch) = self.epoch_of(key) else {
            return Outcome::Refused(ClaimRefusal::NotHeld);
        };
        let mut c = self.claims.get(key).copied();
        if matches!(c, Some(Claim::Held { .. })) {
            let step = claim::claim_step(
                c,
                seat,
                ClaimEvent::DoneBegin { epoch },
                self.t(),
                RoomTime(now),
                self.ctx(seat),
                cfg(),
            );
            if let Err(r) = step.outcome {
                return Outcome::Refused(r);
            }
            c = step.claim;
            self.store(key, c);
        }
        let Some(sha) = on_main else {
            return Outcome::Closing;
        };
        let step = claim::claim_step(
            c,
            seat,
            ClaimEvent::DoneCommit { epoch, sha },
            self.t(),
            RoomTime(now),
            self.ctx(seat),
            cfg(),
        );
        self.store(key, step.claim);
        match step.outcome {
            Ok(r) => {
                self.push(News::Closed { key: key.clone() });
                Outcome::Ok(r)
            }
            Err(r) => Outcome::Refused(r),
        }
    }

    /// The Stop line: this seat's turn ended; its claims wait on a person.
    pub fn turn_ended(&mut self, seat: SeatId, now: u64) {
        if let Some(s) = self.seats.get_mut(&seat) {
            s.waiting_since = Some(now);
        }
    }

    /// A prompt: the person answered; the seat is no longer waiting.
    pub fn prompted(&mut self, seat: SeatId) {
        if let Some(s) = self.seats.get_mut(&seat) {
            s.waiting_since = None;
        }
    }

    /// SessionEnd: release everything the seat holds, and drop it.
    pub fn ended(&mut self, seat: SeatId) {
        let held: Vec<Key> = self
            .claims
            .iter()
            .filter(|(_, c)| c.owner() == Some(seat))
            .map(|(k, _)| k.clone())
            .collect();
        for k in held {
            let freed = claim::taint_free(self.claims.get(&k).copied(), seat);
            self.store(&k, freed);
        }
        self.queued.retain(|q| q.seat != seat);
        self.seats.remove(&seat);
    }

    /// An edit by `seat` to `path`: add it to the footprint, and return the
    /// other present seats that edited it too, once per path.
    pub fn footprint(&mut self, seat: SeatId, path: &str, now: u64) -> Vec<SeatId> {
        if !path_ok(path) {
            return Vec::new();
        }
        let present = self.present(now);
        let others: Vec<SeatId> = self
            .seats
            .iter()
            .filter(|(id, s)| {
                **id != seat && present.contains(id) && s.paths.iter().any(|p| p == path)
            })
            .map(|(id, _)| *id)
            .collect();
        let max = usize::from(cfg().get().max_paths);
        let s = self.seats.entry(seat).or_default();
        s.paths.retain(|p| p != path);
        s.paths.push(path.to_string());
        newest(&mut s.paths, max);
        if others.is_empty() || s.noticed.iter().any(|p| p == path) {
            return Vec::new();
        }
        s.noticed.push(path.to_string());
        newest(&mut s.noticed, max);
        others
    }

    /// The news this seat has not been told, at most 400 characters, and
    /// mark it told.
    pub fn news_for(&mut self, seat: SeatId) -> Vec<String> {
        let seen = self.seats.get(&seat).map_or(self.seq, |s| s.seen);
        let mut out = Vec::new();
        let mut used = 0usize;
        for item in self.news.iter().filter(|n| n.seq > seen) {
            let line = match &item.news {
                News::Taken { key, from, by } if *from == seat => format!(
                    "Your claim on {} lapsed and {by} took it over. Stop work on it, or claim it again if it frees.",
                    key.as_str()
                ),
                News::Taken { key, by, .. } if *by != seat => {
                    format!("{by} took over {} after its lease ran out.", key.as_str())
                }
                News::Closed { key } => format!("{} is done.", key.as_str()),
                News::Main { sha } => format!("Main moved to {sha}."),
                News::File { path } => {
                    format!("{path} changed; read it if your work depends on it.")
                }
                News::QueuedFailed { key, seat: s } if *s == seat => {
                    format!(
                        "Your claim on {}, queued during the pause, did not go through.",
                        key.as_str()
                    )
                }
                News::Taken { .. } | News::QueuedFailed { .. } => continue,
            };
            let next = used.saturating_add(line.len()).saturating_add(1);
            if next > NEWS_CHARS {
                break;
            }
            used = next;
            out.push(line);
        }
        let seq = self.seq;
        if let Some(s) = self.seats.get_mut(&seat) {
            s.seen = seq;
        }
        out
    }

    /// `heddle pause`: 15 minutes; renewals extend it to at most 60 minutes
    /// from when it was first set; not again for 15 minutes after it lapses.
    pub fn pause_now(&mut self, now: u64) -> Result<u64, u64> {
        match self.pause {
            Some(p) if p.until > now => {
                let until = now
                    .saturating_add(PAUSE_FOR)
                    .min(p.first_at.saturating_add(PAUSE_MAX))
                    .max(p.until);
                self.pause = Some(Pause { until, ..p });
                Ok(until)
            }
            Some(_) | None if self.cooldown_until > now => Err(self.cooldown_until),
            Some(_) | None => {
                let until = now.saturating_add(PAUSE_FOR);
                self.pause = Some(Pause {
                    first_at: now,
                    until,
                    stop_until: None,
                });
                Ok(until)
            }
        }
    }

    /// `heddle stop`, on a person's request: a pause, plus pushes to main are
    /// refused for 30 minutes; then it is a pause until that lapses.
    pub fn stop_now(&mut self, now: u64) -> u64 {
        let stop_until = now.saturating_add(STOP_FOR);
        let until = stop_until.saturating_add(PAUSE_FOR);
        let first_at = self.pause.map_or(now, |p| p.first_at);
        let until = self.pause.map_or(until, |p| p.until.max(until));
        self.pause = Some(Pause {
            first_at,
            until,
            stop_until: Some(stop_until),
        });
        stop_until
    }

    /// `heddle resume`: ends a pause or a stop now. Queued work runs at the
    /// next request.
    pub fn resume_now(&mut self) {
        self.pause = None;
    }

    /// The decisions watch: main's commit and the watched files' stamps,
    /// read by the caller. News for what changed since the last look; the
    /// first look only records.
    pub fn watch(&mut self, now: u64, main: Option<String>, files: Vec<(String, String)>) {
        if now.saturating_sub(self.watch_checked) < WATCH_EVERY && self.watch_checked != 0 {
            return;
        }
        let first = self.watch_checked == 0;
        self.watch_checked = now;
        if let Some(m) = main {
            if !first && self.main.as_deref() != Some(m.as_str()) {
                let short: String = m.chars().take(7).collect();
                self.push(News::Main { sha: short });
            }
            self.main = Some(m);
        }
        for (path, stamp) in files {
            let changed = self.watched.get(&path).is_some_and(|old| *old != stamp)
                || (!first && !self.watched.contains_key(&path));
            if changed {
                self.push(News::File { path: path.clone() });
            }
            self.watched.insert(path, stamp);
        }
    }

    // ---- the file ----

    /// The room as text.
    pub fn encode(&self) -> String {
        let mut o = String::from(FORMAT);
        o.push('\n');
        let line = |o: &mut String, s: String| {
            o.push_str(&s);
            o.push('\n');
        };
        line(
            &mut o,
            format!(
                "clock {} {}",
                self.clocks.last_now.0, self.clocks.lease_clock.0
            ),
        );
        line(&mut o, format!("epoch {}", self.epoch));
        line(&mut o, format!("seq {}", self.seq));
        line(&mut o, format!("cooldown {}", self.cooldown_until));
        line(&mut o, format!("watched {}", self.watch_checked));
        if let Some(m) = &self.main {
            line(&mut o, format!("main {m}"));
        }
        if let Some(p) = self.pause {
            let stop = p.stop_until.map_or("-".to_string(), |s| s.to_string());
            line(&mut o, format!("pause {} {} {stop}", p.first_at, p.until));
        }
        for (id, s) in &self.seats {
            let w = s.waiting_since.map_or("-".to_string(), |w| w.to_string());
            line(&mut o, format!("seat {id} {} {w} {}", s.last_seen, s.seen));
            for p in &s.paths {
                line(&mut o, format!("path {id} {p}"));
            }
            for p in &s.noticed {
                line(&mut o, format!("noticed {id} {p}"));
            }
        }
        for (k, c) in &self.claims {
            let k = k.as_str();
            match c {
                Claim::Held {
                    owner,
                    epoch,
                    expires,
                } => line(
                    &mut o,
                    format!("claim {k} held {owner} {} {}", epoch.get(), expires.0),
                ),
                Claim::Closing {
                    owner,
                    epoch,
                    deadline,
                    resume_expires,
                } => line(
                    &mut o,
                    format!(
                        "claim {k} closing {owner} {} {} {}",
                        epoch.get(),
                        deadline.0,
                        resume_expires.0
                    ),
                ),
                Claim::Closed { how, sha, at } => {
                    let h = match how {
                        Closure::Done => "done",
                        Closure::Dropped => "dropped",
                    };
                    line(&mut o, format!("claim {k} closed {h} {sha} {}", at.0));
                }
            }
        }
        for n in &self.news {
            let body = match &n.news {
                News::Taken { key, from, by } => format!("taken {} {from} {by}", key.as_str()),
                News::Closed { key } => format!("closed {}", key.as_str()),
                News::Main { sha } => format!("main {sha}"),
                News::File { path } => format!("file {path}"),
                News::QueuedFailed { key, seat } => format!("queuedfailed {} {seat}", key.as_str()),
            };
            line(&mut o, format!("news {} {body}", n.seq));
        }
        for q in &self.queued {
            let d = q.done.map_or("-".to_string(), |s| s.to_string());
            line(&mut o, format!("queued {} {} {d}", q.seat, q.key.as_str()));
        }
        for (p, stamp) in &self.watched {
            line(&mut o, format!("watch {stamp} {p}"));
        }
        o
    }

    /// The room from text. A wrong header is an empty room; a line that
    /// does not parse is dropped.
    pub fn decode(text: &str) -> Room {
        let mut lines = text.lines();
        let mut r = Room::default();
        if lines.next() != Some(FORMAT) {
            return r;
        }
        for l in lines {
            let _ = r.read_line(l);
        }
        r
    }

    fn read_line(&mut self, l: &str) -> Option<()> {
        let (tag, rest) = l.split_once(' ')?;
        let num = |s: &str| s.parse::<u64>().ok();
        let seat = |s: &str| SeatId::parse(s.as_bytes());
        let mut w = rest.split(' ');
        match tag {
            "clock" => {
                self.clocks = Clocks {
                    last_now: RoomTime(num(w.next()?)?),
                    lease_clock: LeaseTime(num(w.next()?)?),
                };
            }
            "epoch" => self.epoch = num(rest)?,
            "seq" => self.seq = num(rest)?,
            "cooldown" => self.cooldown_until = num(rest)?,
            "watched" => self.watch_checked = num(rest)?,
            "main" => {
                self.main = rest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                    .then(|| rest.to_string())
            }
            "pause" => {
                let (a, b, c) = (num(w.next()?)?, num(w.next()?)?, w.next()?);
                self.pause = Some(Pause {
                    first_at: a,
                    until: b,
                    stop_until: num(c),
                });
            }
            "seat" => {
                let id = seat(w.next()?)?;
                let last_seen = num(w.next()?)?;
                let waiting_since = num(w.next()?);
                let seen = num(w.next()?)?;
                self.seats.insert(
                    id,
                    Seat {
                        last_seen,
                        waiting_since,
                        seen,
                        ..Seat::default()
                    },
                );
            }
            "path" | "noticed" => {
                let (id, p) = rest.split_once(' ')?;
                let s = self.seats.get_mut(&seat(id)?)?;
                if path_ok(p) {
                    if tag == "path" {
                        s.paths.push(p.to_string())
                    } else {
                        s.noticed.push(p.to_string())
                    }
                }
            }
            "claim" => {
                let key = Key::parse(w.next()?)?;
                let c = match w.next()? {
                    "held" => Claim::Held {
                        owner: seat(w.next()?)?,
                        epoch: Epoch::new(num(w.next()?)?)?,
                        expires: LeaseTime(num(w.next()?)?),
                    },
                    "closing" => Claim::Closing {
                        owner: seat(w.next()?)?,
                        epoch: Epoch::new(num(w.next()?)?)?,
                        deadline: LeaseTime(num(w.next()?)?),
                        resume_expires: LeaseTime(num(w.next()?)?),
                    },
                    "closed" => Claim::Closed {
                        how: match w.next()? {
                            "done" => Closure::Done,
                            "dropped" => Closure::Dropped,
                            _ => return None,
                        },
                        sha: Sha::parse(w.next()?.as_bytes())?,
                        at: RoomTime(num(w.next()?)?),
                    },
                    _ => return None,
                };
                self.claims.insert(key, c);
            }
            "news" => {
                let seq = num(w.next()?)?;
                let news = match w.next()? {
                    "taken" => News::Taken {
                        key: Key::parse(w.next()?)?,
                        from: seat(w.next()?)?,
                        by: seat(w.next()?)?,
                    },
                    "closed" => News::Closed {
                        key: Key::parse(w.next()?)?,
                    },
                    "main" => {
                        let sha = w.next()?;
                        sha.bytes().all(|b| b.is_ascii_hexdigit()).then_some(())?;
                        News::Main {
                            sha: sha.to_string(),
                        }
                    }
                    "file" => {
                        let p = rest.splitn(3, ' ').nth(2)?;
                        (p.len() <= 400 && p.bytes().all(|b| (0x20..0x7f).contains(&b)))
                            .then_some(())?;
                        News::File {
                            path: p.to_string(),
                        }
                    }
                    "queuedfailed" => News::QueuedFailed {
                        key: Key::parse(w.next()?)?,
                        seat: seat(w.next()?)?,
                    },
                    _ => return None,
                };
                self.news.push(NewsItem { seq, news });
            }
            "queued" => {
                let s = seat(w.next()?)?;
                let key = Key::parse(w.next()?)?;
                let d = w.next()?;
                let done = if d == "-" {
                    None
                } else {
                    Some(Sha::parse(d.as_bytes())?)
                };
                self.queued.push(Queued { seat: s, key, done });
            }
            "watch" => {
                let (stamp, p) = rest.split_once(' ')?;
                if p.len() <= 400 && p.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                    self.watched.insert(p.to_string(), stamp.to_string());
                }
            }
            _ => return None,
        }
        Some(())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    fn s(n: u8) -> SeatId {
        SeatId::parse(format!("s-0000000{n}").as_bytes()).expect("seat")
    }
    fn k(x: &str) -> Key {
        Key::parse(x).expect("key")
    }
    fn sha(c: char) -> Sha {
        Sha::parse(c.to_string().repeat(40).as_bytes()).expect("sha")
    }
    const T0: u64 = 1_800_000_000_000;

    #[test]
    fn one_winner_then_renewal_then_a_takeover_after_lease_and_grace() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.see(s(2), T0);
        assert!(matches!(
            r.claim(s(1), &k("fix-login"), T0),
            Outcome::Ok(ClaimReply::Granted(_))
        ));
        assert_eq!(r.claim(s(2), &k("fix-login"), T0), Outcome::HeldBy(s(1)));
        assert_eq!(
            r.claim(s(1), &k("fix-login"), T0),
            Outcome::Ok(ClaimReply::AlreadyYours)
        );
        // 31 minutes of silence from s1, two minutes at a time (the lease clock's cap).
        let mut now = T0;
        for _ in 0..16 {
            now += 2 * MINUTE;
            r.tick(now);
            r.see(s(2), now);
        }
        assert_eq!(r.phase(&k("fix-login")), Phase::Takeable);
        assert!(matches!(
            r.claim(s(2), &k("fix-login"), now),
            Outcome::Ok(ClaimReply::Granted(_))
        ));
        // The old owner is told once; nobody else hears it twice.
        r.see(s(1), now);
        let told = r.news_for(s(1));
        assert!(
            told.iter()
                .any(|l| l.starts_with("Your claim on fix-login lapsed")),
            "{told:?}"
        );
        assert!(r.news_for(s(1)).is_empty());
    }

    #[test]
    fn a_claim_waiting_on_a_person_is_not_taken_over() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.claim(s(1), &k("a"), T0);
        r.turn_ended(s(1), T0);
        let mut now = T0;
        for _ in 0..30 {
            now += 2 * MINUTE;
            r.tick(now);
        }
        assert_eq!(r.claim(s(2), &k("a"), now), Outcome::Waiting(s(1)));
        // The person answers; the seat's next hook renews; it is held as usual.
        r.prompted(s(1));
        r.see(s(1), now);
        assert_eq!(r.claim(s(2), &k("a"), now), Outcome::HeldBy(s(1)));
        // SessionEnd releases it.
        r.ended(s(1));
        assert!(matches!(
            r.claim(s(2), &k("a"), now),
            Outcome::Ok(ClaimReply::Granted(_))
        ));
    }

    #[test]
    fn a_pause_queues_claims_extends_to_an_hour_and_has_a_cooldown() {
        let mut r = Room::default();
        r.tick(T0);
        assert_eq!(r.pause_now(T0), Ok(T0 + 15 * MINUTE));
        assert_eq!(
            r.claim(s(1), &k("b"), T0 + MINUTE),
            Outcome::Queued {
                until: T0 + 15 * MINUTE
            }
        );
        // Renewals stretch it, to at most an hour from the first.
        assert_eq!(r.pause_now(T0 + 10 * MINUTE), Ok(T0 + 25 * MINUTE));
        assert_eq!(r.pause_now(T0 + 24 * MINUTE), Ok(T0 + 39 * MINUTE));
        assert_eq!(r.pause_now(T0 + 38 * MINUTE), Ok(T0 + 53 * MINUTE));
        assert_eq!(r.pause_now(T0 + 52 * MINUTE), Ok(T0 + 60 * MINUTE));
        let until = T0 + 60 * MINUTE;
        // It lapses; the queued claim completes by itself; a new pause waits 15 minutes.
        r.tick(until);
        assert!(matches!(r.claims.get(&k("b")), Some(Claim::Held { .. })));
        assert_eq!(r.pause_now(until + MINUTE), Err(until + 15 * MINUTE));
        assert!(r.pause_now(until + 16 * MINUTE).is_ok());
    }

    #[test]
    fn a_stop_refuses_pushes_for_30_minutes_then_is_a_pause_and_resume_ends_it() {
        let mut r = Room::default();
        r.tick(T0);
        assert_eq!(r.stop_now(T0), T0 + 30 * MINUTE);
        assert!(matches!(r.level(T0 + MINUTE), Level::Stopped { .. }));
        assert!(matches!(r.level(T0 + 31 * MINUTE), Level::Paused { .. }));
        r.resume_now();
        assert_eq!(r.level(T0 + 31 * MINUTE), Level::Running);
    }

    #[test]
    fn done_closes_only_with_a_commit_on_main() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.claim(s(1), &k("c"), T0);
        assert_eq!(r.done(s(1), &k("c"), None, T0), Outcome::Closing);
        assert!(matches!(
            r.done(s(1), &k("c"), Some(sha('a')), T0),
            Outcome::Ok(_)
        ));
        assert_eq!(r.phase(&k("c")), Phase::Closed);
        assert_eq!(
            r.claim(s(2), &k("c"), T0),
            Outcome::Refused(ClaimRefusal::Closed)
        );
        r.see(s(2), T0);
        assert_eq!(
            r.news_for(s(2)),
            Vec::<String>::new(),
            "a seat joining late is told only what came after"
        );
    }

    #[test]
    fn the_overlap_notice_goes_once_per_path_to_the_later_editor() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.see(s(2), T0);
        assert!(r.footprint(s(1), "src/a.rs", T0).is_empty());
        assert_eq!(r.footprint(s(2), "src/a.rs", T0), vec![s(1)]);
        assert!(
            r.footprint(s(2), "src/a.rs", T0).is_empty(),
            "once per path"
        );
        assert!(r.footprint(s(2), "/an/absolute/path", T0).is_empty());
    }

    #[test]
    fn the_watch_records_first_then_tells_what_changed() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.watch(
            T0,
            Some("a".repeat(40)),
            vec![("DECISIONS.md".into(), "1:10".into())],
        );
        assert!(r.news_for(s(1)).is_empty());
        r.watch(T0 + 10_000, Some("b".repeat(40)), vec![]);
        assert!(r.news_for(s(1)).is_empty(), "at most every 30 seconds");
        r.watch(
            T0 + 31_000,
            Some("b".repeat(40)),
            vec![("DECISIONS.md".into(), "2:11".into())],
        );
        let n = r.news_for(s(1));
        assert_eq!(
            n,
            vec![
                "Main moved to bbbbbbb.".to_string(),
                "DECISIONS.md changed; read it if your work depends on it.".to_string()
            ]
        );
    }

    #[test]
    fn the_file_round_trips_and_garbage_is_dropped() {
        let mut r = Room::default();
        r.tick(T0);
        r.see(s(1), T0);
        r.claim(s(1), &k("d"), T0);
        r.claim(s(1), &k("e"), T0);
        r.done(s(1), &k("e"), Some(sha('c')), T0);
        r.footprint(s(1), "x y.md", T0);
        let _ = r.pause_now(T0);
        r.claim(s(2), &k("f"), T0);
        r.watch(
            T0,
            Some("c".repeat(40)),
            vec![("fabric/records/r 1.md".into(), "3:4".into())],
        );
        assert_eq!(Room::decode(&r.encode()), r);
        assert_eq!(Room::decode("not a room"), Room::default());
        let damaged = r.encode().replace("claim d held", "claim D held")
            + "nonsense line\nseat s-bad 1 - 0\n";
        let back = Room::decode(&damaged);
        assert!(!back.claims.contains_key(&k("d")) && back.claims.contains_key(&k("e")));
    }

    #[test]
    fn keys_and_seats_have_their_grammar() {
        assert!(Key::parse("fix-login-2").is_some());
        for bad in ["", "-x", "Fix", "a b", "a/b", &"x".repeat(41)] {
            assert!(Key::parse(bad).is_none(), "{bad}");
        }
        let conv = ConvId::parse(b"00893aaf-19fa-41d2-8238-13269b9b3ca0").expect("conv");
        assert_eq!(
            seat_of(&conv).map(|s| s.to_string()),
            Some("s-00893aaf".to_string())
        );
    }
}
