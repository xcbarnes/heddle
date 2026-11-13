//! M5: pause, stop and resume. Four fixed slots and one scope, everyone.
//!
//! An agent's unconfirmed stop counts as Stopped for `STOP_DECAY` after the
//! *first* one, and as a pause after that (decision A3, a default pending
//! Xavier's confirmation). A person's marks never decay. Only an admin-signed
//! Resume clears a slot, and only up to the sequence number the person saw.

use crate::config::Config;
use crate::time::RoomTime;

/// The room's change counter (E10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// The level, ordered `Running < Paused < Stopped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Work goes ahead.
    Running,
    /// No new claims and no `done`; tools still run.
    Paused,
    /// Only read-only tools and the listed heddle verbs.
    Stopped,
}

/// What a `Restrict` event asks for. Any token may send one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestrictLevel {
    /// An agent's pause.
    Paused,
    /// An agent's stop.
    Stopped,
}

/// One occupied slot. `seq` is the room's `seq` after the step that set or
/// last touched it; `first_at` is when it was first set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The room's sequence number after the step that set or last touched it.
    pub seq: Seq,
    /// When the slot went from empty to set.
    pub first_at: RoomTime,
}

/// The four slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restrictions {
    /// An agent's (or the everyday terminal's) pause.
    pub agent_pause: Option<Mark>,
    /// An agent's (or the everyday terminal's) stop. Decays to a pause.
    pub agent_stop: Option<Mark>,
    /// A person's pause, from the admin user.
    pub person_pause: Option<Mark>,
    /// A person's stop, from the admin user. Never decays.
    pub person_stop: Option<Mark>,
}

/// Which slot an event sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// `Restrict(Paused)`.
    AgentPause,
    /// `Restrict(Stopped)`.
    AgentStop,
    /// `Admin Pause`.
    PersonPause,
    /// `Admin Stop`.
    PersonStop,
}

impl Slot {
    /// The slot a `Restrict` event from any token sets.
    pub const fn of_agent(l: RestrictLevel) -> Slot {
        match l {
            RestrictLevel::Paused => Slot::AgentPause,
            RestrictLevel::Stopped => Slot::AgentStop,
        }
    }
}

/// The level. `last_now` is already clamped (K5), and the subtraction
/// saturates, so this cannot underflow.
pub fn level(r: &Restrictions, last_now: RoomTime, cfg: &Config) -> Level {
    let agent_stop_live = match r.agent_stop {
        Some(m) => last_now.0.saturating_sub(m.first_at.0) < cfg.get().stop_decay,
        None => false,
    };
    if r.person_stop.is_some() || agent_stop_live {
        Level::Stopped
    } else if r.agent_pause.is_some() || r.agent_stop.is_some() || r.person_pause.is_some() {
        Level::Paused
    } else {
        Level::Running
    }
}

/// Set a slot, or touch it again. A repeat takes the new `seq` and keeps
/// `first_at`, so repeating a stop cannot restart its decay (P9).
pub fn mark(r: Restrictions, slot: Slot, seq: Seq, now: RoomTime) -> Restrictions {
    let touch = |old: Option<Mark>| {
        Some(match old {
            Some(m) => Mark {
                seq,
                first_at: m.first_at,
            },
            None => Mark { seq, first_at: now },
        })
    };
    match slot {
        Slot::AgentPause => Restrictions {
            agent_pause: touch(r.agent_pause),
            ..r
        },
        Slot::AgentStop => Restrictions {
            agent_stop: touch(r.agent_stop),
            ..r
        },
        Slot::PersonPause => Restrictions {
            person_pause: touch(r.person_pause),
            ..r
        },
        Slot::PersonStop => Restrictions {
            person_stop: touch(r.person_stop),
            ..r
        },
    }
}

fn merge(into: Option<Mark>, m: Mark) -> Mark {
    match into {
        Some(p) => Mark {
            seq: p.seq.max(m.seq),
            first_at: p.first_at.min(m.first_at),
        },
        None => m,
    }
}

