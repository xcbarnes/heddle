//! H1, the PHI half: what each Claude Code hook decides before any
//! coordination runs. Pure and total; `heddle` does the I/O.
//!
//! The PHI checks run first in every hook, in every folder, ignore the `off`
//! flag and fail closed (H1d, H1e): a check that cannot complete gives the
//! decision a hit would give, under rule R-00. Every sentence a decision
//! carries comes from the fixed table in [`text`] (H1c), with grammar values
//! only: rule ids and incident ids, never payload text.

use alloc::string::String;

use crate::ids::IncidentId;
use crate::phi::{Brake, PhiEvent, RuleId};

/// The eight hook lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HookKind {
    /// Inject the digest; it cannot deny.
    SessionStart,
    /// Before a prompt is sent.
    UserPromptSubmit,
    /// Before a slash command expands.
    UserPromptExpansion,
    /// Before a tool runs.
    PreToolUse,
    /// After a tool ran.
    PostToolUse,
    /// Before the next model call, with every result of the batch.
    PostToolBatch,
    /// Before a compaction: a model call over the whole conversation.
    PreCompact,
    /// A settings file changed.
    ConfigChange,
}

impl HookKind {
    /// The event name as Claude Code writes it.
    pub fn parse(name: &[u8]) -> Option<HookKind> {
        match name {
            b"SessionStart" => Some(HookKind::SessionStart),
            b"UserPromptSubmit" => Some(HookKind::UserPromptSubmit),
            b"UserPromptExpansion" => Some(HookKind::UserPromptExpansion),
            b"PreToolUse" => Some(HookKind::PreToolUse),
            b"PostToolUse" => Some(HookKind::PostToolUse),
            b"PostToolBatch" => Some(HookKind::PostToolBatch),
            b"PreCompact" => Some(HookKind::PreCompact),
            b"ConfigChange" => Some(HookKind::ConfigChange),
            _ => None,
        }
    }
    /// The name, for `hookEventName`.
    pub const fn name(self) -> &'static str {
        match self {
            HookKind::SessionStart => "SessionStart",
            HookKind::UserPromptSubmit => "UserPromptSubmit",
            HookKind::UserPromptExpansion => "UserPromptExpansion",
            HookKind::PreToolUse => "PreToolUse",
            HookKind::PostToolUse => "PostToolUse",
            HookKind::PostToolBatch => "PostToolBatch",
            HookKind::PreCompact => "PreCompact",
            HookKind::ConfigChange => "ConfigChange",
        }
    }
    /// Whether this line carries `onFailure: block` (phi.md §6).
    pub const fn is_phi_line(self) -> bool {
        !matches!(self, HookKind::SessionStart | HookKind::ConfigChange)
    }
}

/// A fixed sentence and the grammar values it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The thread is Exposed.
    Braked(IncidentId),
    /// The thread is Sealed.
    Sealed,
    /// A typed prompt was held: no expansions or compactions until it passes.
    Held,
    /// This prompt was held before it was sent.
    PromptHeld(RuleId),
    /// This result was withheld and the thread stopped.
    ResultWithheld(RuleId, IncidentId),
    /// The loop was stopped before the next model call.
    LoopStopped(RuleId, IncidentId),
    /// A PHI check could not run (R-00).
    Fault,
}

/// What a hook prints. `Pass` prints nothing at all: on several events plain
/// stdout becomes model context or compaction instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Print nothing; the action goes ahead.
    Pass,
    /// SessionStart: add the sentence to the context.
    Inject(Msg),
    /// UserPromptSubmit: block, without echoing the prompt.
    BlockPrompt(Msg),
    /// UserPromptExpansion: block.
    BlockExpansion(Msg),
    /// PreCompact: block.
    BlockCompaction(Msg),
    /// PreToolUse: deny.
    Deny(Msg),
    /// PostToolUse: replace the Bash result with the sentence.
    ReplaceBash(Msg),
    /// PostToolBatch: stop the loop before the next model call.
    StopLoop(Msg),
    /// Any event: `continue: false`, Claude Code's universal stop. For an
    /// unknown event, and for a result that cannot be replaced.
    Halt(Msg),
}

