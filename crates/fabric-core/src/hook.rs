//! H1, the PHI half: what each Claude Code hook decides, before any
//! coordination runs. Pure and total; `heddle` does the I/O.
//!
//! The seamless design (2026-11-14) with Xavier's decision 3 for PHI-free
//! repos: a fault before a tool runs costs a retry; a replaced result is
//! withheld and the conversation goes on (PostToolBatch stops the turn after
//! three); the
//! first patient-shaped tool input gets one deny with the made-up-data hint
//! and a second pauses the conversation. A paused conversation is denied and
//! stopped (`continue: false`) at every line: two exits only, rewind or a new
//! session. Every sentence comes from the fixed table in [`text`], with
//! grammar values only: rule ids and references, never payload text.

use alloc::string::String;

use crate::ids::IncidentId;
use crate::phi::{Brake, PhiEvent, RuleId};

/// The hook lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HookKind {
    /// Session start; it cannot block.
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
    /// Whether this line carries `onFailure: "block"` (in a brake repo).
    pub const fn is_phi_line(self) -> bool {
        !matches!(self, HookKind::SessionStart)
    }
}

/// A fixed sentence and the grammar values it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// A typed prompt was held before it was sent (row I1).
    Held(RuleId),
    /// The conversation is paused by this hit (row I2).
    Paused(RuleId, IncidentId),
    /// The conversation was paused earlier.
    StillPaused(IncidentId),
    /// The conversation is sealed.
    Sealed,
    /// A typed prompt is held: no expansion or compaction until it is rewritten.
    HeldWaiting,
    /// The first patient-shaped tool input: denied, with the made-up-data hint.
    InputHint(RuleId),
    /// A tool result was withheld; the conversation goes on.
    Withheld(RuleId),
    /// Three results withheld in one turn: the turn stops.
    TurnStopped,
    /// heddle could not finish its check: a retry (row I3).
    Hiccup,
    /// Hook line 9 refused a settings change that would remove heddle's lines.
    WiringKept,
    /// A Bash command matched a seatbelt.
    Seatbelt(Seatbelt),
}

/// A Bash command that would switch the brake off by accident (§5 item 4).
/// A match on command text: a seatbelt, not a boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seatbelt {
    /// A nested `claude` that skips this project's settings.
    NestedClaude,
    /// A git command that deletes ignored files, the local settings among them.
    DeletesIgnored,
}

/// Check each simple command in `command` (split at `;`, `&`, `|` and line
/// breaks) for a seatbelt.
pub fn seatbelt(command: &str) -> Option<Seatbelt> {
    command.split([';', '&', '|', '\n']).find_map(|part| {
        let mut words = part.split_whitespace().skip_while(|w| {
            w.contains('=') && !w.starts_with('-')
                || matches!(*w, "env" | "exec" | "command" | "nohup" | "time")
        });
        let program = words.next()?;
        let name = program
            .rsplit('/')
            .next()
            .unwrap_or(program)
            .trim_matches(['"', '\'']);
        let args: alloc::vec::Vec<&str> = words.map(|w| w.trim_matches(['"', '\''])).collect();
        match name {
            "claude" => args
                .iter()
                .any(|a| {
                    *a == "--bare"
                        || ["--settings", "--setting-sources"].iter().any(|f| {
                            *a == *f || a.strip_prefix(f).is_some_and(|r| r.starts_with('='))
                        })
                })
                .then_some(Seatbelt::NestedClaude),
            "git" => {
                // Skip git's own options: `-C <path>`, `-c <k=v>` and flags.
                let mut rest = args.iter().copied();
                let sub = loop {
                    match rest.next()? {
                        "-C" | "-c" | "--git-dir" | "--work-tree" => {
                            rest.next()?;
                        }
                        w if w.starts_with('-') => {}
                        w => break w,
                    }
                };
                let opts: alloc::vec::Vec<&str> = rest.collect();
                let short = |c: char| {
                    opts.iter()
                        .any(|o| o.starts_with('-') && !o.starts_with("--") && o.contains(c))
                };
                let long = |l: &str| opts.contains(&l);
                let deletes = match sub {
                    "clean" => (short('x') || short('X')) && !short('n') && !long("--dry-run"),
                    "stash" => short('a') || long("--all"),
                    _ => false,
                };
                deletes.then_some(Seatbelt::DeletesIgnored)
            }
            _ => None,
        }
    })
}