/// `Admin Confirm{seq}`: the agent mark with that `seq` moves to the matching
/// person slot, merging with it if it is occupied. `None` is `NoSuchMark`.
pub fn confirm(r: Restrictions, seq: Seq) -> Option<Restrictions> {
    if let Some(m) = r.agent_pause.filter(|m| m.seq == seq) {
        return Some(Restrictions {
            agent_pause: None,
            person_pause: Some(merge(r.person_pause, m)),
            ..r
        });
    }
    if let Some(m) = r.agent_stop.filter(|m| m.seq == seq) {
        return Some(Restrictions {
            agent_stop: None,
            person_stop: Some(merge(r.person_stop, m)),
            ..r
        });
    }
    None
}

/// `Admin Resume{upto}`: clears every slot whose `seq <= upto` (P1, P8).
pub fn resume(r: Restrictions, upto: Seq) -> Restrictions {
    let keep = |s: Option<Mark>| s.filter(|m| m.seq > upto);
    Restrictions {
        agent_pause: keep(r.agent_pause),
        agent_stop: keep(r.agent_stop),
        person_pause: keep(r.person_pause),
        person_stop: keep(r.person_stop),
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const CFG: Config = Config::MODEL;

    #[test]
    fn p3_an_agent_stop_decays_to_a_pause_and_a_person_stop_never_does() {
        let decay = CFG.get().stop_decay;
        let agent = mark(
            Restrictions::default(),
            Slot::AgentStop,
            Seq(1),
            RoomTime(10),
        );
        let person = mark(
            Restrictions::default(),
            Slot::PersonStop,
            Seq(1),
            RoomTime(10),
        );
        for t in 10..10 + 3 * decay {
            let want = if t - 10 < decay {
                Level::Stopped
            } else {
                Level::Paused
            };
            assert_eq!(level(&agent, RoomTime(t), &CFG), want);
            assert_eq!(level(&person, RoomTime(t), &CFG), Level::Stopped);
        }
    }

    #[test]
    fn p9_repeating_a_stop_cannot_restart_the_decay() {
        let decay = CFG.get().stop_decay;
        let mut r = Restrictions::default();
        for t in 0..4 * decay {
            r = mark(r, Slot::AgentStop, Seq(t + 1), RoomTime(t));
            let want = if t < decay {
                Level::Stopped
            } else {
                Level::Paused
            };
            assert_eq!(level(&r, RoomTime(t), &CFG), want);
            assert_eq!(r.agent_stop.map(|m| m.first_at), Some(RoomTime(0)));
        }
    }

    #[test]
    fn p8_resume_never_clears_a_mark_set_after_the_person_looked() {
        let r = mark(
            Restrictions::default(),
            Slot::AgentPause,
            Seq(3),
            RoomTime(0),
        );
        let r = mark(r, Slot::PersonStop, Seq(5), RoomTime(1));
        let after = resume(r, Seq(4));
        assert_eq!(after.agent_pause, None);
        assert_eq!(
            after.person_stop,
            Some(Mark {
                seq: Seq(5),
                first_at: RoomTime(1)
            })
        );
        assert_eq!(resume(after, Seq(5)), Restrictions::default());
    }

    #[test]
    fn confirm_moves_and_merges_and_refuses_an_unknown_seq() {
        let r = mark(
            Restrictions::default(),
            Slot::AgentStop,
            Seq(2),
            RoomTime(7),
        );
        let r = mark(r, Slot::PersonStop, Seq(4), RoomTime(9));
        let c = confirm(r, Seq(2));
        let want = Restrictions {
            person_stop: Some(Mark {
                seq: Seq(4),
                first_at: RoomTime(7),
            }),
            ..Restrictions::default()
        };
        assert_eq!(c, Some(want));
        assert_eq!(confirm(r, Seq(3)), None);
        // A confirmed stop no longer decays.
        assert_eq!(
            c.map(|x| level(&x, RoomTime(1_000), &CFG)),
            Some(Level::Stopped)
        );
    }
}
