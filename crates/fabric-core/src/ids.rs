//! Field grammars (v01_architecture.md §4.3). A value that does not parse
//! cannot exist: each type has one checked constructor, `parse`, and no other
//! way in. None derives `Default`; none implements `From`.

use core::fmt;

/// The 32 characters of lowercase Crockford base32: op ids use them.
const BASE32: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const HEX: &[u8; 16] = b"0123456789abcdef";

fn lower_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// `prefix` then exactly `N` bytes of `[a-z0-9]`.
fn prefixed<const N: usize>(s: &[u8], prefix: &[u8]) -> Option<[u8; N]> {
    let rest = s.strip_prefix(prefix)?;
    let arr = <[u8; N]>::try_from(rest).ok()?;
    if arr.iter().all(|b| lower_alnum(*b)) { Some(arr) } else { None }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => b.checked_sub(b'0'),
        b'a'..=b'f' => b.checked_sub(b'a')?.checked_add(10),
        _ => None,
    }
}

/// Exactly `2 * N` lowercase hex characters.
fn from_hex<const N: usize>(s: &[u8]) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    let mut it = s.iter();
    for slot in out.iter_mut() {
        let hi = hex_val(*it.next()?)?;
        let lo = hex_val(*it.next()?)?;
        *slot = hi.checked_mul(16)?.checked_add(lo)?;
    }
    if it.next().is_some() { None } else { Some(out) }
}

fn write_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        fmt::Write::write_char(f, char::from(*b))?;
    }
    Ok(())
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        let hi = b.checked_shr(4).and_then(|n| HEX.get(usize::from(n)));
        let lo = HEX.get(usize::from(b & 0x0f));
        if let (Some(h), Some(l)) = (hi, lo) {
            fmt::Write::write_char(f, char::from(*h))?;
            fmt::Write::write_char(f, char::from(*l))?;
        }
    }
    Ok(())
}

macro_rules! prefixed_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $n:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $n]);

        impl $name {
            /// The checked constructor.
            pub fn parse(s: &[u8]) -> Option<$name> {
                prefixed::<$n>(s, $prefix).map($name)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_bytes(f, $prefix)?;
                write_bytes(f, &self.0)
            }
        }
    };
}

prefixed_id!(
    /// A task: `fab-[a-z0-9]{6}`, derived from content (M17).
    TaskId, b"fab-", 6
);
prefixed_id!(
    /// A record: `rec-[a-z0-9]{6}`, derived from content (M17).
    RecId, b"rec-", 6
);
prefixed_id!(
    /// A PHI incident: `inc-[a-z0-9]{6}`, minted by heddle.
    IncidentId, b"inc-", 6
);
prefixed_id!(
    /// A seat, one worktree on one machine: `s-[a-z0-9]{8}`, minted by heddle.
    SeatId, b"s-", 8
);

/// A git commit: `[0-9a-f]{40}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha([u8; 20]);

impl Sha {
    /// The checked constructor.
    pub fn parse(s: &[u8]) -> Option<Sha> {
        from_hex::<20>(s).map(Sha)
    }
}

impl fmt::Display for Sha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

/// A batch's nonce: `[0-9a-f]{32}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nonce([u8; 16]);