/// The fixed table (CS7). Phrased as facts from a named source.
pub fn text(m: &Msg) -> String {
    use core::fmt::Write;
    let mut s = String::new();
    let _ = match m {
        Msg::Braked(inc) => write!(
            s,
            "Fabric stopped this thread: possible patient information reached it (incident {inc}). \
             Rewind to before that turn, scrub it, and have the admin user record the rewind. \
             Moving the thread to PHI-safe infrastructure is not available yet."
        ),
        Msg::Sealed => write!(s, "Fabric sealed this thread: a person confirmed patient information reached it."),
        Msg::Held => write!(
            s,
            "Fabric is holding a prompt in this thread: possible patient information. \
             Rewrite the prompt without patient details and send it again first."
        ),
        Msg::PromptHeld(rule) => write!(
            s,
            "Fabric held this prompt: possible patient information (rule {}). It was not sent. \
             Rewrite it without patient details and send again.",
            rule_name(*rule)
        ),
        Msg::ResultWithheld(rule, inc) => write!(
            s,
            "Fabric withheld this result and stopped this thread: possible patient information \
             (rule {}, incident {inc}).",
            rule_name(*rule)
        ),
        Msg::LoopStopped(rule, inc) => write!(
            s,
            "Fabric stopped this thread before the next model call: possible patient information \
             (rule {}, incident {inc}).",
            rule_name(*rule)
        ),
        Msg::Fault => write!(
            s,
            "Fabric's PHI check could not run (rule R-00), so this was blocked. \
             A person must repair heddle from the admin user."
        ),
    };
    s
}

/// A rule's printed id.
pub const fn rule_name(r: RuleId) -> &'static str {
    match r {
        RuleId::R00 => "R-00",
        RuleId::R01 => "R-01",
        RuleId::R02 => "R-02",
        RuleId::R03 => "R-03",
        RuleId::R04 => "R-04",
        RuleId::R05 => "R-05",
        RuleId::R06 => "R-06",
        RuleId::R07 => "R-07",
        RuleId::R08 => "R-08",
        RuleId::R09 => "R-09",
        RuleId::R10 => "R-10",
        RuleId::R11 => "R-11",
        RuleId::R12 => "R-12",
    }
}

/// Check 1 of every hook: the effective label (T6). `None` means go on to
/// the scan.
pub fn brake_gate(kind: HookKind, brake: Brake) -> Option<Decision> {
    let stop = match brake {
        Brake::Clear => return None,
        Brake::Held => {
            // Held blocks only what would send the held text onward.
            return match kind {
                HookKind::UserPromptExpansion => Some(Decision::BlockExpansion(Msg::Held)),
                HookKind::PreCompact => Some(Decision::BlockCompaction(Msg::Held)),
                HookKind::SessionStart
                | HookKind::UserPromptSubmit
                | HookKind::PreToolUse
                | HookKind::PostToolUse
                | HookKind::PostToolBatch
                | HookKind::ConfigChange => None,
            };
        }
        Brake::Exposed(inc) => Msg::Braked(inc),
        Brake::Sealed => Msg::Sealed,
    };
    Some(stopped(kind, stop))
}

/// The decision that stops this hook's action, with `m`.
fn stopped(kind: HookKind, m: Msg) -> Decision {
    match kind {
        HookKind::SessionStart => Decision::Inject(m),
        HookKind::UserPromptSubmit => Decision::BlockPrompt(m),
        HookKind::UserPromptExpansion => Decision::BlockExpansion(m),
        HookKind::PreToolUse => Decision::Deny(m),
        HookKind::PostToolUse => Decision::Halt(m),
        HookKind::PostToolBatch => Decision::StopLoop(m),
        HookKind::PreCompact => Decision::BlockCompaction(m),
        HookKind::ConfigChange => Decision::Pass,
    }
}

/// A PHI check could not complete: the decision a hit would give (H1e).
pub fn fault(kind: HookKind) -> Decision {
    stopped(kind, Msg::Fault)
}

/// What the edge knows about this hook's moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCtx {
    /// Rule hits block here: an enrolled repo or a path inside one (B10).
    /// R-00 blocks everywhere regardless.
    pub enforce: bool,
    /// UserPromptSubmit: a person typed the prompt, by the conservative rule
    /// (no subagent hook since the last prompt).
    pub person_typed: bool,
    /// PostToolUse: this tool's result has a known shape to replace.
    pub can_replace: bool,
    /// PostToolUse: the result passed a model before the hook (web fetch, a
    /// subagent).
    pub passed_model: bool,
}

