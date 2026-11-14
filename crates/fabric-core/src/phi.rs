//! M13: the PHI label, per seat and per conversation; M24: a source under an
//! incident; and the room's copy of each seat's label.
//!
//! The label answers one question: could a model vendor already hold patient
//! text from this thread, or be about to? Labels only go up. As Xavier ruled
//! (decisions B8, B9, B12a), the brake has two exits only, a rewind or a
//! migration, and fails closed: a detector that cannot run is a hit under rule
//! R-00. Here a fault *is* a hit (the caller passes `RuleId::R00`), so there is
//! no separate fault path that could pass (T12).

use crate::ids::{IncidentId, McpServerId};

/// A stage-1 rule. `R00` is "the detector did not run".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleId {
    /// The detector did not run: rules would not load, a fault, a passed
    /// deadline, input that could not be read.
    R00,
    /// Nine digits shaped like an SSN, near a keyword.
    R01,
    /// A Medicare beneficiary id shape, near a keyword.
    R02,
    /// A medical record number after its keyword.
    R03,
    /// A full date after a date-of-birth keyword.
    R04,
    /// Data-shaped text with two or more direct-identifier field names.
    R05,
    /// A path on the restricted-paths list.
    R06,
    /// Content too large to scan, or that does not decode.
    R07,
    /// Content the rules cannot read (images, PDFs, archives): counted only.
    R08,
    /// A dashed SSN shape with no keyword.
    R09,
    /// An HL7 v2 patient segment.
    R10,
    /// An X12 subscriber or patient segment with a date of birth nearby.
    R11,
    /// A FHIR Patient resource.
    R12,
}

/// Why a seat is Exposed. Travels in `Taint`, so the room can refuse an exit
/// record the cause does not allow (T9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cause {
    /// A tool result was replaced before the model saw it.
    ResultMasked,
    /// The loop was stopped before the next model call.
    BatchUnsent,
    /// The model wrote the text into a tool call.
    ToolInput,
    /// Found after a model had it, or it must be assumed so.
    FoundLater,
}

/// A type with no values: a variant that holds it cannot be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Never {}

/// Why a seat is Sealed. Sealed is the brake left on for good, not an exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealReason {
    /// A person confirmed real patient data that a rewind cannot recall.
    Confirmed,
    /// Moved to PHI-safe infrastructure. There is nowhere to migrate to in
    /// v0.1, so this variant cannot be built.
    Migrated(Never),
}

/// The seat's local label, kept in its own file (L0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phi {
    /// Nothing found.
    Clean,
    /// A prompt a person typed was blocked before it was sent.
    Held,
    /// The thread cannot proceed. Two exits: a rewind, or (later) a migration.
    Exposed {
        /// The incident.
        incident: IncidentId,
        /// Why.
        cause: Cause,
        /// Hits since the incident opened.
        hits: u16,
        /// Whether the room has acknowledged the `Taint`.
        acked: bool,
    },
    /// Final.
    Sealed {
        /// Why.
        reason: SealReason,
    },
}

/// The conversation's label, in the machine-wide quarantine set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvPhi {
    /// Nothing found.
    Clean,
    /// Blocked text may sit in local files until `heddle phi scrub`.
    Residue,
    /// The conversation fired: braked wherever it is resumed or forked.
    Exposed(IncidentId),
    /// Final.
    Sealed,
}

/// The room's copy of a seat's label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatPhi {
    /// Nothing recorded.
    Clean,
    /// Tainted.
    Exposed {
        /// The incident.
        incident: IncidentId,
        /// Why.
        cause: Cause,
    },
    /// Final.
    Sealed,
}

/// A short code `heddle phi scrub` prints: 8 lowercase base32 characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scrubbed([u8; 8]);

impl Scrubbed {
    /// The checked constructor. A rewind record cannot be built without one.
    pub fn parse(s: &[u8]) -> Option<Scrubbed> {
        let arr = <[u8; 8]>::try_from(s).ok()?;
        let ok = arr
            .iter()
            .all(|b| b"0123456789abcdefghjkmnpqrstvwxyz".contains(b));
        if ok { Some(Scrubbed(arr)) } else { None }
    }
    /// The code's bytes.
    pub const fn code(&self) -> &[u8; 8] {
        &self.0
    }
}

