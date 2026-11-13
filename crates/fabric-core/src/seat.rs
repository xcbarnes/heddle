//! M12: seat and session identity, the laptop side.
//!
//! A seat holds claims: one worktree on one machine. A conversation is one
//! harness session. The last conversation to start in a worktree, or the last
//! one a person typed into, is the writer and may change claims; a fork never
//! may (X5). Other conversations are observers: they may read and edit, but
//! not change claims (X6). There is no process test and no timer here.

use crate::ids::{ConvId, SeatId};

/// How many forks a binding remembers. Past that, the oldest is dropped, so X5
/// holds for the latest eight forks of a worktree.
pub const MAX_FORKS: usize = 8;

/// The `binding` file of one worktree. Read and written once per hook, so the
/// size of the `Bound` variant does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// No seat yet.
    NoSeat,
    /// A seat.
    Bound {
        /// The seat.
        seat: SeatId,
        /// The writer. `None` while only forks have started here.
        conv: Option<ConvId>,
        /// How many times the writer changed.
        n: u32,
        /// The room has accepted `SeatOpen`.
        opened: bool,
        /// Join-blocked: the room refused `SeatOpen` with `TokenBraked`. This
        /// is not a PHI label (X8): this worktree's own thread has not fired.
        blocked: bool,
        /// Conversations that started as forks.
        forks: [Option<ConvId>; MAX_FORKS],
    },
}

/// What a conversation may do with claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// May change claims.
    Writer,
    /// May read and edit, not change claims.
    Observer,
}

/// SessionStart's `source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A new session.
    Startup,
    /// `--resume`, `--continue`, `/resume`.
    Resume,
    /// `/clear`.
    Clear,
    /// A compaction. It never moves the seat.
    Compact,
    /// A fork. Forks hold no claims.
    Fork,
}

/// A hook from a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatEvent {
    /// SessionStart.
    Start {
        /// How the session started.
        source: Source,
        /// The conversation.
        conv: ConvId,
    },
    /// UserPromptSubmit: a person typed into this conversation.
    PersonPrompt {
        /// The conversation.
        conv: ConvId,
    },
    /// Any other hook. A subagent's hook carries its parent's `session_id`
    /// and acts as its parent.
    Hook {
        /// The conversation.
        conv: ConvId,
    },
}

/// The room's answer to `SeatOpen`, or a sign the room lost the seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomAnswer {
    /// `SeatOpen` was accepted.
    Opened,
    /// The room does not know the seat: send `SeatOpen` again.
    NoSuchSeat,
    /// Another seat of this machine's token is Exposed.
    TokenBraked,
}

/// What the edge must do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeatEffects {
    /// A seat was minted from the fresh id; store its new secret.
    pub minted: bool,
    /// Send `SeatOpen` (again).
    pub open_seat: bool,
    /// Say once: forks hold no claims.
    pub fork_line: bool,
    /// Say once: another conversation holds this worktree's claims.
    pub observer_line: bool,
}

const NONE: SeatEffects = SeatEffects {
    minted: false,
    open_seat: false,
    fork_line: false,
    observer_line: false,
};

fn is_fork(forks: &[Option<ConvId>; MAX_FORKS], conv: ConvId) -> bool {
    forks.contains(&Some(conv))
}

fn add_fork(forks: [Option<ConvId>; MAX_FORKS], conv: ConvId) -> [Option<ConvId>; MAX_FORKS] {
    if is_fork(&forks, conv) {
        return forks;
    }
    if let Some(slot) = forks.iter().position(Option::is_none) {
        let mut out = forks;
        if let Some(s) = out.get_mut(slot) {
            *s = Some(conv);
        }
        return out;
    }
    // Full: drop the oldest, keep order.
    let mut out = [None; MAX_FORKS];
    let mut src = forks
        .iter()
        .skip(1)
        .copied()
        .chain(core::iter::once(Some(conv)));
    for o in out.iter_mut() {
        *o = src.next().flatten();
    }
    out
}

fn mint(seat: SeatId, conv: Option<ConvId>, forks: [Option<ConvId>; MAX_FORKS]) -> Binding {
    Binding::Bound {
        seat,
        conv,
        n: if conv.is_some() { 1 } else { 0 },
        opened: false,
        blocked: false,
        forks,
    }
}