/// What a hook prints. `Pass` prints nothing at all: on several events plain
/// stdout becomes model context or compaction instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Print nothing; the action goes ahead.
    Pass,
    /// SessionStart: add the sentence to the context.
    Inject(Msg),
    /// UserPromptSubmit: block. `suppress` leaves the prompt out of the block
    /// message: set on a detector hit, never on a fault (so it can be copied).
    BlockPrompt {
        /// The sentence.
        msg: Msg,
        /// Leave the prompt text out of the block message.
        suppress: bool,
    },
    /// UserPromptExpansion: block.
    BlockExpansion(Msg),
    /// PreCompact: block.
    BlockCompaction(Msg),
    /// PreToolUse: deny this call; the turn goes on (a retry, or a first strike).
    Deny(Msg),
    /// PreToolUse: deny and stop processing (`continue: false`): the brake.
    DenyAndStop(Msg),
    /// PostToolUse: replace the result with the sentence, in the tool's own
    /// shape; the turn goes on.
    Replace(Msg, ResultShape),
    /// PostToolBatch: stop the loop before the next model call.
    StopLoop(Msg),
    /// Any event: `continue: false`, Claude Code's universal stop.
    Halt(Msg),
    /// ConfigChange: refuse the change for the running session (hook line 9).
    BlockConfig(Msg),
}

/// A tool result heddle can replace, by its output shape. A built-in tool's
/// replacement must match its output schema, or Claude Code ignores it and
/// sends the original (PostToolBatch's scan then stops the loop); an MCP
/// tool's output is not validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultShape {
    /// `{stdout, stderr, interrupted, isImage}`.
    Bash,
    /// `{type: "text", file: {filePath, content, numLines, startLine, totalLines}}`.
    Read,
    /// `{mode, numFiles, filenames, content, numLines}`.
    Grep,
    /// A content-block array, `[{type: "text", text}]`.
    Mcp,
}

impl ResultShape {
    /// The shape of this tool's result, if heddle can replace it.
    pub fn of(tool: &str) -> Option<ResultShape> {
        match tool {
            "Bash" => Some(ResultShape::Bash),
            "Read" => Some(ResultShape::Read),
            "Grep" => Some(ResultShape::Grep),
            t if t.starts_with("mcp__") => Some(ResultShape::Mcp),
            _ => None,
        }
    }
}

/// The fixed table. Plain words; facts, not orders.
pub fn text(m: &Msg) -> String {
    use core::fmt::Write;
    const SYNTHETIC: &str = "If this is made-up data, use ids starting SYN-, Social Security numbers in area 000 \
         or 666, or \"synthetic\": true in JSON; those never trip the check.";
    const NOBODY: &str = "This is recorded only on this computer; nobody is notified. Most of these are false alarms.";
    let mut s = String::new();
    let _ = match m {
        Msg::Held(rule) => write!(
            s,
            "Fabric held this message: something in it looks like patient information (rule {}). \
             It was not sent to the AI model. {NOBODY} {SYNTHETIC} A copy of the held text remains in \
             Claude's history on this computer.",
            rule_name(*rule)
        ),
        Msg::Paused(rule, inc) => write!(
            s,
            "Fabric paused this conversation: something looks like patient information (rule {}, \
             reference {}). {NOBODY} To carry on: start a new session. Or rewind: /rewind, pick the \
             message before this one, choose Restore conversation, then edit and send.",
            rule_name(*rule),
            reference(inc)
        ),
        Msg::StillPaused(inc) => write!(
            s,
            "Fabric paused this conversation earlier: something looked like patient information \
             (reference {}). To carry on: start a new session. Or rewind: /rewind, pick the message \
             before it, choose Restore conversation, then edit and send. If this comes back right \
             after a rewind, send it once more.",
            reference(inc)
        ),
        Msg::Sealed => write!(
            s,
            "Fabric sealed this conversation. Start a new session to carry on."
        ),
        Msg::HeldWaiting => write!(
            s,
            "Fabric is holding a message in this conversation: rewrite it without patient details and \
             send it again first."
        ),
        Msg::InputHint(rule) => write!(
            s,
            "Fabric blocked this tool call: its input looks like patient information (rule {}). \
             {SYNTHETIC} A second one in this conversation pauses it.",
            rule_name(*rule)
        ),
        Msg::Withheld(rule) => write!(
            s,
            "Fabric withheld this result: it looks like patient information (rule {}). {SYNTHETIC} \
             The conversation goes on.",
            rule_name(*rule)
        ),
        Msg::TurnStopped => write!(
            s,
            "Fabric stopped this turn: three tool results in it looked like patient information and \
             were withheld. {SYNTHETIC} The conversation goes on with your next message."
        ),
        Msg::Hiccup => write!(s, "Fabric had a hiccup and found nothing. Try it again."),
        Msg::Seatbelt(Seatbelt::NestedClaude) => write!(
            s,
            "Fabric stopped this command: it starts Claude without this project's settings, which would \
             leave Fabric's checks behind. If your person wants it, they can run it themselves."
        ),
        Msg::Seatbelt(Seatbelt::DeletesIgnored) => write!(
            s,
            "Fabric stopped this command: it deletes ignored files, and this project's Fabric settings are \
             one of them. Leave out -x, -X or --all, or ask your person to run it themselves."
        ),
        Msg::WiringKept => write!(
            s,
            "Fabric kept this project's settings as they were for this session: the change would remove its \
             checks. To remove Fabric, ask your agent to uninstall it."
        ),
    };
    s
}