impl Nonce {
    /// The checked constructor.
    pub fn parse(s: &[u8]) -> Option<Nonce> {
        from_hex::<16>(s).map(Nonce)
    }
    /// From 16 random bytes drawn at the edge.
    pub const fn from_random(b: [u8; 16]) -> Nonce {
        Nonce(b)
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

/// One event's retry id: 26 lowercase Crockford base32 characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId([u8; 26]);

impl OpId {
    /// The checked constructor.
    pub fn parse(s: &[u8]) -> Option<OpId> {
        let arr = <[u8; 26]>::try_from(s).ok()?;
        if arr.iter().all(|b| BASE32.contains(b)) { Some(OpId(arr)) } else { None }
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_bytes(f, &self.0)
    }
}

/// A seat's secret, 32 bytes, sent as 64 hex characters. The room keeps only
/// its SHA-256. `Debug` never prints it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Proof([u8; 32]);

impl Proof {
    /// The checked constructor.
    pub fn parse(s: &[u8]) -> Option<Proof> {
        from_hex::<32>(s).map(Proof)
    }
    /// From 32 random bytes drawn at the edge.
    pub const fn from_random(b: [u8; 32]) -> Proof {
        Proof(b)
    }
    /// The secret's bytes, for hashing.
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Proof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Proof(..)")
    }
}

/// A machine token's row in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(pub u16);

macro_rules! short_name {
    ($(#[$doc:meta])* $name:ident, $max:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name {
            bytes: [u8; $max],
            len: u8,
        }

        impl $name {
            /// The checked constructor: `[a-z0-9-]`, 1 to the maximum.
            pub fn parse(s: &[u8]) -> Option<$name> {
                let len = u8::try_from(s.len()).ok()?;
                if s.is_empty() || s.len() > $max || !s.iter().all(|b| lower_alnum(*b) || *b == b'-') {
                    return None;
                }
                let mut bytes = [0u8; $max];
                for (slot, b) in bytes.iter_mut().zip(s) {
                    *slot = *b;
                }
                Some($name { bytes, len })
            }

            /// The name's bytes.
            pub fn as_bytes(&self) -> &[u8] {
                self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_bytes(f, self.as_bytes())
            }
        }
    };
}

short_name!(
    /// A person's login: `[a-z0-9-]{1,39}`, set at minting, never from a payload.
    Login, 39
);
short_name!(
    /// A machine's name: `[a-z0-9-]{1,24}`, set at minting.
    Machine, 24
);

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    #[test]
    fn prefixed_ids_round_trip_and_refuse_everything_else() {
        let t = TaskId::parse(b"fab-k3q9x2");
        assert_eq!(t.map(|t| t.to_string()), Some("fab-k3q9x2".to_string()));
        for bad in [&b"fab-k3q9x"[..], b"fab-k3q9x22", b"fab-K3Q9X2", b"fab-k3q9x-", b"rec-k3q9x2", b"", b"fab-"] {
            assert_eq!(TaskId::parse(bad), None, "{bad:?}");
        }
        assert!(SeatId::parse(b"s-abcd1234").is_some());
        assert!(SeatId::parse(b"s-abcd123").is_none());
        assert!(IncidentId::parse(b"inc-000aaa").is_some());
        assert!(RecId::parse(b"rec-a1b2c3").is_some());
    }

    #[test]
    fn hex_types_round_trip_and_refuse_upper_case_and_bad_lengths() {
        let s = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(Sha::parse(s.as_bytes()).map(|x| x.to_string()), Some(s.to_string()));
        assert!(Sha::parse(s.to_uppercase().as_bytes()).is_none());
        assert!(Sha::parse(&s.as_bytes()[..39]).is_none());
        assert!(Nonce::parse(b"00112233445566778899aabbccddeeff").is_some());
        assert!(Nonce::parse(b"00112233445566778899aabbccddeef").is_none());
        let p = Proof::parse(b"ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00");
        assert_eq!(p.map(|p| format!("{p:?}")), Some("Proof(..)".to_string()));
    }

    #[test]
    fn op_ids_use_crockford_base32_only() {
        assert!(OpId::parse(b"01hz3k4m5n6p7q8r9s0t1v2w3x").is_some());
        assert!(OpId::parse(b"01hz3k4m5n6p7q8r9s0t1v2w3i").is_none());
        assert!(OpId::parse(b"01hz3k4m5n6p7q8r9s0t1v2w3").is_none());
    }

    #[test]
    fn logins_and_machines_are_bounded() {
        assert_eq!(Login::parse(b"xavier-b").map(|l| l.to_string()), Some("xavier-b".to_string()));
        assert!(Login::parse(&[b'a'; 39]).is_some());
        assert!(Login::parse(&[b'a'; 40]).is_none());
        assert!(Login::parse(b"").is_none());
        assert!(Login::parse(b"Xavier").is_none());
        assert!(Machine::parse(&[b'm'; 25]).is_none());
    }
}
