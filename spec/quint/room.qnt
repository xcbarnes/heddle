// The room, at design level: the lease clock (M2), claims (M1), pause and
// stop (M5) and the room's copy of each seat's PHI label (M13), behind the
// gates of the envelope (R0, check 6). Constants are the scaled model values.
//
// One state record `st`, the state before the last step `prev`, and what the
// last step was `ev`, so step invariants (K1, C5, T8, ...) are predicates.
// Invariant names are the specification's ids.
module room {
  pure val LEASE = 3
  pure val GRACE = 2
  pure val SILENCE_CAP = 1
  pure val CLOSING = 2
  pure val STOP_DECAY = 4
  pure val MAX_CLAIMS_PER_SEAT = 1

  pure val SEATS = Set("s1", "s2", "s3")
  pure val KEYS = Set("k1", "k2")
  // Room-clock jumps: backwards (clamped), none, one, and past each constant.
  pure val DTS = Set(-1, 0, 1, 2, 3, 5)

  type Phase = PFree | PLive | PStale | PTakeable | PClosing | PClosingStale | PClosingTakeable | PClosed

  type HeldRow = { owner: str, epoch: int, expires: int }
  type ClosingRow = { owner: str, epoch: int, deadline: int, resumeExpires: int }
  type Claim = Free | Held(HeldRow) | Closing(ClosingRow) | Closed(int)

  type Mark = { seq: int, firstAt: int }
  type Slot = Empty | Marked(Mark)
  type Restrictions = { agentPause: Slot, agentStop: Slot, personPause: Slot, personStop: Slot }

  type Cause = ResultMasked | BatchUnsent | ToolInput | FoundLater
  type SeatPhi = Clean | Exposed(Cause) | Sealed

  type State = {
    lastNow: int,
    leaseClock: int,
    seq: int,
    nextEpoch: int,
    claims: str -> Claim,
    restr: Restrictions,
    phi: str -> SeatPhi
  }

  type Kind =
    | KPeek | KClaim | KRelease | KDoneBegin | KDoneAbort | KDoneCommit | KSync
    | KRestrictPause | KRestrictStop | KTaintMasked | KTaintInput
    | KAdminPause | KAdminStop | KResume | KConfirm | KReleaseKey | KReopen
    | KRecordFalseAlarm | KRecordReal | KRecordConfirmed | KTick

  type Ev = {
    kind: Kind,
    seat: str,
    key: str,
    accepted: bool,
    granted: int,
    frozenBefore: bool,
    levelAtGate: int,
    n: int
  }

  pure val KINDS = Set(
    KPeek, KClaim, KRelease, KDoneBegin, KDoneAbort, KDoneCommit, KSync,
    KRestrictPause, KRestrictStop, KTaintMasked, KTaintInput,
    KAdminPause, KAdminStop, KResume, KConfirm, KReleaseKey, KReopen,
    KRecordFalseAlarm, KRecordReal, KRecordConfirmed, KTick
  )

  // ---- pure helpers ----

  pure def min(a: int, b: int): int = if (a < b) a else b
  pure def max(a: int, b: int): int = if (a > b) a else b

  pure def isSet(s: Slot): bool = match s {
    | Empty => false
    | Marked(m) => true
  }
  pure def seqOf(s: Slot): int = match s {
    | Empty => -1
    | Marked(m) => m.seq
  }
  pure def firstAtOf(s: Slot): int = match s {
    | Empty => -1
    | Marked(m) => m.firstAt
  }

  // M5: 0 Running, 1 Paused, 2 Stopped.
  pure def levelOf(r: Restrictions, lastNow: int): int = {
    val agentStopLive = match r.agentStop {
      | Empty => false
      | Marked(m) => lastNow - m.firstAt < STOP_DECAY
    }
    if (isSet(r.personStop) or agentStopLive) 2
    else if (isSet(r.agentPause) or isSet(r.agentStop) or isSet(r.personPause)) 1
    else 0
  }

