//! M1: claim and lease, per work key.
//!
//! A claim is an efficiency lock: it stops duplicate work. Git's refusal of a
//! push onto a moved main is what protects the code. Free is "no row", and
//! [`Phase`] is derived on every request and never stored, so no timer writes
//! state. [`claim_step`] is one total function with the guard order of the
//! specification; where a table and the code differ, the matrix printed from
//! the code is the specification.

use core::num::NonZeroU64;

use crate::config::Config;
use crate::ids::{SeatId, Sha};
use crate::time::{LeaseTime, RoomTime};

/// Rises on every grant, from one room-wide counter that never resets within
/// a generation (C2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(NonZeroU64);

impl Epoch {
    /// The checked constructor: any nonzero value, as on the wire.
    pub fn new(v: u64) -> Option<Epoch> {
        NonZeroU64::new(v).map(Epoch)
    }
    /// The epoch a grant takes after `counter`; `None` when it is exhausted.
    pub fn after(counter: u64) -> Option<Epoch> {
        counter.checked_add(1).and_then(Epoch::new)
    }
    /// The value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// How a task closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Closure {
    /// `status: done` on main.
    Done,
    /// `status: dropped` on main.
    Dropped,
}

/// A claim row. There is no Free row: Free is the absence of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// Held by a seat until `expires`, on the lease clock.
    Held {
        /// The seat that holds it.
        owner: SeatId,
        /// The grant's epoch.
        epoch: Epoch,
        /// When the lease ends.
        expires: LeaseTime,
    },
    /// Between `DoneBegin` and `DoneCommit`.
    Closing {
        /// The seat that holds it.
        owner: SeatId,
        /// The grant's epoch.
        epoch: Epoch,
        /// When the closing window ends.
        deadline: LeaseTime,
        /// The lease to restore on `DoneAbort` (C7).
        resume_expires: LeaseTime,
    },
    /// A tombstone, kept `CLOSED_TTL`.
    Closed {
        /// Done or dropped.
        how: Closure,
        /// `closing_commit`: the first commit on main that closed the task.
        sha: Sha,
        /// When the room closed it.
        at: RoomTime,
    },
}

impl Claim {
    /// The seat that holds a Held or Closing claim.
    pub const fn owner(&self) -> Option<SeatId> {
        match self {
            Claim::Held { owner, .. } | Claim::Closing { owner, .. } => Some(*owner),
            Claim::Closed { .. } => None,
        }
    }
}

/// A claim's phase at lease time `t`. Derived, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// No row.
    Free,
    /// Held, inside its lease.
    Live,
    /// Held, past its lease but inside the grace period. The owner revives it.
    Stale,
    /// Held, past lease and grace. Another seat may take it over.
    Takeable,
    /// Closing, inside its window.
    Closing,
    /// Closing, past its window but inside the grace period.
    ClosingStale,
    /// Closing, past window and grace.
    ClosingTakeable,
    /// A tombstone.
    Closed,
}

/// The phase (C6: it depends only on the claim and the lease clock).
pub fn phase(c: Option<&Claim>, t: LeaseTime, cfg: &Config) -> Phase {
    let grace = cfg.get().grace;
    match c {
        None => Phase::Free,
        Some(Claim::Held { expires, .. }) => {
            if t < *expires {
                Phase::Live
            } else if t < expires.plus(grace) {
                Phase::Stale
            } else {
                Phase::Takeable
            }
        }
        Some(Claim::Closing { deadline, .. }) => {
            if t < *deadline {
                Phase::Closing
            } else if t < deadline.plus(grace) {
                Phase::ClosingStale
            } else {
                Phase::ClosingTakeable
            }
        }
        Some(Claim::Closed { .. }) => Phase::Closed,
    }
}

/// The events that name a key and a calling seat, after R0's checks 3 to 6.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimEvent {
    /// Take the key, or keep it.
    Claim,
    /// Stretch the lease before a long tool call.
    Quiet {
        /// The claim's epoch.
        epoch: Epoch,
        /// How long, capped at `QUIET_MAX`.
        for_ms: u32,
    },
    /// Give the key up.
    Release {
        /// The claim's epoch.
        epoch: Epoch,
    },
    /// Step 2 of `done`.
    DoneBegin {
        /// The claim's epoch.
        epoch: Epoch,
    },
    /// `done` gave up after main moved three times.
    DoneAbort {
        /// The claim's epoch.
        epoch: Epoch,
    },
    /// Step 4 of `done`: the closing commit is on main.
    DoneCommit {
        /// The claim's epoch.
        epoch: Epoch,
        /// `closing_commit`.
        sha: Sha,
    },
}

