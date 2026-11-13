//! M23: asset class and flow (phi.md §2, §3).
//!
//! Every asset carries Xavier's two tags, "may it contain PHI" and "is it
//! PHI-safe", and a third, "can it never be PHI-safe". The class is a total
//! function of the three, and "may contain PHI but not PHI-safe" has no value
//! (invariant A1). An asset nobody has vetted is `Unvetted`, which is not a
//! class, and every thread is denied it.

/// An asset's class. Not ordered: see [`may_retag`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetClass {
    /// Holds no PHI and is not PHI-safe: GitHub, the room, this repo.
    Open,
    /// Can never be PHI-safe: e-sign software, a public homepage.
    Walled,
    /// PHI-safe and holds no PHI: public files on BAA-covered storage.
    SafeClean,
    /// PHI-safe and may hold PHI: a system of record.
    SafeBearing,
}

/// Whether anybody has answered the two questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vetting {
    /// Nobody has: a mailbox, a shared drive, a connector nobody looked at.
    Unvetted,
    /// Classed.
    Classed(AssetClass),
}

/// A tag combination that has no class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IllegalTags {
    /// May contain PHI and is not PHI-safe.
    PhiNotSafe,
    /// PHI-safe and never PHI-safe at once.
    SafeAndNeverSafe,
}

/// The class, from the three tags (A1).
pub fn classify(may_contain_phi: bool, phi_safe: bool, never_safe: bool) -> Result<AssetClass, IllegalTags> {
    match (may_contain_phi, phi_safe, never_safe) {
        (true, false, _) => Err(IllegalTags::PhiNotSafe),
        (_, true, true) => Err(IllegalTags::SafeAndNeverSafe),
        (false, false, false) => Ok(AssetClass::Open),
        (false, false, true) => Ok(AssetClass::Walled),
        (false, true, false) => Ok(AssetClass::SafeClean),
        (true, true, false) => Ok(AssetClass::SafeBearing),
    }
}

/// The only class moves between registry versions (A2). Nothing leaves
/// Walled or SafeBearing.
pub fn may_retag(old: AssetClass, new: AssetClass) -> bool {
    old == new
        || matches!(
            (old, new),
            (AssetClass::Open, AssetClass::SafeClean)
                | (AssetClass::Open, AssetClass::SafeBearing)
                | (AssetClass::SafeClean, AssetClass::SafeBearing)
        )
}

/// Which side a thread is on. In v0.1 heddle only ever builds `PhiFree`;
/// `PhiSafe` exists so that `flow` is total and invariant A3 can be checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThreadPool {
    /// The PHI-free side, where every Fabric thread runs.
    PhiFree,
    /// The PHI-safe side. Does not exist yet.
    PhiSafe,
}

/// Who moves the data: a thread, or a system (a pipeline, a backup, a log
/// shipper), classed like any asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A conversation.
    Thread(ThreadPool),
    /// A system, by its class.
    System(AssetClass),
}

/// What the subject does to the asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// Fetch a whole immutable version by its id.
    PullById,
    /// Ask a question of it. Queries count: the question lands in its logs.
    Query,
    /// Write to it.
    Push,
}

/// Why a flow is denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// The PHI-free side holds no credential for it.
    NoCredential,
    /// A PHI-safe subject may not touch an open asset: queries count.
    QueriesCount,
    /// PHI touched this Open or Walled asset; it is out of service.
    Quarantined,
    /// Nobody has vetted it.
    Unvetted,
    /// The registry must say SafeBearing before the write (A6).
    RetagFirst,
    /// The BAA covering it has lapsed.
    BaaLapsed,
    /// The pools do not match.
    WrongPool,
}

/// A flow's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Go ahead.
    Allow,
    /// Go ahead, by id only, through the read-only by-id port.
    AllowById,
    /// Not a push: the PHI-safe side pulls reviewed commits itself.
    DestinationPulls,
    /// No.
    Deny(DenyReason),
}