/// M12 for a hook from a conversation. `fresh` is a random seat id minted at
/// the edge; it is used only when the worktree has no seat.
pub fn seat_step(b: Binding, ev: SeatEvent, fresh: SeatId) -> (Binding, Role, SeatEffects) {
    let conv = match ev {
        SeatEvent::Start { conv, .. }
        | SeatEvent::PersonPrompt { conv }
        | SeatEvent::Hook { conv } => conv,
    };
    let Binding::Bound {
        seat,
        conv: writer,
        n,
        opened,
        blocked,
        forks,
    } = b
    else {
        // No seat. A fork gets a seat with no writer; anything else starts one.
        return match ev {
            SeatEvent::Start {
                source: Source::Fork,
                ..
            } => (
                mint(fresh, None, add_fork([None; MAX_FORKS], conv)),
                Role::Observer,
                SeatEffects {
                    minted: true,
                    open_seat: true,
                    fork_line: true,
                    ..NONE
                },
            ),
            SeatEvent::Start { .. } | SeatEvent::PersonPrompt { .. } | SeatEvent::Hook { .. } => (
                mint(fresh, Some(conv), [None; MAX_FORKS]),
                Role::Writer,
                SeatEffects {
                    minted: true,
                    open_seat: true,
                    ..NONE
                },
            ),
        };
    };
    let bound = |w: Option<ConvId>, n: u32, f: [Option<ConvId>; MAX_FORKS]| Binding::Bound {
        seat,
        conv: w,
        n,
        opened,
        blocked,
        forks: f,
    };
    // A conversation that started as a fork stays an observer (X5).
    if is_fork(&forks, conv) {
        return (b, Role::Observer, NONE);
    }
    let is_writer = writer == Some(conv);
    let take = || {
        (
            bound(Some(conv), n.saturating_add(1), forks),
            Role::Writer,
            NONE,
        )
    };
    match ev {
        // A fork is never the writer, even if it reuses the writer's id.
        SeatEvent::Start {
            source: Source::Fork,
            ..
        } => {
            let w = if is_writer { None } else { writer };
            (
                bound(w, n, add_fork(forks, conv)),
                Role::Observer,
                SeatEffects {
                    fork_line: true,
                    ..NONE
                },
            )
        }
        // An automatic compaction never moves the seat.
        SeatEvent::Start {
            source: Source::Compact,
            ..
        } => match writer {
            None => take(),
            Some(w) if w == conv => (b, Role::Writer, NONE),
            Some(_) => (b, Role::Observer, NONE),
        },
        // The last starter is the writer; a person's prompt takes the seat back.
        SeatEvent::Start {
            source: Source::Startup | Source::Resume | Source::Clear,
            ..
        }
        | SeatEvent::PersonPrompt { .. } => {
            if is_writer {
                (b, Role::Writer, NONE)
            } else {
                take()
            }
        }
        SeatEvent::Hook { .. } => match writer {
            None => take(),
            Some(w) if w == conv => (b, Role::Writer, NONE),
            Some(_) => (
                b,
                Role::Observer,
                SeatEffects {
                    observer_line: true,
                    ..NONE
                },
            ),
        },
    }
}