impl ClaimEvent {
    const fn epoch(self) -> Option<Epoch> {
        match self {
            ClaimEvent::Claim => None,
            ClaimEvent::Quiet { epoch, .. }
            | ClaimEvent::Release { epoch }
            | ClaimEvent::DoneBegin { epoch }
            | ClaimEvent::DoneAbort { epoch }
            | ClaimEvent::DoneCommit { epoch, .. } => Some(epoch),
        }
    }
}

/// What a grant needs from the rest of the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimCtx {
    /// Held and Closing claims the caller owns now.
    pub open_by_caller: u16,
    /// Held and Closing rows in the room.
    pub live_rows: u16,
    /// The room-wide epoch counter.
    pub next_epoch: u64,
}

/// An accepted claim event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimReply {
    /// A new grant, with its epoch. The room's counter becomes this epoch.
    Granted(Epoch),
    /// The caller already holds it; the lease is renewed.
    AlreadyYours,
    /// The caller's claim is closing; nothing changed.
    Closing,
    /// The move was made.
    Done,
    /// Nothing to do: the same move already happened.
    NoChange,
}

/// A refused claim event. Refusals are values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimRefusal {
    /// Another seat holds it, inside lease and grace.
    HeldBy,
    /// The caller does not hold it.
    NotOwner,
    /// The event carries an old epoch.
    StaleEpoch,
    /// Nothing is held.
    NotHeld,
    /// The claim is not closing.
    NotClosing,
    /// The claim is closing.
    Closing,
    /// The key is closed.
    Closed,
    /// The key is not closed.
    NotClosed,
    /// The caller holds `MAX_CLAIMS_PER_SEAT` claims.
    TooManyClaims,
    /// The room holds `MAX_KEYS` (or `MAX_CLOSED`) rows.
    Full,
    /// The epoch counter cannot rise.
    Exhausted,
}

/// What the step means for the room's news.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimNews {
    /// Nothing to announce.
    None,
    /// A takeover from `from`.
    Takeover {
        /// The seat that lost the claim.
        from: SeatId,
    },
    /// The key closed.
    Closed,
}

/// One step of M1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimStep {
    /// The row after the step; `None` is Free.
    pub claim: Option<Claim>,
    /// Accepted or refused.
    pub outcome: Result<ClaimReply, ClaimRefusal>,
    /// News to append.
    pub news: ClaimNews,
}

fn unchanged(c: Option<Claim>, outcome: Result<ClaimReply, ClaimRefusal>) -> ClaimStep {
    ClaimStep { claim: c, outcome, news: ClaimNews::None }
}

fn moved(c: Option<Claim>, reply: ClaimReply, news: ClaimNews) -> ClaimStep {
    ClaimStep { claim: c, outcome: Ok(reply), news }
}

/// A grant, in step 2 (a new row) or as a takeover in step 3.
fn grant(c: Option<Claim>, caller: SeatId, t: LeaseTime, ctx: ClaimCtx, cfg: &Config, news: ClaimNews) -> ClaimStep {
    let r = cfg.get();
    if ctx.open_by_caller >= r.max_claims_per_seat {
        return unchanged(c, Err(ClaimRefusal::TooManyClaims));
    }
    // A takeover reuses the row, so only a new row can fill the table.
    if c.is_none() && ctx.live_rows >= r.max_keys {
        return unchanged(c, Err(ClaimRefusal::Full));
    }
    match Epoch::after(ctx.next_epoch) {
        None => unchanged(c, Err(ClaimRefusal::Exhausted)),
        Some(epoch) => moved(
            Some(Claim::Held { owner: caller, epoch, expires: t.plus(r.lease) }),
            ClaimReply::Granted(epoch),
            news,
        ),
    }
}