/// An exit record *records* an exit; it is not one. There is no false-alarm
/// value (T11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitRecord {
    /// The person rewound and scrubbed. `real`: was it patient data?
    Rewound {
        /// The scrub's code.
        scrub: Scrubbed,
        /// Was it real patient data?
        real: bool,
    },
    /// Real patient data that a rewind cannot recall: the seat is Sealed.
    Confirmed,
}

/// Which exit record a verified snapshot shows, without its scrub code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeenRecord {
    /// A rewind.
    Rewound,
    /// A confirmation.
    Confirmed,
}

/// What reaches `phi_step`. The incident ids are minted at the edge, at random;
/// one is used only when the step opens a new incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhiEvent {
    /// The pre-filter fired on a prompt, or could not run (`R00`).
    PromptHit {
        /// The rule.
        rule: RuleId,
        /// A person typed it (by the conservative rule of M13).
        person_typed: bool,
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// A prompt passed the pre-filter.
    PromptClean,
    /// The pre-filter fired on a tool result, or could not run.
    ResultHit {
        /// The rule.
        rule: RuleId,
        /// The adapter can replace this tool's result.
        can_replace: bool,
        /// The adapter can stop before the next model call.
        can_stop: bool,
        /// The result passed a model before the hook (web fetch, a subagent).
        passed_model: bool,
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// The batch about to be sent still holds a hit.
    BatchHit {
        /// The rule.
        rule: RuleId,
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// The model wrote the text into a tool call.
    ToolInputHit {
        /// The rule.
        rule: RuleId,
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// The scan of bytes added to the transcript since the last hook.
    TranscriptHit {
        /// The rule.
        rule: RuleId,
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// A raise marker: `heddle phi flag`, or a git hook's PHI rule.
    Flag {
        /// A fresh incident id.
        incident: IncidentId,
    },
    /// A verified snapshot shows this seat Exposed.
    RoomSaysExposed {
        /// The room's incident.
        incident: IncidentId,
        /// The room's cause.
        cause: Cause,
    },
    /// A verified snapshot shows this seat Sealed.
    RoomSaysSealed,
    /// The rewind proof: heddle re-read the conversation, and every entry
    /// behind the resent prompt was written before the flagged turn began.
    /// Lowers that incident's label to Clean; nothing else moves.
    ChainProvedClear {
        /// The incident the proof is for.
        incident: IncidentId,
    },
    /// A verified snapshot shows this incident, or an exit record for it.
    TaintAcked {
        /// The incident the room shows.
        incident: IncidentId,
    },
    /// A verified snapshot's last exit record for this seat.
    RecordSeen {
        /// The record's incident.
        incident: IncidentId,
        /// Which record.
        record: SeenRecord,
        /// How many hits the admin reviewed.
        upto: u16,
        /// A fresh incident id, for hits that came after the admin looked.
        fresh: IncidentId,
    },
}

/// What the hook must do because of the step. The hook's own decision type
/// carries the block, deny, replace or stop; these flags say which.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhiEffects {
    /// Block the prompt without echoing it.
    pub block_prompt: bool,
    /// Deny the tool call.
    pub deny_call: bool,
    /// Replace the tool result with a fixed sentence.
    pub replace_result: bool,
    /// Stop the loop before the next model call.
    pub stop_loop: bool,
    /// Send (or resend) `Taint` for this incident and cause.
    pub taint: Option<(IncidentId, Cause)>,
    /// Put the conversation in the quarantine set as Exposed.
    pub quarantine: Option<IncidentId>,
    /// Mark the conversation Residue.
    pub residue: bool,
    /// Open a source incident (M24) for this tool result.
    pub source_incident: Option<IncidentId>,
    /// Count the rule, for tuning; add it to the incident note when Exposed.
    pub count: Option<RuleId>,
    /// A rewind was recorded: the conversation becomes Clean once this machine
    /// holds the scrub record, Residue until then.
    pub conv_rewound: bool,
    /// The conversation is Sealed.
    pub conv_sealed: bool,
}

const NONE: PhiEffects = PhiEffects {
    block_prompt: false,
    deny_call: false,
    replace_result: false,
    stop_loop: false,
    taint: None,
    quarantine: None,
    residue: false,
    source_incident: None,
    count: None,
    conv_rewound: false,
    conv_sealed: false,
};

fn exposed(incident: IncidentId, cause: Cause) -> Phi {
    Phi::Exposed {
        incident,
        cause,
        hits: 1,
        acked: false,
    }
}

fn brake(
    incident: IncidentId,
    cause: Cause,
    rule: Option<RuleId>,
    fx: PhiEffects,
) -> (Phi, PhiEffects) {
    let fx = PhiEffects {
        taint: Some((incident, cause)),
        quarantine: Some(incident),
        count: rule,
        ..fx
    };
    (exposed(incident, cause), fx)
}

/// What a hit does to a label that is already Exposed: count it.
fn another_hit(p: Phi, rule: Option<RuleId>, fx: PhiEffects) -> (Phi, PhiEffects) {
    match p {
        Phi::Exposed {
            incident,
            cause,
            hits,
            acked,
        } => (
            Phi::Exposed {
                incident,
                cause,
                hits: hits.saturating_add(1),
                acked,
            },
            PhiEffects { count: rule, ..fx },
        ),
        Phi::Clean | Phi::Held | Phi::Sealed { .. } => (p, PhiEffects { count: rule, ..fx }),
    }
}

/// The M13 laptop table. Total; Sealed absorbs everything (T11).
pub fn phi_step(p: Phi, ev: PhiEvent) -> (Phi, PhiEffects) {
    if let Phi::Sealed { .. } = p {
        return (p, NONE);
    }
    let open = matches!(p, Phi::Clean | Phi::Held);
    match ev {
        PhiEvent::PromptHit {
            rule,
            person_typed,
            incident,
        } => {
            let fx = PhiEffects {
                block_prompt: true,
                ..NONE
            };
            if !open {
                another_hit(p, Some(rule), fx)
            } else if person_typed {
                (
                    Phi::Held,
                    PhiEffects {
                        residue: true,
                        count: Some(rule),
                        ..fx
                    },
                )
            } else {
                brake(incident, Cause::FoundLater, Some(rule), fx)
            }
        }
        PhiEvent::PromptClean => match p {
            Phi::Held => (Phi::Clean, NONE),
            Phi::Clean | Phi::Exposed { .. } | Phi::Sealed { .. } => (p, NONE),
        },
        PhiEvent::ResultHit {
            rule,
            can_replace,
            can_stop,
            passed_model,
            incident,
        } => {
            let fx = PhiEffects {
                replace_result: can_replace,
                stop_loop: can_stop,
                ..NONE
            };
            if !open {
                return another_hit(p, Some(rule), fx);
            }
            let cause = if passed_model {
                Cause::FoundLater
            } else if can_replace {
                Cause::ResultMasked
            } else if can_stop {
                Cause::BatchUnsent
            } else {
                Cause::FoundLater
            };
            brake(
                incident,
                cause,
                Some(rule),
                PhiEffects {
                    source_incident: Some(incident),
                    ..fx
                },
            )
        }
        PhiEvent::BatchHit { rule, incident } => {
            let fx = PhiEffects {
                stop_loop: true,
                ..NONE
            };
            if open {
                brake(incident, Cause::BatchUnsent, Some(rule), fx)
            } else {
                another_hit(p, Some(rule), fx)
            }
        }
        PhiEvent::ToolInputHit { rule, incident } => {
            let fx = PhiEffects {
                deny_call: true,
                ..NONE
            };
            if open {
                brake(incident, Cause::ToolInput, Some(rule), fx)
            } else {
                another_hit(p, Some(rule), fx)
            }
        }
        PhiEvent::TranscriptHit { rule, incident } => {
            if open {
                brake(incident, Cause::FoundLater, Some(rule), NONE)
            } else {
                another_hit(p, Some(rule), NONE)
            }
        }
        PhiEvent::Flag { incident } => {
            if open {
                brake(incident, Cause::FoundLater, None, NONE)
            } else {
                another_hit(p, None, NONE)
            }
        }
        PhiEvent::RoomSaysExposed { incident, cause } => {
            if open {
                (
                    Phi::Exposed {
                        incident,
                        cause,
                        hits: 1,
                        acked: true,
                    },
                    PhiEffects {
                        quarantine: Some(incident),
                        ..NONE
                    },
                )
            } else {
                (p, NONE)
            }
        }
        PhiEvent::ChainProvedClear { incident: proved } => match p {
            Phi::Exposed { incident, .. } if incident == proved => (Phi::Clean, NONE),
            Phi::Clean | Phi::Held | Phi::Exposed { .. } | Phi::Sealed { .. } => (p, NONE),
        },
        PhiEvent::RoomSaysSealed => (
            Phi::Sealed {
                reason: SealReason::Confirmed,
            },
            PhiEffects {
                conv_sealed: true,
                ..NONE
            },
        ),
        PhiEvent::TaintAcked { incident: seen } => match p {
            Phi::Exposed {
                incident,
                cause,
                hits,
                ..
            } if incident == seen => (
                Phi::Exposed {
                    incident,
                    cause,
                    hits,
                    acked: true,
                },
                NONE,
            ),
            Phi::Exposed { .. } | Phi::Clean | Phi::Held | Phi::Sealed { .. } => (p, NONE),
        },
        PhiEvent::RecordSeen {
            incident: seen,
            record,
            upto,
            fresh,
        } => match p {
            // RecordSeen fires only for exactly the local incident id.
            Phi::Exposed {
                incident,
                cause,
                hits,
                ..
            } if incident == seen => match record {
                SeenRecord::Confirmed => (
                    Phi::Sealed {
                        reason: SealReason::Confirmed,
                    },
                    PhiEffects {
                        conv_sealed: true,
                        ..NONE
                    },
                ),
                SeenRecord::Rewound if hits <= upto => (
                    Phi::Clean,
                    PhiEffects {
                        conv_rewound: true,
                        ..NONE
                    },
                ),
                SeenRecord::Rewound => {
                    let left = hits.saturating_sub(upto);
                    let next = Phi::Exposed {
                        incident: fresh,
                        cause,
                        hits: left,
                        acked: false,
                    };
                    (
                        next,
                        PhiEffects {
                            taint: Some((fresh, cause)),
                            quarantine: Some(fresh),
                            ..NONE
                        },
                    )
                }
            },
            Phi::Exposed { .. } | Phi::Clean | Phi::Held | Phi::Sealed { .. } => (p, NONE),
        },
    }
}

/// At every hook while Exposed and not acknowledged, resend `Taint` (T10).
pub fn resend(p: &Phi) -> Option<(IncidentId, Cause)> {
    match p {
        Phi::Exposed {
            incident,
            cause,
            acked: false,
            ..
        } => Some((*incident, *cause)),
        Phi::Exposed { .. } | Phi::Clean | Phi::Held | Phi::Sealed { .. } => None,
    }
}

/// What the hooks enforce: the highest of the three labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brake {
    /// No brake.
    Clear,
    /// A typed prompt was held: expansions and compaction are blocked.
    Held,
    /// Every tool denied, every prompt blocked.
    Exposed(IncidentId),
    /// The same, for good.
    Sealed,
}

impl Brake {
    const fn rank(&self) -> u8 {
        match self {
            Brake::Clear => 0,
            Brake::Held => 1,
            Brake::Exposed(_) => 2,
            Brake::Sealed => 3,
        }
    }
}

/// The effective label: the highest of the seat's, the conversation's and
/// the room's. Ties keep the first: seat, then conversation, then room.
pub fn effective(seat: &Phi, conv: &ConvPhi, room: Option<&SeatPhi>) -> Brake {
    let s = match seat {
        Phi::Clean => Brake::Clear,
        Phi::Held => Brake::Held,
        Phi::Exposed { incident, .. } => Brake::Exposed(*incident),
        Phi::Sealed { .. } => Brake::Sealed,
    };
    let c = match conv {
        ConvPhi::Clean | ConvPhi::Residue => Brake::Clear,
        ConvPhi::Exposed(i) => Brake::Exposed(*i),
        ConvPhi::Sealed => Brake::Sealed,
    };
    let r = match room {
        None | Some(SeatPhi::Clean) => Brake::Clear,
        Some(SeatPhi::Exposed { incident, .. }) => Brake::Exposed(*incident),
        Some(SeatPhi::Sealed) => Brake::Sealed,
    };
    let mut best = s;
    for b in [c, r] {
        if b.rank() > best.rank() {
            best = b;
        }
    }
    best
}

/// Why the room refuses an exit record (the M13 room table, in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordRefusal {
    /// The seat is Clean.
    NotExposed,
    /// The seat is Sealed.
    AlreadySealed,
    /// The seat is Exposed under another incident.
    WrongIncident,
    /// A rewind of real patient data a vendor already holds (T9).
    RewoundNotAllowed,
}

/// `Admin RecordPhi`, for an existing seat. The room cannot see a rewind; it
/// checks the incident, the cause and that a rewind carries a scrub code.
pub fn room_record(
    p: SeatPhi,
    incident: IncidentId,
    record: ExitRecord,
) -> Result<SeatPhi, RecordRefusal> {
    match p {
        SeatPhi::Clean => Err(RecordRefusal::NotExposed),
        SeatPhi::Sealed => Err(RecordRefusal::AlreadySealed),
        SeatPhi::Exposed { incident: i, .. } if i != incident => Err(RecordRefusal::WrongIncident),
        SeatPhi::Exposed { cause, .. } => match record {
            ExitRecord::Confirmed => Ok(SeatPhi::Sealed),
            ExitRecord::Rewound { real: false, .. } => Ok(SeatPhi::Clean),
            ExitRecord::Rewound { real: true, .. } => match cause {
                Cause::ResultMasked | Cause::BatchUnsent => Ok(SeatPhi::Clean),
                Cause::ToolInput | Cause::FoundLater => Err(RecordRefusal::RewoundNotAllowed),
            },
        },
    }
}

/// `Taint` for a Clean seat whose last exit record is not for this incident
/// moves it to Exposed; anything else is `NoChange` (the event is idempotent).
/// The room then frees every claim the seat holds.
pub fn room_taint(
    p: SeatPhi,
    last_record: Option<IncidentId>,
    incident: IncidentId,
    cause: Cause,
) -> Option<SeatPhi> {
    match p {
        SeatPhi::Clean if last_record != Some(incident) => {
            Some(SeatPhi::Exposed { incident, cause })
        }
        SeatPhi::Clean | SeatPhi::Exposed { .. } | SeatPhi::Sealed => None,
    }
}

/// M24: where a tool-result hit came from. A source key never leaves the
/// machine and is never rendered (Q2); the folder is held by the edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKey {
    /// A file tool's folder (the path stays in the local incident file).
    Folder,
    /// An MCP server.
    McpServer(McpServerId),
    /// A shell or web tool: the thread halts but no source is denied.
    Unknown,
}