/// A reference a person can quote: the incident id without its prefix.
fn reference(inc: &IncidentId) -> String {
    use alloc::string::ToString;
    let full = inc.to_string();
    full.strip_prefix("inc-").map(String::from).unwrap_or(full)
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

/// The decision that stops this hook's action with `m`, for a paused
/// conversation.
fn stopped(kind: HookKind, m: Msg) -> Decision {
    match kind {
        HookKind::SessionStart => Decision::Inject(m),
        HookKind::UserPromptSubmit => Decision::BlockPrompt {
            msg: m,
            suppress: false,
        },
        HookKind::UserPromptExpansion => Decision::BlockExpansion(m),
        HookKind::PreToolUse => Decision::DenyAndStop(m),
        HookKind::PostToolUse => Decision::Halt(m),
        HookKind::PostToolBatch => Decision::StopLoop(m),
        HookKind::PreCompact => Decision::BlockCompaction(m),
        HookKind::ConfigChange => Decision::Pass,
    }
}

/// Check 1 of every hook: the conversation's label (T6). `None`: go on. A
/// settings change is not the conversation's: hook line 9 judges it alone.
pub fn brake_gate(kind: HookKind, brake: Brake) -> Option<Decision> {
    if kind == HookKind::ConfigChange {
        return None;
    }
    match brake {
        Brake::Clear => None,
        Brake::Held => match kind {
            HookKind::UserPromptExpansion => Some(Decision::BlockExpansion(Msg::HeldWaiting)),
            HookKind::PreCompact => Some(Decision::BlockCompaction(Msg::HeldWaiting)),
            HookKind::SessionStart
            | HookKind::UserPromptSubmit
            | HookKind::PreToolUse
            | HookKind::PostToolUse
            | HookKind::PostToolBatch
            | HookKind::ConfigChange => None,
        },
        Brake::Exposed(inc) => Some(stopped(kind, Msg::StillPaused(inc))),
        Brake::Sealed => Some(stopped(kind, Msg::Sealed)),
    }
}

/// heddle could not finish its check (rule R-00). Before a tool runs that
/// costs a retry and raises no label (decision 3(i)). After a tool has run,
/// the turn stops; PostToolBatch's own scan of the same results is the net.
/// A settings change it cannot check is refused for the running session.
pub fn fault(kind: HookKind) -> Decision {
    match kind {
        HookKind::SessionStart => Decision::Pass,
        HookKind::ConfigChange => Decision::BlockConfig(Msg::Hiccup),
        HookKind::UserPromptSubmit => Decision::BlockPrompt {
            msg: Msg::Hiccup,
            suppress: false,
        },
        HookKind::UserPromptExpansion => Decision::BlockExpansion(Msg::Hiccup),
        HookKind::PreToolUse => Decision::Deny(Msg::Hiccup),
        HookKind::PostToolUse => Decision::Halt(Msg::Hiccup),
        HookKind::PostToolBatch => Decision::StopLoop(Msg::Hiccup),
        HookKind::PreCompact => Decision::BlockCompaction(Msg::Hiccup),
    }
}

/// What the edge knows about this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanCtx {
    /// UserPromptSubmit: a person typed the prompt (no subagent hook since
    /// the last prompt: the conservative rule of M13).
    pub person_typed: bool,
    /// PostToolUse: the shape to replace this tool's result with, if any.
    pub result_shape: Option<ResultShape>,
    /// PostToolUse: the result passed a model before the hook (web fetch, a subagent).
    pub passed_model: bool,
    /// Patient-shaped tool inputs already denied in this conversation.
    pub input_strikes: u8,
}