  // A repeat takes the new seq and keeps first_at (P9).
  pure def touch(s: Slot, seq: int, now: int): Slot = match s {
    | Empty => Marked({ seq: seq, firstAt: now })
    | Marked(m) => Marked({ seq: seq, firstAt: m.firstAt })
  }
  pure def mergeInto(into: Slot, m: Mark): Slot = match into {
    | Empty => Marked(m)
    | Marked(p) => Marked({ seq: max(p.seq, m.seq), firstAt: min(p.firstAt, m.firstAt) })
  }
  pure def keepAbove(s: Slot, upto: int): Slot = if (seqOf(s) > upto) s else Empty

  // M1
  pure def phase(c: Claim, t: int): Phase = match c {
    | Free => PFree
    | Held(h) => if (t < h.expires) PLive else if (t < h.expires + GRACE) PStale else PTakeable
    | Closing(cl) => if (t < cl.deadline) PClosing else if (t < cl.deadline + GRACE) PClosingStale else PClosingTakeable
    | Closed(at) => PClosed
  }
  pure def ownerOf(c: Claim): str = match c {
    | Free => ""
    | Held(h) => h.owner
    | Closing(cl) => cl.owner
    | Closed(at) => ""
  }
  pure def isTakeable(p: Phase): bool = p == PTakeable or p == PClosingTakeable

  pure def openClaims(claims: str -> Claim, seat: str): int =
    KEYS.filter(k => ownerOf(claims.get(k)) == seat).size()

  pure def isClean(p: SeatPhi): bool = match p {
    | Clean => true
    | Exposed(c) => false
    | Sealed => false
  }
  pure def isExposed(p: SeatPhi): bool = match p {
    | Clean => false
    | Exposed(c) => true
    | Sealed => false
  }
  pure def maskedOrUnsent(p: SeatPhi): bool = match p {
    | Clean => false
    | Exposed(c) => c == ResultMasked or c == BatchUnsent
    | Sealed => false
  }

  // ---- the step ----

  // M2, run once per authenticated request. `frozen` reads the old last_now.
  pure def advance(s: State, dt: int): State = {
    val frozen = levelOf(s.restr, s.lastNow) >= 1
    val eff = if (dt > 0) s.lastNow + dt else s.lastNow
    val by = if (frozen) 0 else min(eff - s.lastNow, SILENCE_CAP)
    s.with("lastNow", eff).with("leaseClock", s.leaseClock + by)
  }

  pure def bump(s: State): State = s.with("seq", s.seq + 1)

  // A grant: a fresh epoch from the room-wide counter (C2).
  pure def grant(s: State, seat: str, k: str): State = {
    val e = s.nextEpoch + 1
    bump(s.with("nextEpoch", e).with("claims", s.claims.set(k, Held({ owner: seat, epoch: e, expires: s.leaseClock + LEASE }))))
  }

  pure def renew(s: State, k: str, h: HeldRow): State =
    bump(s.with("claims", s.claims.set(k, Held(h.with("expires", max(h.expires, s.leaseClock + LEASE))))))

  // (new state, accepted, granted epoch or -1)
  pure def claimMove(s: State, seat: str, k: str): (State, bool, int) = {
    val c = s.claims.get(k)
    val roomForGrant = openClaims(s.claims, seat) < MAX_CLAIMS_PER_SEAT
    val p = phase(c, s.leaseClock)
    match c {
      | Closed(at) => (s, false, -1)
      | Free => if (roomForGrant) (grant(s, seat, k), true, s.nextEpoch + 1) else (s, false, -1)
      | Held(h) =>
        if (h.owner == seat) (renew(s, k, h), true, -1)
        else if (isTakeable(p) and roomForGrant) (grant(s, seat, k), true, s.nextEpoch + 1)
        else (s, false, -1)
      | Closing(cl) =>
        if (cl.owner == seat) (s, true, -1)
        else if (isTakeable(p) and roomForGrant) (grant(s, seat, k), true, s.nextEpoch + 1)
        else (s, false, -1)
    }
  }

