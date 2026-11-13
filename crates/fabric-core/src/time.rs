//! The three clocks (state_machines.md §0) and the lease clock (M2).
//!
//! The clocks are different types, so they cannot be mixed. No event carries a
//! client timestamp (K4): the room reads only its own clock, and the laptop's
//! two clocks never reach the room.

use crate::config::Config;

/// Milliseconds on the room's clock: the Durable Object's `Date.now()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomTime(pub u64);

/// Milliseconds on the lease clock (M2). Every lease is measured on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseTime(pub u64);

/// The laptop's clocks: the wall clock, and a monotonic clock that keeps
/// counting during sleep (macOS `CLOCK_MONOTONIC`, not Rust's `Instant`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalNow {
    /// Wall clock, milliseconds since the Unix epoch.
    pub wall_ms: u64,
    /// Monotonic clock that counts sleep, milliseconds.
    pub mono_ms: u64,
}

impl LeaseTime {
    /// `self + d`. The lease clock saturates: `advance` must never refuse.
    pub const fn plus(self, d: u64) -> LeaseTime {
        LeaseTime(self.0.saturating_add(d))
    }
}

/// The room's two clocks, as kept in its `meta` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clocks {
    /// The latest room time any request carried (clamped: K5).
    pub last_now: RoomTime,
    /// The lease clock.
    pub lease_clock: LeaseTime,
}

/// M2. Runs once per authenticated request, reads included (`Tick` does not
/// count). `frozen` is `level >= Paused`, read with the old `last_now`.
///
/// K1 the lease clock never decreases; K2 one request moves it by at most
/// `min(dt, SILENCE_CAP)`; K3 it does not move while frozen; K5 `last_now`
/// never decreases.
pub fn advance(c: Clocks, now: RoomTime, frozen: bool, cfg: &Config) -> Clocks {
    let eff = if now > c.last_now { now } else { c.last_now };
    let dt = eff.0.saturating_sub(c.last_now.0);
    let by = if frozen { 0 } else { dt.min(cfg.get().silence_cap) };
    Clocks {
        last_now: eff,
        lease_clock: c.lease_clock.plus(by),
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn k1_k2_k3_k5_over_a_small_domain_and_the_edges() {
        let cfg = Config::MODEL;
        let cap = cfg.get().silence_cap;
        for last in 0..12u64 {
            for now in 0..12u64 {
                for lc in [0, 7, u64::MAX - 1, u64::MAX] {
                    for frozen in [false, true] {
                        let c = Clocks { last_now: RoomTime(last), lease_clock: LeaseTime(lc) };
                        let d = advance(c, RoomTime(now), frozen, &cfg);
                        assert!(d.lease_clock >= c.lease_clock, "K1");
                        let moved = d.lease_clock.0 - c.lease_clock.0;
                        assert!(moved <= cap && moved <= now.saturating_sub(last), "K2");
                        assert!(!frozen || moved == 0, "K3");
                        assert!(d.last_now >= c.last_now, "K5");
                    }
                }
            }
        }
    }

    #[test]
    fn a_two_hour_sleep_costs_a_lease_at_most_two_minutes() {
        let cfg = Config::V01;
        let c = Clocks { last_now: RoomTime(1_000), lease_clock: LeaseTime(50) };
        let d = advance(c, RoomTime(1_000 + 2 * 3_600_000), false, &cfg);
        assert_eq!(d.lease_clock, LeaseTime(50 + 120_000));
    }
}
