//! Every constant is a field of [`Config`] (state_machines.md §0).
//!
//! [`Config::new`] is the one checked constructor, so the arithmetic
//! invariants (K1, K2, C5, C7, P9) can be proved for every value it accepts and
//! results from the scaled model carry over to the real values.

/// The raw values. Durations are milliseconds; caps are counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawConfig {
    /// A lease, on the lease clock.
    pub lease: u64,
    /// How long after a lease ends before a claim may be taken over.
    pub grace: u64,
    /// The most one request may move the lease clock.
    pub silence_cap: u64,
    /// The longest a `Quiet` may stretch a lease.
    pub quiet_max: u64,
    /// How long a `done` may sit in Closing.
    pub closing: u64,
    /// How long an agent's unconfirmed stop counts as Stopped (decision A3).
    pub stop_decay: u64,
    /// How long a retry record is kept.
    pub ops_ttl: u64,
    /// How long the audit log is kept.
    pub log_ttl: u64,
    /// How long a Closed claim row is kept.
    pub closed_ttl: u64,
    /// How long a silent Clean seat with no claim is kept.
    pub seat_ttl: u64,
    /// How long a machine token is valid.
    pub token_ttl: u64,
    /// Seats per room.
    pub max_room_seats: u16,
    /// Open claims per seat.
    pub max_claims_per_seat: u16,
    /// Paths per footprint.
    pub max_paths: u16,
    /// Events per batch.
    pub max_batch: u16,
    /// Held and Closing rows.
    pub max_keys: u16,
    /// Closed rows.
    pub max_closed: u16,
    /// Retry records per token.
    pub max_ops_per_token: u16,
    /// News items kept.
    pub max_news: u16,
    /// Record ids remembered as announced.
    pub max_recs: u16,
    /// Events per token per minute.
    pub budget_per_min: u16,
}

/// Why [`Config::new`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// `0 < silence_cap <= lease` does not hold.
    SilenceCap,
    /// `quiet_max >= lease` does not hold.
    QuietMax,
    /// A duration that must be positive is zero.
    ZeroDuration,
    /// A cap is zero.
    ZeroCap,
}

/// A checked configuration. The only ways to get one are [`Config::new`] and
/// the two constants, which a test runs back through `new`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config(RawConfig);

const MINUTE: u64 = 60_000;
const DAY: u64 = 86_400_000;

impl Config {
    /// The values v0.1 ships with (decision C32).
    pub const V01: Config = Config(RawConfig {
        lease: 15 * MINUTE,
        grace: 15 * MINUTE,
        silence_cap: 2 * MINUTE,
        quiet_max: 120 * MINUTE,
        closing: 10 * MINUTE,
        stop_decay: 30 * MINUTE,
        ops_ttl: 120 * MINUTE,
        log_ttl: 30 * DAY,
        closed_ttl: 30 * DAY,
        seat_ttl: 30 * DAY,
        token_ttl: 30 * DAY,
        max_room_seats: 64,
        max_claims_per_seat: 3,
        max_paths: 64,
        max_batch: 32,
        max_keys: 128,
        max_closed: 500,
        max_ops_per_token: 1024,
        max_news: 50,
        max_recs: 200,
        budget_per_min: 600,
    });

    /// The scaled values the exhaustive search uses ("Model value" column).
    pub const MODEL: Config = Config(RawConfig {
        lease: 3,
        grace: 2,
        silence_cap: 1,
        quiet_max: 5,
        closing: 2,
        stop_decay: 4,
        ops_ttl: 3,
        log_ttl: 6,
        closed_ttl: 6,
        seat_ttl: 6,
        token_ttl: 6,
        max_room_seats: 2,
        max_claims_per_seat: 1,
        max_paths: 1,
        max_batch: 2,
        max_keys: 2,
        max_closed: 2,
        max_ops_per_token: 2,
        max_news: 2,
        max_recs: 2,
        budget_per_min: 3,
    });

    /// The checked constructor.
    pub const fn new(r: RawConfig) -> Result<Config, ConfigError> {
        if r.silence_cap == 0 || r.silence_cap > r.lease {
            Err(ConfigError::SilenceCap)
        } else if r.quiet_max < r.lease {
            Err(ConfigError::QuietMax)
        } else if r.grace == 0
            || r.closing == 0
            || r.stop_decay == 0
            || r.ops_ttl == 0
            || r.log_ttl == 0
            || r.closed_ttl == 0
            || r.seat_ttl == 0
            || r.token_ttl == 0
        {
            Err(ConfigError::ZeroDuration)
        } else if r.max_room_seats == 0
            || r.max_claims_per_seat == 0
            || r.max_paths == 0
            || r.max_batch == 0
            || r.max_keys == 0
            || r.max_closed == 0
            || r.max_ops_per_token == 0
            || r.max_news == 0
            || r.max_recs == 0
            || r.budget_per_min == 0
        {
            Err(ConfigError::ZeroCap)
        } else {
            Ok(Config(r))
        }
    }

    /// The values, read-only.
    pub const fn get(&self) -> &RawConfig {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_constants_pass_the_checked_constructor() {
        assert_eq!(Config::new(*Config::V01.get()), Ok(Config::V01));
        assert_eq!(Config::new(*Config::MODEL.get()), Ok(Config::MODEL));
    }

    #[test]
    fn the_constructor_refuses_each_broken_rule() {
        let ok = *Config::V01.get();
        let silence_zero = RawConfig { silence_cap: 0, ..ok };
        let silence_big = RawConfig { silence_cap: ok.lease + 1, ..ok };
        let quiet_small = RawConfig { quiet_max: ok.lease - 1, ..ok };
        let grace_zero = RawConfig { grace: 0, ..ok };
        let cap_zero = RawConfig { max_batch: 0, ..ok };
        assert_eq!(Config::new(silence_zero), Err(ConfigError::SilenceCap));
        assert_eq!(Config::new(silence_big), Err(ConfigError::SilenceCap));
        assert_eq!(Config::new(quiet_small), Err(ConfigError::QuietMax));
        assert_eq!(Config::new(grace_zero), Err(ConfigError::ZeroDuration));
        assert_eq!(Config::new(cap_zero), Err(ConfigError::ZeroCap));
    }
}