  pure def ownerMove(s: State, kind: Kind, seat: str, k: str): (State, bool) = {
    val c = s.claims.get(k)
    match c {
      | Free => (s, false)
      | Closed(at) =>
        // DoneCommit on a matching Closed row is NoChange: accepted, no move.
        if (kind == KDoneCommit) (s, true) else (s, false)
      | Held(h) =>
        if (h.owner != seat) (s, false)
        else if (kind == KRelease) (bump(s.with("claims", s.claims.set(k, Free))), true)
        else if (kind == KDoneBegin)
          (bump(s.with("claims", s.claims.set(k, Closing({ owner: seat, epoch: h.epoch, deadline: s.leaseClock + CLOSING, resumeExpires: h.expires })))), true)
        else (s, false)
      | Closing(cl) =>
        if (cl.owner != seat) (s, false)
        else if (kind == KRelease) (bump(s.with("claims", s.claims.set(k, Free))), true)
        else if (kind == KDoneBegin)
          (bump(s.with("claims", s.claims.set(k, Closing(cl.with("deadline", s.leaseClock + CLOSING))))), true)
        else if (kind == KDoneAbort)
          (bump(s.with("claims", s.claims.set(k, Held({ owner: seat, epoch: cl.epoch, expires: max(cl.resumeExpires, s.leaseClock + LEASE) })))), true)
        else if (kind == KDoneCommit) (bump(s.with("claims", s.claims.set(k, Closed(s.lastNow)))), true)
        else (s, false)
    }
  }

  // Sync renews every Held claim the seat owns.
  pure def syncMove(s: State, seat: str): State = {
    val renewed = KEYS.mapBy(k => match s.claims.get(k) {
      | Held(h) => if (h.owner == seat) Held(h.with("expires", max(h.expires, s.leaseClock + LEASE))) else Held(h)
      | Free => Free
      | Closing(cl) => Closing(cl)
      | Closed(at) => Closed(at)
    })
    if (renewed == s.claims) s else bump(s.with("claims", renewed))
  }

  // Taint: the seat becomes Exposed and every claim it owns becomes Free.
  pure def taintMove(s: State, seat: str, cause: Cause): (State, bool) =
    if (not(isClean(s.phi.get(seat)))) (s, true)
    else {
      val freed = KEYS.mapBy(k => if (ownerOf(s.claims.get(k)) == seat) Free else s.claims.get(k))
      (bump(s.with("phi", s.phi.set(seat, Exposed(cause))).with("claims", freed)), true)
    }

  // Admin RecordPhi, checked in the order of the M13 room table.
  pure def recordMove(s: State, kind: Kind, seat: str): (State, bool) = {
    val p = s.phi.get(seat)
    if (not(isExposed(p))) (s, false)
    else if (kind == KRecordConfirmed) (bump(s.with("phi", s.phi.set(seat, Sealed))), true)
    else if (kind == KRecordReal and not(maskedOrUnsent(p))) (s, false)
    else (bump(s.with("phi", s.phi.set(seat, Clean))), true)
  }

  pure def isSeatNamed(kind: Kind): bool = Set(KClaim, KRelease, KDoneBegin, KDoneAbort, KDoneCommit, KSync, KTaintMasked, KTaintInput).contains(kind)
  pure def needsCleanSeat(kind: Kind): bool = Set(KClaim, KDoneBegin, KDoneAbort, KDoneCommit, KSync).contains(kind)
  pure def isNewWork(kind: Kind): bool = kind == KClaim or kind == KDoneBegin