/// M1 for one key and one calling seat. `t` is the lease clock after this
/// request's `advance`; `now` is room time.
pub fn claim_step(
    c: Option<Claim>,
    caller: SeatId,
    ev: ClaimEvent,
    t: LeaseTime,
    now: RoomTime,
    ctx: ClaimCtx,
    cfg: &Config,
) -> ClaimStep {
    let r = cfg.get();
    let p = phase(c.as_ref(), t, cfg);
    match c {
        // 1. A Closed row.
        Some(Claim::Closed { how, sha, .. }) => match ev {
            ClaimEvent::DoneCommit { sha: s, .. } if how == Closure::Done && s == sha => {
                unchanged(c, Ok(ClaimReply::NoChange))
            }
            ClaimEvent::Claim
            | ClaimEvent::Quiet { .. }
            | ClaimEvent::Release { .. }
            | ClaimEvent::DoneBegin { .. }
            | ClaimEvent::DoneAbort { .. }
            | ClaimEvent::DoneCommit { .. } => unchanged(c, Err(ClaimRefusal::Closed)),
        },
        // 2. No row.
        None => match ev {
            ClaimEvent::Claim => grant(c, caller, t, ctx, cfg, ClaimNews::None),
            ClaimEvent::Quiet { .. } | ClaimEvent::Release { .. } | ClaimEvent::DoneBegin { .. } => {
                unchanged(c, Err(ClaimRefusal::NotHeld))
            }
            ClaimEvent::DoneAbort { .. } | ClaimEvent::DoneCommit { .. } => {
                unchanged(c, Err(ClaimRefusal::NotClosing))
            }
        },
        Some(Claim::Held { owner, epoch, expires }) => {
            // 3. Not the owner.
            if owner != caller {
                return match ev {
                    ClaimEvent::Claim if p == Phase::Takeable => {
                        grant(c, caller, t, ctx, cfg, ClaimNews::Takeover { from: owner })
                    }
                    ClaimEvent::Claim => unchanged(c, Err(ClaimRefusal::HeldBy)),
                    ClaimEvent::Quiet { .. }
                    | ClaimEvent::Release { .. }
                    | ClaimEvent::DoneBegin { .. }
                    | ClaimEvent::DoneAbort { .. }
                    | ClaimEvent::DoneCommit { .. } => unchanged(c, Err(ClaimRefusal::NotOwner)),
                };
            }
            // 4. An old epoch.
            if ev.epoch().is_some_and(|e| e != epoch) {
                return unchanged(c, Err(ClaimRefusal::StaleEpoch));
            }
            // 5. The phase cell: Live, Stale and Takeable act alike for the owner.
            match ev {
                ClaimEvent::Claim => moved(
                    Some(Claim::Held { owner, epoch, expires: expires.max(t.plus(r.lease)) }),
                    ClaimReply::AlreadyYours,
                    ClaimNews::None,
                ),
                ClaimEvent::Quiet { for_ms, .. } => moved(
                    Some(Claim::Held {
                        owner,
                        epoch,
                        expires: expires.max(t.plus(u64::from(for_ms).min(r.quiet_max))),
                    }),
                    ClaimReply::Done,
                    ClaimNews::None,
                ),
                ClaimEvent::Release { .. } => moved(None, ClaimReply::Done, ClaimNews::None),
                ClaimEvent::DoneBegin { .. } => moved(
                    Some(Claim::Closing { owner, epoch, deadline: t.plus(r.closing), resume_expires: expires }),
                    ClaimReply::Done,
                    ClaimNews::None,
                ),
                ClaimEvent::DoneAbort { .. } | ClaimEvent::DoneCommit { .. } => {
                    unchanged(c, Err(ClaimRefusal::NotClosing))
                }
            }
        }
        Some(Claim::Closing { owner, epoch, resume_expires, .. }) => {
            if owner != caller {
                return match ev {
                    ClaimEvent::Claim if p == Phase::ClosingTakeable => {
                        grant(c, caller, t, ctx, cfg, ClaimNews::Takeover { from: owner })
                    }
                    ClaimEvent::Claim => unchanged(c, Err(ClaimRefusal::HeldBy)),
                    ClaimEvent::Quiet { .. }
                    | ClaimEvent::Release { .. }
                    | ClaimEvent::DoneBegin { .. }
                    | ClaimEvent::DoneAbort { .. }
                    | ClaimEvent::DoneCommit { .. } => unchanged(c, Err(ClaimRefusal::NotOwner)),
                };
            }
            if ev.epoch().is_some_and(|e| e != epoch) {
                return unchanged(c, Err(ClaimRefusal::StaleEpoch));
            }
            match ev {
                ClaimEvent::Claim => unchanged(c, Ok(ClaimReply::Closing)),
                ClaimEvent::Quiet { .. } => unchanged(c, Err(ClaimRefusal::Closing)),
                ClaimEvent::Release { .. } => moved(None, ClaimReply::Done, ClaimNews::None),
                ClaimEvent::DoneBegin { .. } => moved(
                    Some(Claim::Closing { owner, epoch, deadline: t.plus(r.closing), resume_expires }),
                    ClaimReply::Done,
                    ClaimNews::None,
                ),
                ClaimEvent::DoneAbort { .. } => moved(
                    Some(Claim::Held { owner, epoch, expires: resume_expires.max(t.plus(r.lease)) }),
                    ClaimReply::Done,
                    ClaimNews::None,
                ),
                ClaimEvent::DoneCommit { sha, .. } => moved(
                    Some(Claim::Closed { how: Closure::Done, sha, at: now }),
                    ClaimReply::Done,
                    ClaimNews::Closed,
                ),
            }
        }
    }
}