/// M12 for the room's answers about this seat.
pub fn seat_room_step(b: Binding, a: RoomAnswer) -> (Binding, SeatEffects) {
    match b {
        Binding::NoSeat => (b, NONE),
        Binding::Bound {
            seat,
            conv,
            n,
            opened,
            forks,
            ..
        } => match a {
            RoomAnswer::Opened => (
                Binding::Bound {
                    seat,
                    conv,
                    n,
                    opened: true,
                    blocked: false,
                    forks,
                },
                NONE,
            ),
            RoomAnswer::NoSuchSeat => (
                Binding::Bound {
                    seat,
                    conv,
                    n,
                    opened: false,
                    blocked: false,
                    forks,
                },
                SeatEffects {
                    open_seat: true,
                    ..NONE
                },
            ),
            RoomAnswer::TokenBraked if !opened => (
                Binding::Bound {
                    seat,
                    conv,
                    n,
                    opened,
                    blocked: true,
                    forks,
                },
                NONE,
            ),
            RoomAnswer::TokenBraked => (b, NONE),
        },
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn conv(n: u8) -> ConvId {
        let mut s = *b"00000000-0000-4000-8000-000000000000";
        s[35] = b'0' + n;
        ConvId::parse(&s).expect("a valid conversation id")
    }
    fn fresh() -> SeatId {
        SeatId::parse(b"s-abcd1234").expect("a valid seat id")
    }

    fn events() -> Vec<SeatEvent> {
        let mut v = Vec::new();
        for c in [conv(1), conv(2), conv(3)] {
            for source in [
                Source::Startup,
                Source::Resume,
                Source::Clear,
                Source::Compact,
                Source::Fork,
            ] {
                v.push(SeatEvent::Start { source, conv: c });
            }
            v.push(SeatEvent::PersonPrompt { conv: c });
            v.push(SeatEvent::Hook { conv: c });
        }
        v
    }

    #[test]
    fn x5_a_fork_never_becomes_writer_over_every_sequence_of_four() {
        let evs = events();
        let mut stack: Vec<(Binding, Vec<ConvId>, u8)> =
            alloc::vec![(Binding::NoSeat, Vec::new(), 0)];
        let mut checked = 0u32;
        while let Some((b, forked, depth)) = stack.pop() {
            if depth == 4 {
                continue;
            }
            for ev in &evs {
                let (b2, role, _) = seat_step(b, *ev, fresh());
                let mut forked2 = forked.clone();
                if let SeatEvent::Start {
                    source: Source::Fork,
                    conv: c,
                } = ev
                {
                    forked2.push(*c);
                }
                let who = match ev {
                    SeatEvent::Start { conv, .. }
                    | SeatEvent::PersonPrompt { conv }
                    | SeatEvent::Hook { conv } => *conv,
                };
                if forked2.contains(&who) {
                    assert_eq!(role, Role::Observer, "X5: {ev:?}");
                }
                if let Binding::Bound { conv: Some(w), .. } = b2 {
                    assert!(
                        !forked2.contains(&w),
                        "X5: a fork is the writer after {ev:?}"
                    );
                }
                checked += 1;
                stack.push((b2, forked2, depth + 1));
            }
        }
        assert!(checked > 100_000);
    }

    #[test]
    fn the_last_starter_writes_and_a_typed_prompt_takes_the_seat_back() {
        let (b, r, fx) = seat_step(
            Binding::NoSeat,
            SeatEvent::Start {
                source: Source::Startup,
                conv: conv(1),
            },
            fresh(),
        );
        assert_eq!((r, fx.minted, fx.open_seat), (Role::Writer, true, true));
        let (b, r, _) = seat_step(
            b,
            SeatEvent::Start {
                source: Source::Startup,
                conv: conv(2),
            },
            fresh(),
        );
        assert_eq!(r, Role::Writer);
        let (b, r, fx) = seat_step(b, SeatEvent::Hook { conv: conv(1) }, fresh());
        assert_eq!((r, fx.observer_line), (Role::Observer, true));
        let (b, r, _) = seat_step(
            b,
            SeatEvent::Start {
                source: Source::Compact,
                conv: conv(1),
            },
            fresh(),
        );
        assert_eq!(r, Role::Observer, "a compaction never moves the seat");
        let (_, r, _) = seat_step(b, SeatEvent::PersonPrompt { conv: conv(1) }, fresh());
        assert_eq!(r, Role::Writer);
    }

    #[test]
    fn x8_a_token_brake_join_blocks_and_an_accepted_open_clears_it() {
        let (b, _, _) = seat_step(
            Binding::NoSeat,
            SeatEvent::Start {
                source: Source::Startup,
                conv: conv(1),
            },
            fresh(),
        );
        let (b, _) = seat_room_step(b, RoomAnswer::TokenBraked);
        assert!(matches!(
            b,
            Binding::Bound {
                blocked: true,
                opened: false,
                ..
            }
        ));
        let (b, _) = seat_room_step(b, RoomAnswer::Opened);
        assert!(matches!(
            b,
            Binding::Bound {
                blocked: false,
                opened: true,
                ..
            }
        ));
        let (b, fx) = seat_room_step(b, RoomAnswer::NoSuchSeat);
        assert!(fx.open_seat && matches!(b, Binding::Bound { opened: false, .. }));
    }
}