  pure def apply(s0: State, kind: Kind, seat: str, k: str, dt: int, n: int): (State, Ev) = {
    val frozenBefore = levelOf(s0.restr, s0.lastNow) >= 1
    val s = if (kind == KTick) s0 else advance(s0, dt)
    val lvl = levelOf(s.restr, s.lastNow)
    val base = { kind: kind, seat: seat, key: k, accepted: false, granted: -1, frozenBefore: frozenBefore and kind != KTick, levelAtGate: lvl, n: n }
    val refused = (s, base)
    // R0 check 6: the PHI gate, then the level gate.
    if (needsCleanSeat(kind) and not(isClean(s.phi.get(seat)))) refused
    else if (isNewWork(kind) and lvl >= 1) refused
    else if (kind == KClaim) {
      val r = claimMove(s, seat, k)
      (r._1, base.with("accepted", r._2).with("granted", r._3))
    }
    else if (Set(KRelease, KDoneBegin, KDoneAbort, KDoneCommit).contains(kind)) {
      val r = ownerMove(s, kind, seat, k)
      (r._1, base.with("accepted", r._2))
    }
    else if (kind == KSync) (syncMove(s, seat), base.with("accepted", true))
    else if (kind == KTaintMasked or kind == KTaintInput) {
      val r = taintMove(s, seat, if (kind == KTaintMasked) ResultMasked else ToolInput)
      (r._1, base.with("accepted", r._2))
    }
    else if (kind == KRestrictPause)
      (bump(s.with("restr", s.restr.with("agentPause", touch(s.restr.agentPause, s.seq + 1, s.lastNow)))), base.with("accepted", true))
    else if (kind == KRestrictStop)
      (bump(s.with("restr", s.restr.with("agentStop", touch(s.restr.agentStop, s.seq + 1, s.lastNow)))), base.with("accepted", true))
    else if (kind == KAdminPause)
      (bump(s.with("restr", s.restr.with("personPause", touch(s.restr.personPause, s.seq + 1, s.lastNow)))), base.with("accepted", true))
    else if (kind == KAdminStop)
      (bump(s.with("restr", s.restr.with("personStop", touch(s.restr.personStop, s.seq + 1, s.lastNow)))), base.with("accepted", true))
    else if (kind == KResume) {
      val upto = s.seq - n
      val r2 = { agentPause: keepAbove(s.restr.agentPause, upto), agentStop: keepAbove(s.restr.agentStop, upto),
                 personPause: keepAbove(s.restr.personPause, upto), personStop: keepAbove(s.restr.personStop, upto) }
      (if (r2 == s.restr) s else bump(s.with("restr", r2)), base.with("accepted", true).with("n", upto))
    }
    else if (kind == KConfirm) {
      val target = s.seq - n
      match s.restr.agentPause {
        | Marked(m) =>
          if (m.seq == target)
            (bump(s.with("restr", s.restr.with("agentPause", Empty).with("personPause", mergeInto(s.restr.personPause, m)))), base.with("accepted", true))
          else match s.restr.agentStop {
            | Marked(m2) =>
              if (m2.seq == target)
                (bump(s.with("restr", s.restr.with("agentStop", Empty).with("personStop", mergeInto(s.restr.personStop, m2)))), base.with("accepted", true))
              else refused
            | Empty => refused
          }
        | Empty => match s.restr.agentStop {
          | Marked(m2) =>
            if (m2.seq == target)
              (bump(s.with("restr", s.restr.with("agentStop", Empty).with("personStop", mergeInto(s.restr.personStop, m2)))), base.with("accepted", true))
            else refused
          | Empty => refused
        }
      }
    }
    else if (kind == KReleaseKey) match s.claims.get(k) {
      | Held(h) => (bump(s.with("claims", s.claims.set(k, Free))), base.with("accepted", true))
      | Closing(cl) => (bump(s.with("claims", s.claims.set(k, Free))), base.with("accepted", true))
      | Free => (s, base.with("accepted", true))
      | Closed(at) => refused
    }
    else if (kind == KReopen) match s.claims.get(k) {
      | Closed(at) => (bump(s.with("claims", s.claims.set(k, Free))), base.with("accepted", true))
      | Free => refused
      | Held(h) => refused
      | Closing(cl) => refused
    }
    else if (Set(KRecordFalseAlarm, KRecordReal, KRecordConfirmed).contains(kind)) {
      val r = recordMove(s, kind, seat)
      (r._1, base.with("accepted", r._2))
    }
    // Peek and Tick change no shared row here (pruning is not modelled).
    else (s, base.with("accepted", true))
  }

  // ---- state machine ----

  pure val INIT: State = {
    lastNow: 0,
    leaseClock: 0,
    seq: 0,
    nextEpoch: 0,
    claims: KEYS.mapBy(k => Free),
    restr: { agentPause: Empty, agentStop: Empty, personPause: Empty, personStop: Empty },
    phi: SEATS.mapBy(s => Clean)
  }