/// The outcome of a hit: what to print, the label event if the conversation
/// pauses, and the counters to bump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitOutcome {
    /// What the hook prints.
    pub decision: Decision,
    /// The label event, when the hit pauses the conversation or holds a prompt.
    pub event: Option<PhiEvent>,
    /// A first patient-shaped tool input was denied.
    pub strike: bool,
    /// A result was withheld.
    pub withheld: bool,
}

/// The first blocking hit in this hook's text. `None` when the hit only
/// counts (R-08, content the rules cannot read).
pub fn on_hit(
    kind: HookKind,
    rule: RuleId,
    ctx: &ScanCtx,
    incident: IncidentId,
) -> Option<HitOutcome> {
    if rule == RuleId::R08 {
        return None;
    }
    let pause = |decision: Decision, event: PhiEvent| HitOutcome {
        decision,
        event: Some(event),
        strike: false,
        withheld: false,
    };
    match kind {
        HookKind::UserPromptSubmit => {
            let msg = if ctx.person_typed {
                Msg::Held(rule)
            } else {
                Msg::Paused(rule, incident)
            };
            Some(pause(
                Decision::BlockPrompt {
                    msg,
                    suppress: true,
                },
                PhiEvent::PromptHit {
                    rule,
                    person_typed: ctx.person_typed,
                    incident,
                },
            ))
        }
        HookKind::PreToolUse if ctx.input_strikes == 0 => Some(HitOutcome {
            decision: Decision::Deny(Msg::InputHint(rule)),
            event: None,
            strike: true,
            withheld: false,
        }),
        HookKind::PreToolUse => Some(pause(
            Decision::DenyAndStop(Msg::Paused(rule, incident)),
            PhiEvent::ToolInputHit { rule, incident },
        )),
        HookKind::PostToolUse => match ctx.result_shape {
            Some(shape) if !ctx.passed_model => Some(HitOutcome {
                decision: Decision::Replace(Msg::Withheld(rule), shape),
                event: None,
                strike: false,
                withheld: true,
            }),
            Some(_) | None => Some(pause(
                Decision::Halt(Msg::Paused(rule, incident)),
                PhiEvent::ResultHit {
                    rule,
                    can_replace: false,
                    can_stop: true,
                    passed_model: ctx.passed_model,
                    incident,
                },
            )),
        },
        HookKind::PostToolBatch => Some(pause(
            Decision::StopLoop(Msg::Paused(rule, incident)),
            PhiEvent::BatchHit { rule, incident },
        )),
        HookKind::SessionStart
        | HookKind::UserPromptExpansion
        | HookKind::PreCompact
        | HookKind::ConfigChange => None,
    }
}

