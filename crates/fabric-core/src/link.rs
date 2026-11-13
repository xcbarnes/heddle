//! M7: the link to the room, fed only by the results of hooks' own calls.
//!
//! There is no background process. One link file per repo is shared by every
//! seat; all times are on the monotonic clock. "Cloudflare down means Fabric
//! off": Off pauses news and makes claims fail, and never lowers the enforced
//! level (N3). No local or provisional claim exists (N1): there is no type for
//! one.

/// Failed calls before the link goes Off.
pub const FAIL_LIMIT: u8 = 3;
/// Or this long, in milliseconds, since the first failure.
pub const FAIL_SPAN: u64 = 10_000;
/// While Off, at most one probe per this long, per repo (L3).
pub const PROBE_EVERY: u64 = 10_000;
/// Answers before an Off link is Online again.
pub const OK_LIMIT: u8 = 2;

/// The link state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Calls are made.
    Online {
        /// Failures in a row.
        fails: u8,
        /// When the first of them happened.
        first_fail: Option<u64>,
    },
    /// Only probes are made.
    Off {
        /// Answers in a row.
        oks: u8,
        /// The earliest the next probe may go.
        next_probe: u64,
    },
}

impl Link {
    /// A new link.
    pub const START: Link = Link::Online {
        fails: 0,
        first_fail: None,
    };
}

/// What a hook's call came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallResult {
    /// A reply that passed `verify`.
    Verified,
    /// A 4xx: the room answered.
    Refused4xx,
    /// A timeout, a 5xx, a TLS failure or a bad signature.
    Failed,
    /// No call was made.
    NotCalled,
}

/// Whether a hook may call the room now.
pub fn may_call(l: &Link, now: u64) -> bool {
    match l {
        Link::Online { .. } => true,
        Link::Off { next_probe, .. } => now >= *next_probe,
    }
}

/// The M7 table.
pub fn link_step(l: Link, r: CallResult, now: u64) -> Link {
    match l {
        Link::Online { fails, first_fail } => match r {
            CallResult::Verified | CallResult::Refused4xx => Link::START,
            CallResult::NotCalled => l,
            CallResult::Failed => {
                let fails = fails.saturating_add(1);
                let first = first_fail.unwrap_or(now);
                if fails >= FAIL_LIMIT || now.saturating_sub(first) >= FAIL_SPAN {
                    Link::Off {
                        oks: 0,
                        next_probe: now.saturating_add(PROBE_EVERY),
                    }
                } else {
                    Link::Online {
                        fails,
                        first_fail: Some(first),
                    }
                }
            }
        },
        Link::Off { oks, .. } => match r {
            CallResult::NotCalled => l,
            CallResult::Failed => Link::Off {
                oks: 0,
                next_probe: now.saturating_add(PROBE_EVERY),
            },
            CallResult::Verified | CallResult::Refused4xx => {
                let oks = oks.saturating_add(1);
                if oks >= OK_LIMIT {
                    Link::START
                } else {
                    Link::Off {
                        oks,
                        next_probe: now,
                    }
                }
            }
        },
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn n2_one_slow_or_failed_reply_does_not_switch_fabric_off() {
        for t in [0, 1, 9_999, u64::MAX] {
            assert!(matches!(
                link_step(Link::START, CallResult::Failed, t),
                Link::Online { .. }
            ));
        }
    }

    #[test]
    fn n5_a_room_steadily_just_too_slow_moves_off_once_and_does_not_flap() {
        let mut l = Link::START;
        let mut moves_off = 0;
        for t in (0..120_000u64).step_by(500) {
            let r = if may_call(&l, t) {
                CallResult::Failed
            } else {
                CallResult::NotCalled
            };
            let next = link_step(l, r, t);
            if matches!(l, Link::Online { .. }) && matches!(next, Link::Off { .. }) {
                moves_off += 1;
            }
            l = next;
        }
        assert_eq!(moves_off, 1);
    }

    #[test]
    fn off_needs_two_answers_and_probes_at_most_every_ten_seconds() {
        let off = Link::Off {
            oks: 0,
            next_probe: 10_000,
        };
        assert!(!may_call(&off, 9_999));
        assert!(may_call(&off, 10_000));
        let once = link_step(off, CallResult::Verified, 10_000);
        assert!(matches!(once, Link::Off { oks: 1, .. }));
        assert_eq!(link_step(once, CallResult::Refused4xx, 10_001), Link::START);
        assert_eq!(
            link_step(once, CallResult::Failed, 10_001),
            Link::Off {
                oks: 0,
                next_probe: 20_001
            }
        );
    }
}