/// `Sync` from `seat` renews the claim when the seat holds it and it is Held.
pub fn sync_renew(c: Option<Claim>, seat: SeatId, t: LeaseTime, cfg: &Config) -> Option<Claim> {
    match c {
        Some(Claim::Held { owner, epoch, expires }) if owner == seat => {
            Some(Claim::Held { owner, epoch, expires: expires.max(t.plus(cfg.get().lease)) })
        }
        Some(Claim::Held { .. }) | Some(Claim::Closing { .. }) | Some(Claim::Closed { .. }) | None => c,
    }
}

/// `LedgerObserve(how, sha)`, from any token: git says the task closed.
/// `closed_full` is whether the room already holds `MAX_CLOSED` Closed rows.
/// Returns the row, the outcome, and the seat that held it, if any.
pub fn ledger_observe(
    c: Option<Claim>,
    how: Closure,
    sha: Sha,
    now: RoomTime,
    closed_full: bool,
) -> (Option<Claim>, Result<ClaimReply, ClaimRefusal>, Option<SeatId>) {
    let closed = Some(Claim::Closed { how, sha, at: now });
    match c {
        None if closed_full => (c, Err(ClaimRefusal::Full), None),
        None => (closed, Ok(ClaimReply::Done), None),
        Some(Claim::Held { owner, .. }) | Some(Claim::Closing { owner, .. }) => {
            (closed, Ok(ClaimReply::Done), Some(owner))
        }
        Some(Claim::Closed { how: h, sha: s, .. }) if h == how && s == sha => (c, Ok(ClaimReply::NoChange), None),
        Some(Claim::Closed { .. }) => (c, Err(ClaimRefusal::Closed), None),
    }
}

/// `Admin ReleaseKey`. Returns the row, the outcome and the seat that lost it.
pub fn admin_release(c: Option<Claim>) -> (Option<Claim>, Result<ClaimReply, ClaimRefusal>, Option<SeatId>) {
    match c {
        None => (c, Ok(ClaimReply::NoChange), None),
        Some(Claim::Held { owner, .. }) | Some(Claim::Closing { owner, .. }) => (None, Ok(ClaimReply::Done), Some(owner)),
        Some(Claim::Closed { .. }) => (c, Err(ClaimRefusal::Closed), None),
    }
}

/// `Admin Reopen`: a Closed row is deleted (C4).
pub fn admin_reopen(c: Option<Claim>) -> (Option<Claim>, Result<ClaimReply, ClaimRefusal>) {
    match c {
        Some(Claim::Closed { .. }) => (None, Ok(ClaimReply::Done)),
        Some(Claim::Held { .. }) | Some(Claim::Closing { .. }) | None => (c, Err(ClaimRefusal::NotClosed)),
    }
}