/// After a clean batch scan: three results withheld in this turn stop it
/// (decision 3(ii)). PostToolBatch's block is verified (day 0), and the
/// replacements are already in the transcript.
pub fn turn_gate(kind: HookKind, withheld_this_turn: u8) -> Option<Decision> {
    (kind == HookKind::PostToolBatch && withheld_this_turn >= 3)
        .then_some(Decision::StopLoop(Msg::TurnStopped))
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
        IncidentId::parse(b"inc-k3q9x2").expect("an incident id")
    }

    fn ctx() -> ScanCtx {
        ScanCtx {
            person_typed: true,
            result_shape: Some(ResultShape::Read),
            passed_model: false,
            input_strikes: 0,
        }
    }

    #[test]
    fn names_round_trip_and_unknown_events_do_not_parse() {
        for k in KINDS {
            assert_eq!(HookKind::parse(k.name().as_bytes()), Some(k));
        }
        assert_eq!(HookKind::parse(b"Stop"), None);
    }

    #[test]
    fn a_paused_conversation_is_stopped_at_every_phi_line() {
        for k in KINDS {
            let d = brake_gate(k, Brake::Exposed(inc()));
            if k.is_phi_line() && k != HookKind::ConfigChange {
                assert!(d.is_some_and(|d| d != Decision::Pass), "{k:?}");
            }
            assert_eq!(brake_gate(k, Brake::Clear), None);
        }
        assert_eq!(
            brake_gate(HookKind::ConfigChange, Brake::Exposed(inc())),
            None
        );
        assert_eq!(
            brake_gate(HookKind::PreToolUse, Brake::Exposed(inc())),
            Some(Decision::DenyAndStop(Msg::StillPaused(inc())))
        );
    }

    #[test]
    fn decision_3_a_fault_before_a_tool_is_a_retry_and_never_suppresses_the_prompt() {
        assert_eq!(fault(HookKind::PreToolUse), Decision::Deny(Msg::Hiccup));
        assert_eq!(
            fault(HookKind::UserPromptSubmit),
            Decision::BlockPrompt {
                msg: Msg::Hiccup,
                suppress: false
            }
        );
        for k in KINDS {
            assert_eq!(fault(k) == Decision::Pass, !k.is_phi_line(), "{k:?}");
        }
    }

    #[test]
    fn decision_3_the_first_tool_input_is_a_hint_and_the_second_pauses() {
        let first = on_hit(HookKind::PreToolUse, RuleId::R09, &ctx(), inc()).expect("a hit");
        assert_eq!(first.decision, Decision::Deny(Msg::InputHint(RuleId::R09)));
        assert!(first.strike && first.event.is_none());
        let second = on_hit(
            HookKind::PreToolUse,
            RuleId::R09,
            &ScanCtx {
                input_strikes: 1,
                ..ctx()
            },
            inc(),
        )
        .expect("a hit");
        assert!(matches!(
            second.decision,
            Decision::DenyAndStop(Msg::Paused(..))
        ));
        assert!(matches!(second.event, Some(PhiEvent::ToolInputHit { .. })));
    }

    #[test]
    fn decision_3_results_are_withheld_and_the_third_in_a_turn_stops_it() {
        let one = on_hit(HookKind::PostToolUse, RuleId::R01, &ctx(), inc()).expect("a hit");
        assert_eq!(
            one.decision,
            Decision::Replace(Msg::Withheld(RuleId::R01), ResultShape::Read)
        );
        assert!(one.withheld && one.event.is_none());
        assert_eq!(turn_gate(HookKind::PostToolBatch, 2), None);
        assert_eq!(
            turn_gate(HookKind::PostToolBatch, 3),
            Some(Decision::StopLoop(Msg::TurnStopped))
        );
        assert_eq!(turn_gate(HookKind::PostToolUse, 3), None);
        // A result that cannot be replaced, or that a model saw first, pauses the conversation.
        let read = on_hit(
            HookKind::PostToolUse,
            RuleId::R01,
            &ScanCtx {
                result_shape: None,
                ..ctx()
            },
            inc(),
        )
        .expect("a hit");
        assert!(matches!(read.decision, Decision::Halt(Msg::Paused(..))) && read.event.is_some());
        let web = on_hit(
            HookKind::PostToolUse,
            RuleId::R01,
            &ScanCtx {
                passed_model: true,
                ..ctx()
            },
            inc(),
        )
        .expect("a hit");
        assert!(web.event.is_some());
    }

    #[test]
    fn a_typed_prompt_is_held_and_r08_only_counts() {
        let held = on_hit(HookKind::UserPromptSubmit, RuleId::R04, &ctx(), inc()).expect("a hit");
        assert_eq!(
            held.decision,
            Decision::BlockPrompt {
                msg: Msg::Held(RuleId::R04),
                suppress: true
            }
        );
        assert!(on_hit(HookKind::PreToolUse, RuleId::R08, &ctx(), inc()).is_none());
    }

    #[test]
    fn the_seatbelts_catch_a_nested_claude_without_settings_and_deleting_ignored_files() {
        for c in [
            "claude --bare -p hi",
            "cd x && claude --settings '{}' -p hi",
            "FOO=1 /usr/local/bin/claude --setting-sources=user",
            "env claude --settings=/tmp/s.json",
        ] {
            assert_eq!(seatbelt(c), Some(Seatbelt::NestedClaude), "{c}");
        }
        for c in [
            "git clean -fdx",
            "git -C repo clean -X -f",
            "make; git clean -xf",
            "git stash -a",
            "git stash push --all",
        ] {
            assert_eq!(seatbelt(c), Some(Seatbelt::DeletesIgnored), "{c}");
        }
        for c in [
            "claude -p hi",
            "git clean -fd",
            "git clean -ndX",
            "git clean -xf --dry-run",
            "git stash -u",
            "git status",
            "echo git clean -fdx",
            "grep -r 'claude --bare' notes",
            "",
        ] {
            assert_eq!(seatbelt(c), None, "{c}");
        }
    }

    #[test]
    fn the_sentences_name_grammar_values_only() {
        let t = text(&Msg::Paused(RuleId::R09, inc()));
        assert!(t.contains("rule R-09") && t.contains("reference k3q9x2") && !t.contains("inc-"));
        for m in [
            Msg::Held(RuleId::R01),
            Msg::StillPaused(inc()),
            Msg::InputHint(RuleId::R03),
            Msg::Withheld(RuleId::R12),
            Msg::Hiccup,
        ] {
            let s = text(&m);
            assert!(!s.is_empty() && s.is_ascii() && s.len() < 600, "{m:?}");
        }
    }
}