/// A source incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceIncident {
    /// Denied on this machine until the room records the incident.
    Open {
        /// The incident.
        inc: IncidentId,
        /// The source.
        source: SourceKey,
    },
    /// The room recorded the incident.
    Recorded {
        /// The source.
        source: SourceKey,
        /// The record seen: a false alarm puts the source back in service.
        record: SeenRecord,
        /// Whether the record said real patient data.
        real: bool,
    },
}

/// Q1: an incident closes only through a recorded exit for the same id.
pub fn source_step(
    s: SourceIncident,
    seen: IncidentId,
    record: SeenRecord,
    real: bool,
) -> SourceIncident {
    match s {
        SourceIncident::Open { inc, source } if inc == seen => SourceIncident::Recorded {
            source,
            record,
            real,
        },
        SourceIncident::Open { .. } | SourceIncident::Recorded { .. } => s,
    }
}

/// Whether the source is still denied. Only a rewind of a false alarm puts it
/// back in service; real patient data keeps it denied until a person acts.
pub fn source_denied(s: &SourceIncident) -> bool {
    match s {
        SourceIncident::Open { source, .. } => !matches!(source, SourceKey::Unknown),
        SourceIncident::Recorded {
            record: SeenRecord::Rewound,
            real: false,
            ..
        } => false,
        SourceIncident::Recorded { source, .. } => !matches!(source, SourceKey::Unknown),
    }
}

