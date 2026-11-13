//! M6: the local snapshot's age, and the high-water mark that stops an old
//! signed snapshot being put back.
//!
//! Age is the larger of the wall-clock difference and the difference on a
//! monotonic clock that keeps counting during sleep, so a constant clock error
//! changes nothing; a negative difference counts as too old. The sticky level
//! is the level in the newest verified snapshot, and an outage never lowers it
//! (P7).

use crate::restrict::Level;
use crate::time::LocalNow;

/// Under this age, in milliseconds, a snapshot is Fresh.
pub const FRESH: u64 = 3_000;
/// Over this age a snapshot is never shown as current state (S4).
pub const EVIDENCE_MAX: u64 = 600_000;
/// After a tool, a hook syncs when the snapshot is older than this.
pub const NEWS_REFRESH: u64 = 30_000;

/// What a hook may do with the stored snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapState {
    /// None stored, or an unknown format.
    Missing,
    /// Verified, younger than `FRESH`.
    Fresh,
    /// Verified, from `FRESH` to `EVIDENCE_MAX`.
    Aging,
    /// Verified, older than `EVIDENCE_MAX`, or its age cannot be computed.
    TooOld,
}

/// The age of a snapshot received at `got`. `None` if either clock went back.
pub fn age_ms(got: LocalNow, now: LocalNow) -> Option<u64> {
    let wall = now.wall_ms.checked_sub(got.wall_ms)?;
    let mono = now.mono_ms.checked_sub(got.mono_ms)?;
    Some(wall.max(mono))
}

/// The state of the stored snapshot, by when it was received.
pub fn snap_state(got: Option<LocalNow>, now: LocalNow) -> SnapState {
    match got {
        None => SnapState::Missing,
        Some(g) => match age_ms(g, now) {
            None => SnapState::TooOld,
            Some(a) if a < FRESH => SnapState::Fresh,
            Some(a) if a <= EVIDENCE_MAX => SnapState::Aging,
            Some(_) => SnapState::TooOld,
        },
    }
}

/// The `trust/high` file: what this repo's laptops have verified so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trust {
    /// The room's generation; a restore bumps it (SH5).
    pub generation: u32,
    /// The highest `seq` verified in that generation.
    pub high_seq: u64,
    /// The level in the newest verified snapshot.
    pub sticky: Level,
    /// The highest asset-registry version seen.
    pub registry_seen: u32,
}

impl Trust {
    /// Before any snapshot.
    pub const START: Trust = Trust { generation: 0, high_seq: 0, sticky: Level::Running, registry_seen: 0 };
}

/// Whether a snapshot at `(generation, seq)` may be accepted (S2): it is not
/// below the high-water mark.
pub fn not_older(t: &Trust, generation: u32, seq: u64) -> bool {
    (generation, seq) >= (t.generation, t.high_seq)
}

/// Fold a verified snapshot into the high-water mark. Only a higher
/// `(generation, seq)`, or an equal one from a live, nonce-checked answer,
/// moves the sticky level; at equal `seq` it can only have fallen by decay
/// (E10), and only a live answer may say so.
pub fn observe(t: Trust, generation: u32, seq: u64, level: Level, live: bool) -> Trust {
    let key = (generation, seq);
    let have = (t.generation, t.high_seq);
    if key > have || (key == have && live) {
        Trust { generation, high_seq: seq, sticky: level, registry_seen: t.registry_seen }
    } else {
        t
    }
}

/// Two writers' marks combine as a join (L1): whichever saw the newer room
/// state wins the room fields, and at the same state the stricter level wins,
/// so a join never lowers the level (P7). The registry version is a maximum.
pub fn join(a: Trust, b: Trust) -> Trust {
    let (ka, kb) = ((a.generation, a.high_seq), (b.generation, b.high_seq));
    let base = if kb > ka {
        b
    } else if ka > kb {
        a
    } else {
        Trust { sticky: a.sticky.max(b.sticky), ..a }
    };
    Trust { registry_seen: a.registry_seen.max(b.registry_seen), ..base }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn at(wall_ms: u64, mono_ms: u64) -> LocalNow {
        LocalNow { wall_ms, mono_ms }
    }

    #[test]
    fn age_takes_the_larger_clock_and_a_clock_that_went_back_is_too_old() {
        assert_eq!(age_ms(at(100, 100), at(200, 150)), Some(100));
        assert_eq!(age_ms(at(100, 100), at(150, 700)), Some(600), "a sleep shows on the monotonic clock");
        assert_eq!(snap_state(Some(at(100, 100)), at(50, 200)), SnapState::TooOld, "wall clock stepped back");
        assert_eq!(snap_state(Some(at(100, 100)), at(200, 50)), SnapState::TooOld, "a reboot reset the monotonic clock");
        assert_eq!(snap_state(None, at(0, 0)), SnapState::Missing);
    }

    #[test]
    fn s4_the_bands_are_fresh_aging_and_too_old() {
        let g = at(0, 0);
        assert_eq!(snap_state(Some(g), at(FRESH - 1, 0)), SnapState::Fresh);
        assert_eq!(snap_state(Some(g), at(FRESH, 0)), SnapState::Aging);
        assert_eq!(snap_state(Some(g), at(EVIDENCE_MAX, 0)), SnapState::Aging);
        assert_eq!(snap_state(Some(g), at(EVIDENCE_MAX + 1, 0)), SnapState::TooOld);
    }

    #[test]
    fn p7_s2_an_old_or_equal_cached_snapshot_never_lowers_the_sticky_level() {
        let t = observe(Trust::START, 1, 10, Level::Stopped, false);
        assert_eq!(observe(t, 1, 9, Level::Running, true).sticky, Level::Stopped);
        assert_eq!(observe(t, 1, 10, Level::Running, false).sticky, Level::Stopped);
        assert_eq!(observe(t, 1, 10, Level::Paused, true).sticky, Level::Paused, "decay, said live");
        assert_eq!(observe(t, 2, 0, Level::Running, false).sticky, Level::Running, "a restore bumps the generation");
        assert!(!not_older(&t, 1, 9));
        assert!(not_older(&t, 2, 0));
    }

    #[test]
    fn l1_the_join_of_two_writes_keeps_the_newer_room_state() {
        let a = Trust { generation: 1, high_seq: 5, sticky: Level::Stopped, registry_seen: 3 };
        let b = Trust { generation: 1, high_seq: 4, sticky: Level::Running, registry_seen: 7 };
        let j = join(a, b);
        assert_eq!((j.high_seq, j.sticky, j.registry_seen), (5, Level::Stopped, 7));
        assert_eq!(join(a, b), join(b, a));
    }
}