/// Days since 1970-01-01, on the edge's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date(pub u32);

/// What `flow` needs to know about the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetView {
    /// Its class, or Unvetted.
    pub vetting: Vetting,
    /// PHI touched it (history; never cleared).
    pub incident: bool,
    /// The last day its BAA covers, for a Safe asset a third party holds.
    pub baa_expires: Option<Date>,
}

/// The one flow function (A3, A4, A6).
pub fn flow(src: Subject, op: Op, dst: &AssetView, today: Date) -> Flow {
    let class = match dst.vetting {
        Vetting::Unvetted => return Flow::Deny(DenyReason::Unvetted),
        Vetting::Classed(c) => c,
    };
    let open_side = matches!(class, AssetClass::Open | AssetClass::Walled);
    if dst.incident && open_side {
        return Flow::Deny(DenyReason::Quarantined);
    }
    if !open_side && dst.baa_expires.is_some_and(|d| d < today) {
        return Flow::Deny(DenyReason::BaaLapsed);
    }
    match src {
        Subject::Thread(ThreadPool::PhiFree)
        | Subject::System(AssetClass::Open)
        | Subject::System(AssetClass::Walled)
        | Subject::System(AssetClass::SafeClean) => match (class, op) {
            (AssetClass::Open | AssetClass::Walled, Op::PullById | Op::Query | Op::Push) => Flow::Allow,
            (AssetClass::SafeClean, Op::PullById) => Flow::AllowById,
            (AssetClass::SafeClean | AssetClass::SafeBearing, Op::Query) => Flow::Deny(DenyReason::NoCredential),
            (AssetClass::SafeBearing, Op::PullById) => Flow::Deny(DenyReason::NoCredential),
            (AssetClass::SafeClean | AssetClass::SafeBearing, Op::Push) => Flow::DestinationPulls,
        },
        Subject::Thread(ThreadPool::PhiSafe) => match (class, op) {
            (AssetClass::Open | AssetClass::Walled, Op::PullById | Op::Query | Op::Push) => {
                Flow::Deny(DenyReason::QueriesCount)
            }
            (AssetClass::SafeClean, Op::PullById | Op::Query) => Flow::Allow,
            (AssetClass::SafeClean, Op::Push) => Flow::Deny(DenyReason::RetagFirst),
            (AssetClass::SafeBearing, Op::PullById | Op::Query | Op::Push) => Flow::Allow,
        },
        // A SafeBearing system: a mirror job may fetch open content by id;
        // sending is allowed only to another SafeBearing asset.
        Subject::System(AssetClass::SafeBearing) => match (class, op) {
            (AssetClass::Open | AssetClass::Walled, Op::PullById) => Flow::AllowById,
            (AssetClass::Open | AssetClass::Walled, Op::Query | Op::Push) => Flow::Deny(DenyReason::QueriesCount),
            (AssetClass::SafeClean, Op::PullById | Op::Query) => Flow::Allow,
            (AssetClass::SafeClean, Op::Push) => Flow::Deny(DenyReason::RetagFirst),
            (AssetClass::SafeBearing, Op::PullById | Op::Query | Op::Push) => Flow::Allow,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const CLASSES: [AssetClass; 4] = [AssetClass::Open, AssetClass::Walled, AssetClass::SafeClean, AssetClass::SafeBearing];

    fn subjects() -> Vec<Subject> {
        let mut v = alloc::vec![Subject::Thread(ThreadPool::PhiFree), Subject::Thread(ThreadPool::PhiSafe)];
        v.extend(CLASSES.iter().map(|c| Subject::System(*c)));
        v
    }

    fn destinations() -> Vec<AssetView> {
        let mut v = Vec::new();
        let vettings = [Vetting::Unvetted].into_iter().chain(CLASSES.iter().map(|c| Vetting::Classed(*c)));
        for vetting in vettings {
            for incident in [false, true] {
                for baa_expires in [None, Some(Date(9)), Some(Date(11))] {
                    v.push(AssetView { vetting, incident, baa_expires });
                }
            }
        }
        v
    }

    /// Where PHI-derived data can already be.
    fn tainted_subject(s: Subject) -> bool {
        matches!(s, Subject::Thread(ThreadPool::PhiSafe) | Subject::System(AssetClass::SafeBearing))
    }
    fn tainted_asset(a: &AssetView) -> bool {
        a.vetting == Vetting::Classed(AssetClass::SafeBearing)
    }
    /// Where PHI-derived data must never arrive.
    fn clean_side_subject(s: Subject) -> bool {
        !tainted_subject(s) && !matches!(s, Subject::System(AssetClass::SafeClean))
    }
    fn clean_side_asset(a: &AssetView) -> bool {
        matches!(a.vetting, Vetting::Unvetted | Vetting::Classed(AssetClass::Open) | Vetting::Classed(AssetClass::Walled))
    }

    #[test]
    fn a1_the_illegal_pair_has_no_class() {
        for phi in [false, true] {
            for safe in [false, true] {
                for never in [false, true] {
                    let c = classify(phi, safe, never);
                    assert_eq!(c.is_err(), (phi && !safe) || (safe && never), "{phi} {safe} {never}");
                    if let Ok(AssetClass::Open | AssetClass::Walled) = c {
                        assert!(!phi);
                    }
                }
            }
        }
    }

    #[test]
    fn a2_nothing_leaves_walled_or_safe_bearing() {
        for old in CLASSES {
            for new in CLASSES {
                if old != new && matches!(old, AssetClass::Walled | AssetClass::SafeBearing) {
                    assert!(!may_retag(old, new));
                }
            }
        }
        assert!(may_retag(AssetClass::Open, AssetClass::SafeBearing));
        assert!(!may_retag(AssetClass::SafeClean, AssetClass::Open));
    }

    #[test]
    fn a3_no_allowed_flow_moves_phi_derived_data_to_the_clean_side() {
        let today = Date(10);
        for s in subjects() {
            for op in [Op::PullById, Op::Query, Op::Push] {
                for d in destinations() {
                    let f = flow(s, op, &d, today);
                    let allowed = matches!(f, Flow::Allow | Flow::AllowById);
                    if !allowed {
                        continue;
                    }
                    // Data leaves the subject on a push, and on a query (queries count).
                    if matches!(op, Op::Push | Op::Query) && tainted_subject(s) {
                        assert!(!clean_side_asset(&d), "A3: {s:?} {op:?} {d:?} -> {f:?}");
                    }
                    // Data reaches the subject on a pull or a query.
                    if matches!(op, Op::PullById | Op::Query) && tainted_asset(&d) {
                        assert!(!clean_side_subject(s), "A3: {s:?} {op:?} {d:?} -> {f:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a4_a6_quarantined_unvetted_lapsed_and_write_before_retag_are_denied() {
        let today = Date(10);
        for s in subjects() {
            for op in [Op::PullById, Op::Query, Op::Push] {
                for d in destinations() {
                    let f = flow(s, op, &d, today);
                    let quarantined = d.incident && clean_side_asset(&d) && d.vetting != Vetting::Unvetted;
                    let lapsed = !clean_side_asset(&d) && d.baa_expires.is_some_and(|x| x < today);
                    if d.vetting == Vetting::Unvetted {
                        assert_eq!(f, Flow::Deny(DenyReason::Unvetted));
                    } else if quarantined {
                        assert_eq!(f, Flow::Deny(DenyReason::Quarantined));
                    } else if lapsed {
                        assert_eq!(f, Flow::Deny(DenyReason::BaaLapsed));
                    }
                }
            }
        }
        let clean = AssetView { vetting: Vetting::Classed(AssetClass::SafeClean), incident: false, baa_expires: None };
        assert_eq!(flow(Subject::Thread(ThreadPool::PhiSafe), Op::Push, &clean, today), Flow::Deny(DenyReason::RetagFirst));
    }
}