/// A label as one line of text, for the file that holds it (L0).
pub fn encode_phi(p: &Phi) -> alloc::string::String {
    use core::fmt::Write;
    let mut s = alloc::string::String::new();
    let _ = match p {
        Phi::Clean => write!(s, "clean"),
        Phi::Held => write!(s, "held"),
        Phi::Exposed {
            incident,
            cause,
            hits,
            acked,
        } => {
            let c = match cause {
                Cause::ResultMasked => "result_masked",
                Cause::BatchUnsent => "batch_unsent",
                Cause::ToolInput => "tool_input",
                Cause::FoundLater => "found_later",
            };
            write!(s, "exposed {incident} {c} {hits} {}", u8::from(*acked))
        }
        Phi::Sealed { .. } => write!(s, "sealed"),
    };
    s
}

/// The checked reader for [`encode_phi`]. `None` for anything else; the
/// caller reads `None` as braked (V5), never as Clean.
pub fn decode_phi(line: &[u8]) -> Option<Phi> {
    let line = line.trim_ascii();
    match line {
        b"clean" => return Some(Phi::Clean),
        b"held" => return Some(Phi::Held),
        b"sealed" => {
            return Some(Phi::Sealed {
                reason: SealReason::Confirmed,
            });
        }
        _ => {}
    }
    let mut parts = line.split(|b| *b == b' ');
    if parts.next()? != b"exposed" {
        return None;
    }
    let incident = IncidentId::parse(parts.next()?)?;
    let cause = match parts.next()? {
        b"result_masked" => Cause::ResultMasked,
        b"batch_unsent" => Cause::BatchUnsent,
        b"tool_input" => Cause::ToolInput,
        b"found_later" => Cause::FoundLater,
        _ => return None,
    };
    let hits = core::str::from_utf8(parts.next()?)
        .ok()?
        .parse::<u16>()
        .ok()?;
    let acked = match parts.next()? {
        b"0" => false,
        b"1" => true,
        _ => return None,
    };
    if parts.next().is_some() || hits == 0 {
        return None;
    }
    Some(Phi::Exposed {
        incident,
        cause,
        hits,
        acked,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn inc(s: &[u8]) -> IncidentId {
        IncidentId::parse(s).expect("a valid incident id")
    }

    fn labels() -> Vec<Phi> {
        let mut v = alloc::vec![
            Phi::Clean,
            Phi::Held,
            Phi::Sealed {
                reason: SealReason::Confirmed
            }
        ];
        for cause in [
            Cause::ResultMasked,
            Cause::BatchUnsent,
            Cause::ToolInput,
            Cause::FoundLater,
        ] {
            for hits in [1, 2, u16::MAX] {
                for acked in [false, true] {
                    v.push(Phi::Exposed {
                        incident: inc(b"inc-aaaaaa"),
                        cause,
                        hits,
                        acked,
                    });
                }
            }
        }
        v
    }

    fn events() -> Vec<PhiEvent> {
        let i = inc(b"inc-bbbbbb");
        let a = inc(b"inc-aaaaaa");
        let mut v = alloc::vec![
            PhiEvent::PromptClean,
            PhiEvent::Flag { incident: i },
            PhiEvent::RoomSaysExposed {
                incident: i,
                cause: Cause::FoundLater
            },
            PhiEvent::RoomSaysSealed,
            PhiEvent::ChainProvedClear { incident: a },
            PhiEvent::ChainProvedClear { incident: i },
            PhiEvent::TaintAcked { incident: a },
            PhiEvent::TaintAcked { incident: i },
        ];
        for rule in [RuleId::R00, RuleId::R01, RuleId::R10] {
            for person_typed in [false, true] {
                v.push(PhiEvent::PromptHit {
                    rule,
                    person_typed,
                    incident: i,
                });
            }
            for (can_replace, can_stop, passed_model) in [
                (true, true, false),
                (false, true, false),
                (false, false, false),
                (true, true, true),
            ] {
                v.push(PhiEvent::ResultHit {
                    rule,
                    can_replace,
                    can_stop,
                    passed_model,
                    incident: i,
                });
            }
            v.push(PhiEvent::BatchHit { rule, incident: i });
            v.push(PhiEvent::ToolInputHit { rule, incident: i });
            v.push(PhiEvent::TranscriptHit { rule, incident: i });
        }
        for record in [SeenRecord::Rewound, SeenRecord::Confirmed] {
            for upto in [0, 1, 2, u16::MAX] {
                for seen in [a, i] {
                    v.push(PhiEvent::RecordSeen {
                        incident: seen,
                        record,
                        upto,
                        fresh: inc(b"inc-cccccc"),
                    });
                }
            }
        }
        v
    }

    fn rank(p: &Phi) -> u8 {
        match p {
            Phi::Clean => 0,
            Phi::Held => 1,
            Phi::Exposed { .. } => 2,
            Phi::Sealed { .. } => 3,
        }
    }

    #[test]
    fn v5_the_label_file_round_trips_and_anything_else_reads_as_none() {
        for p in labels() {
            assert_eq!(decode_phi(encode_phi(&p).as_bytes()), Some(p), "{p:?}");
        }
        for bad in [
            &b""[..],
            b"Clean",
            b"exposed",
            b"exposed inc-aaaaaa tool_input 0 0",
            b"exposed inc-aaaaaa tool_input 1 2",
            b"exposed inc-aaaaaa tool_input 1 0 extra",
            b"exposed inc-aaaaaa leaked 1 0",
            b"exposed inc-AAAAAA tool_input 1 0",
        ] {
            assert_eq!(decode_phi(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn t1_t2_t11_labels_only_lower_by_a_rewind_and_sealed_is_final() {
        for p in labels() {
            for ev in events() {
                let (q, _) = phi_step(p, ev);
                if rank(&q) < rank(&p) {
                    let ok = match (p, ev) {
                        // T2: Held to Clean on a rewritten prompt that passes.
                        (Phi::Held, PhiEvent::PromptClean) => q == Phi::Clean,
                        // T2: Exposed to Clean on a rewind record for exactly this incident.
                        (
                            Phi::Exposed { incident, hits, .. },
                            PhiEvent::RecordSeen {
                                incident: seen,
                                record: SeenRecord::Rewound,
                                upto,
                                ..
                            },
                        ) => incident == seen && hits <= upto && q == Phi::Clean,
                        // The rewind proof, for exactly this incident.
                        (
                            Phi::Exposed { incident, .. },
                            PhiEvent::ChainProvedClear { incident: proved },
                        ) => incident == proved && q == Phi::Clean,
                        _ => false,
                    };
                    assert!(ok, "a label lowered: {p:?} --{ev:?}--> {q:?}");
                }
                if let Phi::Sealed { .. } = p {
                    assert_eq!(q, p, "T11: Sealed is final");
                }
            }
        }
    }

    #[test]
    fn t12_a_detector_fault_makes_the_same_move_as_a_blocking_rule() {
        let i = inc(b"inc-bbbbbb");
        for p in labels() {
            for person_typed in [false, true] {
                let fault = phi_step(
                    p,
                    PhiEvent::PromptHit {
                        rule: RuleId::R00,
                        person_typed,
                        incident: i,
                    },
                );
                let hit = phi_step(
                    p,
                    PhiEvent::PromptHit {
                        rule: RuleId::R01,
                        person_typed,
                        incident: i,
                    },
                );
                assert_eq!(fault.0, hit.0);
                assert_eq!(fault.1.block_prompt, hit.1.block_prompt);
            }
            let fault = phi_step(
                p,
                PhiEvent::ToolInputHit {
                    rule: RuleId::R00,
                    incident: i,
                },
            );
            let hit = phi_step(
                p,
                PhiEvent::ToolInputHit {
                    rule: RuleId::R03,
                    incident: i,
                },
            );
            assert_eq!((fault.0, fault.1.deny_call), (hit.0, hit.1.deny_call));
        }
    }

    #[test]
    fn every_hit_on_an_open_label_brakes_except_a_typed_prompt() {
        for p in [Phi::Clean, Phi::Held] {
            for ev in events() {
                let (q, fx) = phi_step(p, ev);
                let typed = matches!(
                    ev,
                    PhiEvent::PromptHit {
                        person_typed: true,
                        ..
                    }
                );
                let hit = matches!(
                    ev,
                    PhiEvent::PromptHit { .. }
                        | PhiEvent::ResultHit { .. }
                        | PhiEvent::BatchHit { .. }
                        | PhiEvent::ToolInputHit { .. }
                        | PhiEvent::TranscriptHit { .. }
                        | PhiEvent::Flag { .. }
                );
                if hit && typed {
                    assert_eq!(q, Phi::Held);
                    assert!(fx.block_prompt && fx.residue && fx.taint.is_none());
                } else if hit {
                    assert!(matches!(q, Phi::Exposed { .. }), "{p:?} {ev:?} -> {q:?}");
                    assert!(fx.taint.is_some() && fx.quarantine.is_some());
                }
            }
        }
    }

    #[test]
    fn a_hit_after_the_admin_looked_reopens_under_a_new_incident() {
        let a = inc(b"inc-aaaaaa");
        let c = inc(b"inc-cccccc");
        let p = Phi::Exposed {
            incident: a,
            cause: Cause::ResultMasked,
            hits: 3,
            acked: true,
        };
        let ev = PhiEvent::RecordSeen {
            incident: a,
            record: SeenRecord::Rewound,
            upto: 2,
            fresh: c,
        };
        let (q, fx) = phi_step(p, ev);
        assert_eq!(
            q,
            Phi::Exposed {
                incident: c,
                cause: Cause::ResultMasked,
                hits: 1,
                acked: false
            }
        );
        assert_eq!(fx.taint, Some((c, Cause::ResultMasked)));
    }

    #[test]
    fn t9_the_room_refuses_a_rewind_of_real_data_a_vendor_holds() {
        let a = inc(b"inc-aaaaaa");
        let scrub = Scrubbed::parse(b"k3q9x2ab").expect("a code");
        for cause in [
            Cause::ResultMasked,
            Cause::BatchUnsent,
            Cause::ToolInput,
            Cause::FoundLater,
        ] {
            let p = SeatPhi::Exposed { incident: a, cause };
            let real = room_record(p, a, ExitRecord::Rewound { scrub, real: true });
            let vendor_holds = matches!(cause, Cause::ToolInput | Cause::FoundLater);
            assert_eq!(real.is_err(), vendor_holds, "{cause:?}");
            assert_eq!(
                room_record(p, a, ExitRecord::Rewound { scrub, real: false }),
                Ok(SeatPhi::Clean)
            );
            assert_eq!(
                room_record(p, a, ExitRecord::Confirmed),
                Ok(SeatPhi::Sealed)
            );
            assert_eq!(
                room_record(p, inc(b"inc-zzzzzz"), ExitRecord::Confirmed),
                Err(RecordRefusal::WrongIncident)
            );
        }
        assert_eq!(
            room_record(SeatPhi::Clean, a, ExitRecord::Confirmed),
            Err(RecordRefusal::NotExposed)
        );
        assert_eq!(
            room_record(SeatPhi::Sealed, a, ExitRecord::Confirmed),
            Err(RecordRefusal::AlreadySealed)
        );
        assert!(Scrubbed::parse(b"short").is_none());
    }

    #[test]
    fn the_effective_label_is_the_highest_and_residue_is_not_a_brake() {
        let a = inc(b"inc-aaaaaa");
        assert_eq!(
            effective(&Phi::Clean, &ConvPhi::Residue, None),
            Brake::Clear
        );
        assert_eq!(
            effective(&Phi::Held, &ConvPhi::Exposed(a), None),
            Brake::Exposed(a)
        );
        assert_eq!(
            effective(&Phi::Clean, &ConvPhi::Clean, Some(&SeatPhi::Sealed)),
            Brake::Sealed
        );
        let room = SeatPhi::Exposed {
            incident: a,
            cause: Cause::FoundLater,
        };
        assert_eq!(
            effective(&Phi::Clean, &ConvPhi::Clean, Some(&room)),
            Brake::Exposed(a)
        );
    }

    #[test]
    fn q1_a_source_reopens_only_for_a_false_alarm_on_its_own_incident() {
        let a = inc(b"inc-aaaaaa");
        let open = SourceIncident::Open {
            inc: a,
            source: SourceKey::Folder,
        };
        assert!(source_denied(&open));
        assert!(source_denied(&source_step(
            open,
            inc(b"inc-bbbbbb"),
            SeenRecord::Rewound,
            false
        )));
        assert!(!source_denied(&source_step(
            open,
            a,
            SeenRecord::Rewound,
            false
        )));
        assert!(source_denied(&source_step(
            open,
            a,
            SeenRecord::Rewound,
            true
        )));
        assert!(source_denied(&source_step(
            open,
            a,
            SeenRecord::Confirmed,
            true
        )));
        let shell = SourceIncident::Open {
            inc: a,
            source: SourceKey::Unknown,
        };
        assert!(!source_denied(&shell), "a shell tool has no source to deny");
    }
}