/// `Taint` or `Admin TaintSeat` for `seat`: every claim it holds becomes Free.
pub fn taint_free(c: Option<Claim>, seat: SeatId) -> Option<Claim> {
    match c {
        Some(Claim::Held { owner, .. }) | Some(Claim::Closing { owner, .. }) if owner == seat => None,
        Some(Claim::Held { .. }) | Some(Claim::Closing { .. }) | Some(Claim::Closed { .. }) | None => c,
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const CFG: Config = Config::MODEL;

    fn seat(s: &[u8]) -> SeatId {
        SeatId::parse(s).expect("a valid seat id")
    }
    fn sha(n: u8) -> Sha {
        let mut s = [b'0'; 40];
        s[39] = b'0' + n;
        Sha::parse(&s).expect("a valid sha")
    }
    fn ep(n: u64) -> Epoch {
        Epoch::new(n).expect("nonzero")
    }

    /// Every row the small domain can hold: owners A and B, epochs 1 and 2,
    /// lease times 0 to 6, and two closing commits.
    fn rows() -> Vec<Option<Claim>> {
        let mut v = Vec::new();
        v.push(None);
        for owner in [seat(b"s-aaaaaaaa"), seat(b"s-bbbbbbbb")] {
            for e in [ep(1), ep(2)] {
                for x in 0..7 {
                    v.push(Some(Claim::Held { owner, epoch: e, expires: LeaseTime(x) }));
                    for rx in 0..4 {
                        v.push(Some(Claim::Closing { owner, epoch: e, deadline: LeaseTime(x), resume_expires: LeaseTime(rx) }));
                    }
                }
            }
        }
        for how in [Closure::Done, Closure::Dropped] {
            for n in [1, 2] {
                v.push(Some(Claim::Closed { how, sha: sha(n), at: RoomTime(0) }));
            }
        }
        v
    }

    fn events() -> Vec<ClaimEvent> {
        let mut v = Vec::new();
        v.push(ClaimEvent::Claim);
        for e in [ep(1), ep(2)] {
            v.push(ClaimEvent::Quiet { epoch: e, for_ms: 1 });
            v.push(ClaimEvent::Quiet { epoch: e, for_ms: 100 });
            v.push(ClaimEvent::Release { epoch: e });
            v.push(ClaimEvent::DoneBegin { epoch: e });
            v.push(ClaimEvent::DoneAbort { epoch: e });
            v.push(ClaimEvent::DoneCommit { epoch: e, sha: sha(1) });
        }
        v
    }

    fn is_owner_event(ev: ClaimEvent) -> bool {
        !matches!(ev, ClaimEvent::Claim)
    }

    #[test]
    fn c2_c3_c5_c7_d2_over_every_small_state_event_caller_and_time() {
        let ctx = ClaimCtx { open_by_caller: 0, live_rows: 0, next_epoch: 2 };
        let mut steps = 0u32;
        for c in rows() {
            for ev in events() {
                for caller in [seat(b"s-aaaaaaaa"), seat(b"s-bbbbbbbb")] {
                    for t in 0..12 {
                        let t = LeaseTime(t);
                        let s = claim_step(c, caller, ev, t, RoomTime(9), ctx, &CFG);
                        steps += 1;
                        let before = c.and_then(|x| x.owner());
                        let after = s.claim.and_then(|x| x.owner());
                        // C2: a grant takes the counter's next value.
                        if let Ok(ClaimReply::Granted(e)) = s.outcome {
                            assert_eq!(e.get(), ctx.next_epoch + 1, "C2");
                        }
                        // C3: owner events change a claim only for its owner, at its epoch.
                        if is_owner_event(ev) && s.claim != c {
                            let held = match c {
                                Some(Claim::Held { owner, epoch, .. }) | Some(Claim::Closing { owner, epoch, .. }) => {
                                    Some((owner, epoch))
                                }
                                Some(Claim::Closed { .. }) | None => None,
                            };
                            assert_eq!(held.map(|(o, _)| o), Some(caller), "C3 owner: {c:?} {ev:?}");
                            assert_eq!(held.map(|(_, e)| e), ev.epoch(), "C3 epoch: {c:?} {ev:?}");
                        }
                        // C5: no takeover before lease plus grace.
                        if let (Some(b), Some(a)) = (before, after) {
                            if b != a {
                                let p = phase(c.as_ref(), t, &CFG);
                                assert!(matches!(p, Phase::Takeable | Phase::ClosingTakeable), "C5: {c:?} {ev:?} {t:?}");
                            }
                        }
                        // C7: a Held claim kept by the same owner and epoch never loses lease.
                        if let (Some(Claim::Held { owner: o1, epoch: e1, expires: x1 }), Some(Claim::Held { owner: o2, epoch: e2, expires: x2 })) = (c, s.claim) {
                            if o1 == o2 && e1 == e2 {
                                assert!(x2 >= x1, "C7: {c:?} {ev:?}");
                            }
                        }
                        // C7: DoneAbort restores at least the lease DoneBegin saved.
                        if let (Some(Claim::Closing { resume_expires, .. }), Some(Claim::Held { expires, .. }), ClaimEvent::DoneAbort { .. }) = (c, s.claim, ev) {
                            assert!(expires >= resume_expires, "C7 abort");
                        }
                        // D2: a Closed row is never granted.
                        if matches!(c, Some(Claim::Closed { .. })) {
                            assert!(!matches!(s.outcome, Ok(ClaimReply::Granted(_))), "D2");
                            assert!(matches!(s.claim, Some(Claim::Closed { .. })), "D2 row");
                        }
                    }
                }
            }
        }
        assert!(steps > 10_000);
    }

    #[test]
    fn a_takeover_needs_lease_plus_grace_and_says_who_lost_it() {
        let (a, b) = (seat(b"s-aaaaaaaa"), seat(b"s-bbbbbbbb"));
        let ctx = ClaimCtx { open_by_caller: 0, live_rows: 1, next_epoch: 1 };
        let held = Some(Claim::Held { owner: a, epoch: ep(1), expires: LeaseTime(3) });
        let lease_plus_grace = 3 + CFG.get().grace;
        let early = claim_step(held, b, ClaimEvent::Claim, LeaseTime(lease_plus_grace - 1), RoomTime(0), ctx, &CFG);
        assert_eq!(early.outcome, Err(ClaimRefusal::HeldBy));
        let late = claim_step(held, b, ClaimEvent::Claim, LeaseTime(lease_plus_grace), RoomTime(0), ctx, &CFG);
        assert_eq!(late.outcome, Ok(ClaimReply::Granted(ep(2))));
        assert_eq!(late.news, ClaimNews::Takeover { from: a });
        // The old owner's late DoneCommit is refused.
        let commit = ClaimEvent::DoneCommit { epoch: ep(1), sha: sha(1) };
        let refused = claim_step(late.claim, a, commit, LeaseTime(9), RoomTime(0), ctx, &CFG);
        assert_eq!(refused.outcome, Err(ClaimRefusal::NotOwner));
    }

    #[test]
    fn caps_and_the_epoch_counter_are_named_refusals() {
        let a = seat(b"s-aaaaaaaa");
        let full = ClaimCtx { open_by_caller: 0, live_rows: CFG.get().max_keys, next_epoch: 0 };
        assert_eq!(claim_step(None, a, ClaimEvent::Claim, LeaseTime(0), RoomTime(0), full, &CFG).outcome, Err(ClaimRefusal::Full));
        let busy = ClaimCtx { open_by_caller: CFG.get().max_claims_per_seat, live_rows: 0, next_epoch: 0 };
        assert_eq!(claim_step(None, a, ClaimEvent::Claim, LeaseTime(0), RoomTime(0), busy, &CFG).outcome, Err(ClaimRefusal::TooManyClaims));
        let spent = ClaimCtx { open_by_caller: 0, live_rows: 0, next_epoch: u64::MAX };
        assert_eq!(claim_step(None, a, ClaimEvent::Claim, LeaseTime(0), RoomTime(0), spent, &CFG).outcome, Err(ClaimRefusal::Exhausted));
    }

    #[test]
    fn the_ledger_closes_any_row_once_and_refuses_a_different_closure() {
        let a = seat(b"s-aaaaaaaa");
        let held = Some(Claim::Held { owner: a, epoch: ep(1), expires: LeaseTime(3) });
        let (row, out, lost) = ledger_observe(held, Closure::Done, sha(1), RoomTime(5), false);
        assert_eq!(out, Ok(ClaimReply::Done));
        assert_eq!(lost, Some(a));
        assert_eq!(ledger_observe(row, Closure::Done, sha(1), RoomTime(6), false).1, Ok(ClaimReply::NoChange));
        assert_eq!(ledger_observe(row, Closure::Dropped, sha(1), RoomTime(6), false).1, Err(ClaimRefusal::Closed));
        assert_eq!(ledger_observe(None, Closure::Done, sha(2), RoomTime(6), true).1, Err(ClaimRefusal::Full));
        assert_eq!(admin_reopen(row).0, None);
        assert_eq!(admin_reopen(held).1, Err(ClaimRefusal::NotClosed));
    }
}