/// The first blocking hit in this hook's text: the decision, and the event
/// for the label machine. `None` when the hit only counts.
pub fn on_hit(kind: HookKind, rule: RuleId, ctx: &ScanCtx, incident: IncidentId) -> Option<(Decision, PhiEvent)> {
    let counts_only = match rule {
        RuleId::R00 => false,
        RuleId::R08 => true,
        RuleId::R01
        | RuleId::R02
        | RuleId::R03
        | RuleId::R04
        | RuleId::R05
        | RuleId::R06
        | RuleId::R07
        | RuleId::R09
        | RuleId::R10
        | RuleId::R11
        | RuleId::R12 => !ctx.enforce,
    };
    if counts_only {
        return None;
    }
    match kind {
        HookKind::UserPromptSubmit => {
            let m = if ctx.person_typed { Msg::PromptHeld(rule) } else { Msg::Braked(incident) };
            Some((Decision::BlockPrompt(m), PhiEvent::PromptHit { rule, person_typed: ctx.person_typed, incident }))
        }
        HookKind::PreToolUse => Some((Decision::Deny(Msg::Braked(incident)), PhiEvent::ToolInputHit { rule, incident })),
        HookKind::PostToolUse => {
            let replace = ctx.can_replace && !ctx.passed_model;
            let m = Msg::ResultWithheld(rule, incident);
            let d = if replace { Decision::ReplaceBash(m) } else { Decision::Halt(m) };
            let ev = PhiEvent::ResultHit {
                rule,
                can_replace: replace,
                can_stop: true,
                passed_model: ctx.passed_model,
                incident,
            };
            Some((d, ev))
        }
        HookKind::PostToolBatch => {
            Some((Decision::StopLoop(Msg::LoopStopped(rule, incident)), PhiEvent::BatchHit { rule, incident }))
        }
        HookKind::SessionStart | HookKind::UserPromptExpansion | HookKind::PreCompact | HookKind::ConfigChange => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    const KINDS: [HookKind; 8] = [
        HookKind::SessionStart,
        HookKind::UserPromptSubmit,
        HookKind::UserPromptExpansion,
        HookKind::PreToolUse,
        HookKind::PostToolUse,
        HookKind::PostToolBatch,
        HookKind::PreCompact,
        HookKind::ConfigChange,
    ];

    fn inc() -> IncidentId {
        IncidentId::parse(b"inc-aaaaaa").expect("an incident id")
    }

    #[test]
    fn names_round_trip_and_unknown_events_do_not_parse() {
        for k in KINDS {
            assert_eq!(HookKind::parse(k.name().as_bytes()), Some(k));
        }
        assert_eq!(HookKind::parse(b"Stop"), None);
        assert_eq!(HookKind::parse(b"pretooluse"), None);
    }

    #[test]
    fn h1e_a_fault_stops_every_phi_line() {
        for k in KINDS {
            let d = fault(k);
            assert_eq!(d == Decision::Pass, !k.is_phi_line() && k != HookKind::SessionStart, "{k:?}");
            if k.is_phi_line() {
                assert_ne!(d, Decision::Pass, "{k:?}");
            }
        }
    }

    #[test]
    fn t6_exposed_and_sealed_stop_every_phi_line_and_held_blocks_onward_sends() {
        for k in KINDS {
            for b in [Brake::Exposed(inc()), Brake::Sealed] {
                let d = brake_gate(k, b);
                assert!(d.is_some() && (d != Some(Decision::Pass) || k == HookKind::ConfigChange), "{k:?} {b:?}");
            }
            assert_eq!(brake_gate(k, Brake::Clear), None);
            let held = brake_gate(k, Brake::Held);
            let onward = matches!(k, HookKind::UserPromptExpansion | HookKind::PreCompact);
            assert_eq!(held.is_some(), onward, "{k:?}");
        }
    }

    #[test]
    fn r00_blocks_everywhere_other_rules_only_where_enforced_and_r08_only_counts() {
        let ctx = ScanCtx { enforce: false, person_typed: true, can_replace: true, passed_model: false };
        assert!(on_hit(HookKind::PreToolUse, RuleId::R00, &ctx, inc()).is_some());
        assert!(on_hit(HookKind::PreToolUse, RuleId::R01, &ctx, inc()).is_none());
        let on = ScanCtx { enforce: true, ..ctx };
        assert!(on_hit(HookKind::PreToolUse, RuleId::R01, &on, inc()).is_some());
        assert!(on_hit(HookKind::PreToolUse, RuleId::R08, &on, inc()).is_none());
    }

    #[test]
    fn a_result_is_replaced_only_when_its_shape_is_known_and_no_model_saw_it() {
        let base = ScanCtx { enforce: true, person_typed: true, can_replace: true, passed_model: false };
        let hit = |ctx: ScanCtx| on_hit(HookKind::PostToolUse, RuleId::R03, &ctx, inc()).map(|(d, _)| d);
        assert!(matches!(hit(base), Some(Decision::ReplaceBash(_))));
        assert!(matches!(hit(ScanCtx { can_replace: false, ..base }), Some(Decision::Halt(_))));
        assert!(matches!(hit(ScanCtx { passed_model: true, ..base }), Some(Decision::Halt(_))));
    }

    #[test]
    fn the_sentences_name_grammar_values_only() {
        let t = text(&Msg::ResultWithheld(RuleId::R01, inc()));
        assert!(t.contains("rule R-01") && t.contains("inc-aaaaaa"));
        assert!(t.is_ascii());
        for m in [Msg::Braked(inc()), Msg::Sealed, Msg::Held, Msg::PromptHeld(RuleId::R04), Msg::Fault] {
            let s = text(&m);
            assert!(!s.is_empty() && s.is_ascii() && s.len() < 400, "{m:?}");
        }
    }
}