  var st: State
  var prev: State
  var ev: Ev

  action init = all {
    st' = INIT,
    prev' = INIT,
    ev' = { kind: KPeek, seat: "s1", key: "k1", accepted: true, granted: -1, frozenBefore: false, levelAtGate: 0, n: 0 },
  }

  action step = {
    nondet kind = oneOf(KINDS)
    nondet seat = oneOf(SEATS)
    nondet k = oneOf(KEYS)
    nondet dt = oneOf(DTS)
    nondet n = oneOf(0.to(2))
    val r = apply(st, kind, seat, k, dt, n)
    all {
      prev' = st,
      st' = r._1,
      ev' = r._2,
    }
  }

  // ---- invariants ----

  // M2
  val K1 = st.leaseClock >= prev.leaseClock
  val K2 = st.leaseClock - prev.leaseClock <= SILENCE_CAP
  val K3 = ev.frozenBefore implies st.leaseClock == prev.leaseClock
  val K5 = st.lastNow >= prev.lastNow
  // M1
  val C1 = KEYS.forall(k => ownerOf(st.claims.get(k)) == "" or SEATS.contains(ownerOf(st.claims.get(k))))
  val C2 = ev.granted == -1 or (ev.granted > prev.nextEpoch and st.nextEpoch == ev.granted)
  val C5 = KEYS.forall(k => {
    val o1 = ownerOf(prev.claims.get(k))
    val o2 = ownerOf(st.claims.get(k))
    (o1 != "" and o2 != "" and o1 != o2) implies isTakeable(phase(prev.claims.get(k), st.leaseClock))
  })
  val C8 = (ev.accepted and isNewWork(ev.kind) and ev.granted != -1) implies ev.levelAtGate == 0
  val C9 = SEATS.forall(s => openClaims(st.claims, s) <= MAX_CLAIMS_PER_SEAT)
  // M5
  val P1 = {
    val cleared = (isSet(prev.restr.personPause) and not(isSet(st.restr.personPause)))
      or (isSet(prev.restr.personStop) and not(isSet(st.restr.personStop)))
    val agentCleared = (isSet(prev.restr.agentPause) and not(isSet(st.restr.agentPause)))
      or (isSet(prev.restr.agentStop) and not(isSet(st.restr.agentStop)))
    (cleared implies ev.kind == KResume) and (agentCleared implies (ev.kind == KResume or ev.kind == KConfirm))
  }
  // Only slots that hold a mark count: an empty slot's seqOf is a sentinel.
  pure def survives(before: Slot, after: Slot, upto: int): bool =
    (isSet(before) and seqOf(before) > upto) implies isSet(after)
  val P8 = ev.kind == KResume implies (
    survives(prev.restr.agentPause, st.restr.agentPause, ev.n)
    and survives(prev.restr.agentStop, st.restr.agentStop, ev.n)
    and survives(prev.restr.personPause, st.restr.personPause, ev.n)
    and survives(prev.restr.personStop, st.restr.personStop, ev.n)
  )
  val P9 = (isSet(prev.restr.agentStop) and isSet(st.restr.agentStop)) implies firstAtOf(prev.restr.agentStop) == firstAtOf(st.restr.agentStop)
  // M13, the room's half
  val T4 = (ev.accepted and isSeatNamed(ev.kind) and needsCleanSeat(ev.kind)) implies isClean(prev.phi.get(ev.seat))
  val T8 = SEATS.forall(s =>
    (isExposed(prev.phi.get(s)) and isClean(st.phi.get(s))) implies (ev.kind == KRecordFalseAlarm or ev.kind == KRecordReal))
  val T9 = (ev.accepted and ev.kind == KRecordReal) implies maskedOrUnsent(prev.phi.get(ev.seat))
  val T11 = SEATS.forall(s => (prev.phi.get(s) == Sealed) implies (st.phi.get(s) == Sealed))
  // R0
  val E10 = st.seq == prev.seq or st.seq == prev.seq + 1

  val inv = K1 and K2 and K3 and K5 and C1 and C2 and C5 and C8 and C9 and P1 and P8 and P9 and T4 and T8 and T9 and T11 and E10
}
