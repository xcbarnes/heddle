//! `heddle hook claude-code <Event> --root <main checkout> --state <folder>
//! [--mode brake|count]`: the PHI checks of every hook line.
//!
//! The hook line's presence is the enrolment: every invocation enforces, and
//! the two absolute arguments are the only way heddle finds its state (no
//! environment variable, so a pulled `env` block cannot point it elsewhere).
//!
//! `--mode count` (decision 4; Eureka until PHI readiness) logs every hit by
//! rule in the state folder and decides nothing: it prints nothing, even on a
//! fault, and its lines carry no `onFailure: "block"`. Without `--mode`, a
//! line is the full brake, so a line that loses the flag fails closed.
//!
//! The order of the first three things is the safety argument:
//! 1. the watchdog is armed before any read (decision new-15): a heddle that
//!    hangs ends itself non-zero before the line's timeout, and the harness's
//!    `onFailure: "block"` then blocks the action (day 0 verified both routes);
//! 2. the panic hook prints one fixed line, so a panic message, which can
//!    quote data, never reaches a model;
//! 3. everything else runs inside `catch_unwind`, and any fault becomes the
//!    fault decision (rule R-00). Bad arguments, an unknown harness, an
//!    unknown event and a missing state folder are faults too.
//!
//! Exit codes (§5.4): 0 with one complete JSON object, or with nothing for a
//! pass. The only deliberate non-zero exits are the watchdog's and a stop
//! that could not be printed; both make the harness block.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use fabric_core::detect;
use fabric_core::hook::{self, Decision, HookKind, Msg, ScanCtx};
use fabric_core::ids::{ConvId, IncidentId};
use fabric_core::json::{self, Value};
use fabric_core::phi::{self, Brake, ConvPhi, Phi, PhiEvent, RuleId};

use crate::state::Machine;
use crate::wiring::Wiring;

/// Shorter than the 10-second timeout setup gives every PHI line.
const WATCHDOG: Duration = Duration::from_millis(8_000);
/// The one deliberate non-zero status.
const STOPPED: u8 = 3;
/// Payload caps (§5.4): 256 KiB, and 4 MiB for the events that carry results.
const CAP: usize = 256 * 1024;
const CAP_RESULTS: usize = 4 * 1024 * 1024;

/// A PHI check that could not complete.
#[derive(Debug)]
pub struct Fault;

/// The entry point, with the arguments after `hook`. Never returns an error:
/// it decides, or it fails closed.
pub fn run(args: &[String]) -> ExitCode {
    let armed = arm_watchdog();
    std::panic::set_hook(Box::new(|_| {
        let _ = std::io::stderr().write_all(b"heddle: internal fault (R-00)\n");
    }));
    let kind = match args {
        [harness, event, ..] if harness == "claude-code" => HookKind::parse(event.as_bytes()),
        _ => None,
    };
    let Some(kind) = kind else {
        return emit(Decision::Halt(Msg::Hiccup));
    };
    let outcome = match parse_line(args.get(2..).unwrap_or_default()) {
        Some(line) if armed => std::panic::catch_unwind(|| decide(kind, &line))
            .unwrap_or_else(|_| Some(line.fault(kind))),
        Some(line) => Some(line.fault(kind)),
        None => Some(hook::fault(kind)),
    };
    match outcome {
        Some(decision) => emit(decision),
        // The live check's canary: fail on purpose, print nothing.
        None => ExitCode::from(STOPPED),
    }
}

/// What the hook line says after the event.
#[derive(Debug, PartialEq, Eq)]
struct Line {
    /// The main checkout.
    root: PathBuf,
    /// The state folder.
    state: PathBuf,
    /// `--mode count`: log every hit, decide nothing.
    count_only: bool,
}

impl Line {
    /// What a fault prints: the fault decision, or nothing in count mode.
    fn fault(&self, kind: HookKind) -> Decision {
        if self.count_only {
            Decision::Pass
        } else {
            hook::fault(kind)
        }
    }
}

/// `--root <abs> --state <abs> [--mode brake|count]`, each at most once, in
/// any order; the two paths required and absolute. The root is required now
/// so the wiring is final; the room and the second label copy read it.
fn parse_line(rest: &[String]) -> Option<Line> {
    let (pairs, odd) = rest.as_chunks::<2>();
    if !odd.is_empty() {
        return None;
    }
    let (mut root, mut state, mut mode) = (None, None, None);
    for [flag, value] in pairs {
        let slot = match flag.as_str() {
            "--root" => &mut root,
            "--state" => &mut state,
            "--mode" => &mut mode,
            _ => return None,
        };
        if slot.replace(value.as_str()).is_some() {
            return None;
        }
    }
    let count_only = match mode {
        None | Some("brake") => false,
        Some("count") => true,
        Some(_) => return None,
    };
    fn absolute(p: Option<&str>) -> Option<&Path> {
        p.map(Path::new).filter(|p| p.is_absolute())
    }
    Some(Line {
        root: absolute(root)?.to_path_buf(),
        state: absolute(state)?.to_path_buf(),
        count_only,
    })
}

/// The watchdog. If it cannot be armed, the hook fails closed.
#[allow(clippy::exit)] // ending the process is the watchdog's whole job
fn arm_watchdog() -> bool {
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(|| {
            std::thread::sleep(WATCHDOG);
            let _ = std::io::stderr().write_all(b"heddle: watchdog\n");
            std::process::exit(i32::from(STOPPED));
        })
        .is_ok()
}

/// The decision for this run; `None` only for the live check's canary.
fn decide(kind: HookKind, line: &Line) -> Option<Decision> {
    let Ok(machine) = Machine::open(line.state.clone()) else {
        return Some(line.fault(kind));
    };
    let payload = read_payload(kind);
    if line.count_only {
        count_hits(kind, payload.ok().as_deref(), &machine);
        return Some(Decision::Pass);
    }
    let (Ok(payload), Some(inc)) = (payload, fresh_incident()) else {
        return Some(hook::fault(kind));
    };
    if kind == HookKind::ConfigChange {
        return Some(config_change(&payload, line).unwrap_or_else(|Fault| hook::fault(kind)));
    }
    if kind == HookKind::PreToolUse && json::from_slice(&payload).is_ok_and(|v| is_canary(&v)) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = std::fs::write(line.state.join("canary"), now.to_string());
        return None;
    }
    Some(decide_with(kind, &payload, &machine, inc).unwrap_or_else(|Fault| hook::fault(kind)))
}

/// `heddle doctor --canary` run as a Bash call: the live check.
fn is_canary(v: &Value) -> bool {
    let command = v
        .get("tool_input")
        .and_then(|i| i.get("command"))
        .and_then(Value::as_str);
    v.get("tool_name").and_then(Value::as_str) == Some("Bash")
        && command.is_some_and(
            |c| match c.split_whitespace().collect::<Vec<_>>().as_slice() {
                [prog, "doctor", "--canary"] => prog.trim_matches(['"', '\'']).ends_with("heddle"),
                _ => false,
            },
        )
}

/// Hook line 9 (T6, T15): a change to the main checkout's local settings
/// that leaves this block's hook lines or `disableAllHooks: false` behind,
/// or leaves a file that does not read, is refused for the running session.
fn config_change(raw: &[u8], line: &Line) -> Result<Decision, Fault> {
    let v = json::from_slice(raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw))
        .map_err(|_| Fault)?;
    let ours = line.root.join(".claude/settings.local.json");
    let source = v.get("source").and_then(Value::as_str);
    let file = v.get("file_path").and_then(Value::as_str).map(Path::new);
    if source != Some("local_settings") || file != Some(ours.as_path()) {
        return Ok(Decision::Pass);
    }
    let heddle = std::env::current_exe().map_err(|_| Fault)?;
    let wiring = Wiring {
        heddle: heddle.to_str().ok_or(Fault)?.to_string(),
        root: line.root.to_str().ok_or(Fault)?.to_string(),
        state: line.state.to_str().ok_or(Fault)?.to_string(),
        home: String::new(),
        count_only: false,
    };
    let now = std::fs::read(&ours)
        .ok()
        .and_then(|b| json::from_slice(&b).ok());
    if now.is_some_and(|s| wiring.lines_intact(&s).is_ok()) {
        return Ok(Decision::Pass);
    }
    // Leave a trace for `doctor`: the person may wonder why a change to the
    // file did not apply to sessions already open. Best effort.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = std::fs::write(line.state.join("config-refused"), secs.to_string());
    Ok(Decision::BlockConfig(Msg::WiringKept))
}

/// Count mode: log each rule that fires, once per run, or R-00 when the
/// payload could not be read, and decide nothing. No label is read or written.
fn count_hits(kind: HookKind, raw: Option<&[u8]>, machine: &Machine) {
    let parsed =
        raw.and_then(|r| json::from_slice(r.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(r)).ok());
    let Some(v) = parsed else {
        machine.count("count", kind, RuleId::R00);
        return;
    };
    if let Some(text) = scan_text(kind, &v) {
        // Each rule once per hook run: the JSON text and the raw strings
        // often show the same value twice.
        let mut seen: Vec<RuleId> = Vec::new();
        for h in detect::detect(&text).hits.iter().flatten() {
            if !seen.contains(&h.rule) {
                seen.push(h.rule);
                machine.count("count", kind, h.rule);
            }
        }
    }
}

/// Read the payload, capped. A payload over the cap is a fault.
fn read_payload(kind: HookKind) -> Result<Vec<u8>, Fault> {
    let cap = if matches!(kind, HookKind::PostToolUse | HookKind::PostToolBatch) {
        CAP_RESULTS
    } else {
        CAP
    };
    let limit = u64::try_from(cap.saturating_add(1)).map_err(|_| Fault)?;
    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .take(limit)
        .read_to_end(&mut buf)
        .map_err(|_| Fault)?;
    if buf.len() > cap { Err(Fault) } else { Ok(buf) }
}

/// A random incident id, from the kernel's generator.
fn fresh_incident() -> Option<IncidentId> {
    const CHARS: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut raw = [0u8; 6];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut raw)
        .ok()?;
    let mut id = *b"inc-000000";
    for (slot, r) in id.iter_mut().skip(4).zip(raw) {
        *slot = *CHARS.get(usize::from(r.checked_rem(36)?))?;
    }
    IncidentId::parse(&id)
}

/// Everything a hook scans, as one buffer: the JSON text of a structured value
/// (so record keys such as `"first_name":` show), then each string raw (so
/// line-based formats such as HL7 show).
fn scan_text(kind: HookKind, v: &Value) -> Option<Vec<u8>> {
    let results;
    let target = match kind {
        HookKind::UserPromptSubmit => v.get("prompt")?,
        HookKind::PreToolUse => v.get("tool_input")?,
        HookKind::PostToolUse => v.get("tool_response")?,
        // The results only. PreToolUse scanned every input, and the model
        // wrote them; a denied input here would turn its first strike into a
        // pause. A batch of an unknown shape is scanned whole.
        HookKind::PostToolBatch => {
            let calls = v.get("tool_calls")?;
            results = calls.as_array().map_or_else(
                || calls.clone(),
                |calls| {
                    Value::Array(
                        calls
                            .iter()
                            .filter_map(|c| c.get("tool_response"))
                            .cloned()
                            .collect(),
                    )
                },
            );
            &results
        }
        HookKind::SessionStart
        | HookKind::UserPromptExpansion
        | HookKind::PreCompact
        | HookKind::ConfigChange => return None,
    };
    let mut out = Vec::new();
    if !matches!(target, Value::String(_)) {
        out.extend_from_slice(json::to_string(target).as_bytes());
        out.push(b'\n');
    }
    for s in target.strings() {
        out.extend_from_slice(s.as_bytes());
        out.push(b'\n');
    }
    Some(out)
}

/// The decision, given everything read from the edge. Faults are `Err`.
pub fn decide_with(
    kind: HookKind,
    raw: &[u8],
    machine: &Machine,
    incident: IncidentId,
) -> Result<Decision, Fault> {
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    let v = json::from_slice(raw).map_err(|_| Fault)?;
    if v.get("hook_event_name").and_then(Value::as_str) != Some(kind.name()) {
        return Err(Fault);
    }
    let conv = v
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|s| ConvId::parse(s.as_bytes()))
        .ok_or(Fault)?;
    let tool = v.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let subagent = v.get("agent_id").is_some();

    let _lock = machine.lock().map_err(|_| Fault)?;
    let (mut st, mut dirty) = machine.read_conv(&conv, incident);

    // A prompt starts a turn. The conservative "a person typed it" rule (M13).
    let mut person_typed = true;
    match kind {
        HookKind::UserPromptSubmit => {
            person_typed = !st.sub_since_prompt;
            if st.sub_since_prompt || st.withheld != 0 {
                st.sub_since_prompt = false;
                st.withheld = 0;
                dirty = true;
            }
        }
        HookKind::PreToolUse | HookKind::PostToolUse | HookKind::PostToolBatch
            if subagent && !st.sub_since_prompt =>
        {
            st.sub_since_prompt = true;
            dirty = true;
        }
        HookKind::SessionStart
        | HookKind::UserPromptExpansion
        | HookKind::PreToolUse
        | HookKind::PostToolUse
        | HookKind::PostToolBatch
        | HookKind::PreCompact
        | HookKind::ConfigChange => {}
    }

    // Check 1: the effective label, in every folder, before anything else.
    let brake: Brake = phi::effective(&st.phi, &ConvPhi::Clean, None);
    if let Some(d) = hook::brake_gate(kind, brake) {
        if dirty {
            machine.write_conv(&conv, &st).map_err(|_| Fault)?;
        }
        return Ok(d);
    }

    // The seatbelts: a command that would switch the brake off by accident.
    if kind == HookKind::PreToolUse
        && tool == "Bash"
        && let Some(belt) = v
            .get("tool_input")
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
            .and_then(hook::seatbelt)
    {
        if dirty {
            machine.write_conv(&conv, &st).map_err(|_| Fault)?;
        }
        return Ok(Decision::Deny(Msg::Seatbelt(belt)));
    }

    // The pre-filter on this hook's text.
    let ctx = ScanCtx {
        person_typed,
        can_replace: kind == HookKind::PostToolUse && tool == "Bash",
        passed_model: matches!(tool, "WebFetch" | "WebSearch" | "Task" | "Agent"),
        input_strikes: st.strikes,
    };
    if let Some(text) = scan_text(kind, &v) {
        let verdict = detect::detect(&text);
        for h in verdict.hits.iter().flatten() {
            match hook::on_hit(kind, h.rule, &ctx, incident) {
                Some(out) => {
                    if let Some(ev) = out.event {
                        st.phi = phi::phi_step(st.phi, ev).0;
                        if let Some(t) = v.get("transcript_path").and_then(Value::as_str) {
                            machine.note_transcript(&conv, t);
                        }
                    }
                    if out.strike {
                        st.strikes = st.strikes.saturating_add(1);
                    }
                    if out.withheld {
                        st.withheld = st.withheld.saturating_add(1);
                    }
                    machine.write_conv(&conv, &st).map_err(|_| Fault)?;
                    return Ok(out.decision);
                }
                None => machine.count("brake", kind, h.rule),
            }
        }
    }

    // A prompt that passes while Held is the rewind of a held prompt (T2).
    if kind == HookKind::UserPromptSubmit && st.phi == Phi::Held {
        st.phi = phi::phi_step(st.phi, PhiEvent::PromptClean).0;
        dirty = true;
    }
    if dirty {
        machine.write_conv(&conv, &st).map_err(|_| Fault)?;
    }
    Ok(hook::turn_gate(kind, st.withheld).unwrap_or(Decision::Pass))
}

/// `{"hookSpecificOutput": {"hookEventName": <event>, ...rest}}` plus `top`.
fn specific<const N: usize>(
    event: HookKind,
    rest: [(&str, Value); N],
    top: Vec<(String, Value)>,
) -> Value {
    let mut inner = vec![("hookEventName".to_string(), Value::str(event.name()))];
    inner.extend(rest.into_iter().map(|(k, v)| (k.to_string(), v)));
    let mut outer = top;
    outer.push(("hookSpecificOutput".to_string(), Value::Object(inner)));
    Value::Object(outer)
}

/// `continue: false`, Claude Code's universal stop.
fn stop(m: &Msg) -> Vec<(String, Value)> {
    vec![
        ("continue".to_string(), Value::Bool(false)),
        ("stopReason".to_string(), Value::str(&hook::text(m))),
    ]
}

/// The harness's JSON for a decision; `None` for a pass.
pub fn render(d: Decision) -> Option<Value> {
    let block = |m: &Msg| {
        vec![
            ("decision".to_string(), Value::str("block")),
            ("reason".to_string(), Value::str(&hook::text(m))),
        ]
    };
    let deny = |m: &Msg, top| {
        specific(
            HookKind::PreToolUse,
            [
                ("permissionDecision", Value::str("deny")),
                ("permissionDecisionReason", Value::str(&hook::text(m))),
            ],
            top,
        )
    };
    let v = match d {
        Decision::Pass => return None,
        Decision::Inject(m) => specific(
            HookKind::SessionStart,
            [("additionalContext", Value::str(&hook::text(&m)))],
            vec![],
        ),
        Decision::BlockPrompt {
            msg,
            suppress: true,
        } => specific(
            HookKind::UserPromptSubmit,
            [("suppressOriginalPrompt", Value::Bool(true))],
            block(&msg),
        ),
        Decision::BlockPrompt {
            msg: m,
            suppress: false,
        }
        | Decision::BlockExpansion(m)
        | Decision::BlockCompaction(m)
        | Decision::StopLoop(m)
        | Decision::BlockConfig(m) => Value::Object(block(&m)),
        Decision::Deny(m) => deny(&m, vec![]),
        Decision::DenyAndStop(m) => deny(&m, stop(&m)),
        Decision::ReplaceBash(m) => specific(
            HookKind::PostToolUse,
            [(
                "updatedToolOutput",
                Value::object([
                    ("stdout", Value::str(&hook::text(&m))),
                    ("stderr", Value::str("")),
                    ("interrupted", Value::Bool(false)),
                    ("isImage", Value::Bool(false)),
                ]),
            )],
            vec![],
        ),
        Decision::Halt(m) => Value::Object(stop(&m)),
    };
    Some(v)
}

/// Print the decision in one write. A stop that cannot be printed exits
/// non-zero, so the harness still blocks.
fn emit(d: Decision) -> ExitCode {
    let Some(v) = render(d) else {
        return ExitCode::SUCCESS;
    };
    let mut out = std::io::stdout().lock();
    match out
        .write_all(json::to_string(&v).as_bytes())
        .and_then(|()| out.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(STOPPED),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    // Split across literals so the repo's own scan never sees it whole.
    const SSN: &str = concat!("987-12", "-3456");
    const CONV: &str = "00893aaf-19fa-41d2-8238-13269b9b3ca0";

    fn inc(s: &[u8]) -> IncidentId {
        IncidentId::parse(s).expect("an incident id")
    }

    fn machine(name: &str) -> (Machine, PathBuf) {
        let dir = std::env::temp_dir().join(format!("heddle-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a test state folder");
        (
            Machine::open(dir.clone()).expect("a test state folder"),
            dir,
        )
    }

    /// A payload: the common fields, then `extra` (a JSON object's members).
    fn payload(event: &str, extra: &str) -> Vec<u8> {
        let sep = if extra.is_empty() { "" } else { "," };
        format!(r#"{{"session_id":"{CONV}","transcript_path":"/tmp/t.jsonl","cwd":"/work/repo","hook_event_name":"{event}"{sep}{extra}}}"#)
            .into_bytes()
    }

    fn bash(event: &str, command: &str) -> Vec<u8> {
        payload(
            event,
            &format!(r#""tool_name":"Bash","tool_input":{{"command":"{command}"}}"#),
        )
    }

    fn bash_result(stdout: &str) -> Vec<u8> {
        payload(
            "PostToolUse",
            &format!(r#""tool_name":"Bash","tool_response":{{"stdout":"{stdout}","stderr":""}}"#),
        )
    }

    fn decide(kind: HookKind, raw: &[u8], m: &Machine, i: &[u8]) -> Decision {
        decide_with(kind, raw, m, inc(i)).expect("decides")
    }

    #[test]
    fn decision_3_the_first_tool_input_is_denied_with_the_hint_and_the_second_pauses() {
        let (m, dir) = machine("tool-input");
        let hit = bash("PreToolUse", &format!("echo id {SSN}"));
        let d = decide(HookKind::PreToolUse, &hit, &m, b"inc-aaaaaa");
        assert!(matches!(d, Decision::Deny(Msg::InputHint(_))), "{d:?}");
        // A strike is not a pause: a clean call goes ahead.
        assert_eq!(
            decide(
                HookKind::PreToolUse,
                &bash("PreToolUse", "ls"),
                &m,
                b"inc-bbbbbb"
            ),
            Decision::Pass
        );
        let d = decide(HookKind::PreToolUse, &hit, &m, b"inc-cccccc");
        assert!(
            matches!(d, Decision::DenyAndStop(Msg::Paused(_, i)) if i == inc(b"inc-cccccc")),
            "{d:?}"
        );
        // Now every line is stopped, in any folder.
        let d = decide(
            HookKind::PreToolUse,
            &bash("PreToolUse", "ls"),
            &m,
            b"inc-dddddd",
        );
        assert_eq!(
            d,
            Decision::DenyAndStop(Msg::StillPaused(inc(b"inc-cccccc")))
        );
        let d = decide(
            HookKind::UserPromptSubmit,
            &payload("UserPromptSubmit", r#""prompt":"hello""#),
            &m,
            b"inc-eeeeee",
        );
        assert_eq!(
            d,
            Decision::BlockPrompt {
                msg: Msg::StillPaused(inc(b"inc-cccccc")),
                suppress: false
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_typed_prompt_hit_is_held_and_a_clean_rewrite_releases_it() {
        let (m, dir) = machine("held");
        let hit = payload("UserPromptSubmit", &format!(r#""prompt":"ssn {SSN}""#));
        let d = decide(HookKind::UserPromptSubmit, &hit, &m, b"inc-aaaaaa");
        assert!(
            matches!(
                d,
                Decision::BlockPrompt {
                    msg: Msg::Held(_),
                    suppress: true
                }
            ),
            "{d:?}"
        );
        let exp = payload("UserPromptExpansion", "");
        assert_eq!(
            decide(HookKind::UserPromptExpansion, &exp, &m, b"inc-aaaaaa"),
            Decision::BlockExpansion(Msg::HeldWaiting)
        );
        let rewrite = payload(
            "UserPromptSubmit",
            r#""prompt":"the same question, without the number""#,
        );
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &rewrite, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        assert_eq!(
            decide(HookKind::UserPromptExpansion, &exp, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decision_3_bash_results_are_withheld_and_the_third_in_a_turn_stops_it() {
        let (m, dir) = machine("results");
        let hit = bash_result(&format!("ssn {SSN}"));
        let batch = payload("PostToolBatch", r#""tool_calls":[]"#);
        for n in 1..=3 {
            let d = decide(HookKind::PostToolUse, &hit, &m, b"inc-aaaaaa");
            assert!(
                matches!(d, Decision::ReplaceBash(Msg::Withheld(_))),
                "{n}: {d:?}"
            );
            let d = decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa");
            assert_eq!(
                d == Decision::StopLoop(Msg::TurnStopped),
                n == 3,
                "{n}: {d:?}"
            );
        }
        // The next prompt starts a new turn, and nothing was paused.
        let prompt = payload("UserPromptSubmit", r#""prompt":"go on""#);
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &prompt, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        assert_eq!(
            decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_read_result_or_a_hit_in_the_batch_pauses_the_conversation() {
        let (m, dir) = machine("read");
        let read = payload(
            "PostToolUse",
            &format!(r#""tool_name":"Read","tool_response":{{"file":{{"content":"ssn {SSN}"}}}}"#),
        );
        let d = decide(HookKind::PostToolUse, &read, &m, b"inc-aaaaaa");
        assert!(matches!(d, Decision::Halt(Msg::Paused(..))), "{d:?}");
        let _ = std::fs::remove_dir_all(dir);
        let (m, dir) = machine("batch");
        // A denied input in the batch is not scanned again: its strike stands.
        let denied = payload(
            "PostToolBatch",
            &format!(
                r#""tool_calls":[{{"tool_name":"Bash","tool_input":{{"command":"echo {SSN}"}},"tool_response":"denied"}}]"#
            ),
        );
        assert_eq!(
            decide(HookKind::PostToolBatch, &denied, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        // A result is.
        let batch = payload(
            "PostToolBatch",
            &format!(r#""tool_calls":[{{"tool_name":"Bash","tool_response":"ssn {SSN}"}}]"#),
        );
        let d = decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa");
        assert!(matches!(d, Decision::StopLoop(Msg::Paused(..))), "{d:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_recorded_payload_passes_on_a_clean_conversation() {
        macro_rules! recorded {
            ($name:literal) => {
                include_bytes!(concat!(
                    "../../../adapters/claude-code/payloads/",
                    $name,
                    ".json"
                ))
                .as_slice()
            };
        }
        let (m, dir) = machine("recorded");
        for (kind, raw) in [
            (HookKind::SessionStart, recorded!("SessionStart-startup")),
            (HookKind::SessionStart, recorded!("SessionStart-resume")),
            (HookKind::SessionStart, recorded!("SessionStart-fork")),
            (HookKind::SessionStart, recorded!("SessionStart-compact")),
            (HookKind::SessionStart, recorded!("SessionStart-clear")),
            (HookKind::UserPromptSubmit, recorded!("UserPromptSubmit")),
            (
                HookKind::UserPromptExpansion,
                recorded!("UserPromptExpansion"),
            ),
            (HookKind::PreToolUse, recorded!("PreToolUse")),
            (HookKind::PostToolUse, recorded!("PostToolUse")),
            (HookKind::PostToolBatch, recorded!("PostToolBatch")),
            (HookKind::PreCompact, recorded!("PreCompact")),
            (HookKind::ConfigChange, recorded!("ConfigChange")),
        ] {
            assert_eq!(
                decide_with(kind, raw, &m, inc(b"inc-aaaaaa")).ok(),
                Some(Decision::Pass),
                "{kind:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn t6_line_9_refuses_a_change_that_drops_a_line_or_does_not_parse() {
        let (_, dir) = machine("line9");
        let root = dir.join("repo");
        std::fs::create_dir_all(root.join(".claude")).expect("dirs");
        let file = root.join(".claude/settings.local.json");
        let line = Line {
            root: root.clone(),
            state: dir.join("state"),
            count_only: false,
        };
        let wiring = Wiring {
            heddle: std::env::current_exe()
                .expect("exe")
                .to_str()
                .expect("utf8")
                .to_string(),
            root: root.to_str().expect("utf8").to_string(),
            state: dir.join("state").to_str().expect("utf8").to_string(),
            home: String::new(),
            count_only: false,
        };
        let good = json::to_string(&wiring.merge(None).expect("merges"));
        let change = |path: &Path| {
            payload(
                "ConfigChange",
                &format!(
                    r#""source":"local_settings","file_path":"{}""#,
                    path.display()
                ),
            )
        };
        std::fs::write(&file, &good).expect("write");
        assert_eq!(
            config_change(&change(&file), &line).ok(),
            Some(Decision::Pass)
        );
        for broken in [
            good.replace(r#""onFailure":"block""#, ""),
            good.replace("PreToolUse", "Pre"),
            "{\"hooks\": ".to_string(),
        ] {
            std::fs::write(&file, broken).expect("write");
            assert_eq!(
                config_change(&change(&file), &line).ok(),
                Some(Decision::BlockConfig(Msg::WiringKept))
            );
        }
        std::fs::remove_file(&file).expect("rm");
        assert_eq!(
            config_change(&change(&file), &line).ok(),
            Some(Decision::BlockConfig(Msg::WiringKept))
        );
        // Another file, or another source, is not line 9's business.
        assert_eq!(
            config_change(&change(&dir.join("other.json")), &line).ok(),
            Some(Decision::Pass)
        );
        let user = payload(
            "ConfigChange",
            r#""source":"user_settings","file_path":"/x""#,
        );
        assert_eq!(config_change(&user, &line).ok(), Some(Decision::Pass));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_canary_is_heddle_doctor_canary_run_as_bash() {
        let c = |cmd: &str| is_canary(&json::from_slice(&bash("PreToolUse", cmd)).expect("json"));
        assert!(c("/Users/me/.local/bin/heddle doctor --canary"));
        assert!(c("heddle doctor --canary"));
        assert!(!c("heddle doctor"));
        assert!(!c("echo heddle doctor --canary"));
        assert!(!c("/bin/cat doctor --canary"));
    }

    #[test]
    fn h1e_bad_input_is_a_fault() {
        let (m, dir) = machine("faults");
        let i = inc(b"inc-aaaaaa");
        assert!(decide_with(HookKind::PreToolUse, b"not json", &m, i).is_err());
        assert!(decide_with(HookKind::PreToolUse, &payload("PostToolUse", ""), &m, i).is_err());
        let no_session = br#"{"hook_event_name":"PreToolUse","cwd":"/x"}"#;
        assert!(decide_with(HookKind::PreToolUse, no_session, &m, i).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_an_unreadable_label_file_is_read_as_paused() {
        let (m, dir) = machine("unreadable");
        let conv = ConvId::parse(CONV.as_bytes()).expect("conv");
        std::fs::write(m.conv_path(&conv), b"fabric-conv 9\nphi clean\nsub 0\n").expect("write");
        let d = decide(
            HookKind::PreToolUse,
            &bash("PreToolUse", "ls"),
            &m,
            b"inc-dddddd",
        );
        assert_eq!(
            d,
            Decision::DenyAndStop(Msg::StillPaused(inc(b"inc-dddddd")))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_arguments_are_both_paths_once_and_absolute_and_an_optional_mode() {
        let a = |s: &[&str]| parse_line(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        let brake = Some(Line {
            root: PathBuf::from("/r"),
            state: PathBuf::from("/s"),
            count_only: false,
        });
        assert_eq!(a(&["--root", "/r", "--state", "/s"]), brake);
        assert_eq!(a(&["--state", "/s", "--root", "/r"]), brake);
        assert_eq!(
            a(&["--root", "/r", "--state", "/s", "--mode", "brake"]),
            brake
        );
        let count = Some(Line {
            root: PathBuf::from("/r"),
            state: PathBuf::from("/s"),
            count_only: true,
        });
        assert_eq!(
            a(&["--mode", "count", "--root", "/r", "--state", "/s"]),
            count
        );
        for bad in [
            &["--state", "/s"][..],
            &["--root", "/r"],
            &["--root", "/r", "--state", "s"],
            &["--root", "r", "--state", "/s"],
            &["--root", "/r", "--state", "/s", "--state", "/t"],
            &["--root", "/r", "--state"],
            &["--root", "/r", "--other", "/s"],
            &["--root", "/r", "--state", "/s", "--mode", "off"],
            &[
                "--root", "/r", "--state", "/s", "--mode", "count", "--mode", "count",
            ],
            &[],
        ] {
            assert_eq!(a(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn decision_4_count_mode_logs_hits_and_never_touches_a_label() {
        let (m, dir) = machine("count");
        count_hits(
            HookKind::PreToolUse,
            Some(&bash("PreToolUse", &format!("echo id {SSN}"))),
            &m,
        );
        count_hits(HookKind::PreToolUse, Some(&bash("PreToolUse", "ls")), &m);
        count_hits(HookKind::PostToolUse, Some(b"not json"), &m);
        count_hits(HookKind::PostToolUse, None, &m);
        let log = std::fs::read_to_string(dir.join("counts")).expect("a counts file");
        let lines: Vec<Vec<&str>> = log
            .lines()
            .map(|l| l.split(' ').skip(1).collect())
            .collect();
        assert_eq!(
            lines,
            [
                ["count", "PreToolUse", "R-09"],
                ["count", "PostToolUse", "R-00"],
                ["count", "PostToolUse", "R-00"]
            ]
        );
        assert!(log.lines().all(|l| {
            l.split(' ')
                .next()
                .is_some_and(|t| t.parse::<u64>().is_ok())
        }));
        let conv = ConvId::parse(CONV.as_bytes()).expect("conv");
        assert!(!m.conv_path(&conv).exists(), "no label in count mode");
        let line = Line {
            root: dir.clone(),
            state: dir.clone(),
            count_only: true,
        };
        assert_eq!(line.fault(HookKind::PreToolUse), Decision::Pass);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_rendered_json_has_the_shapes_claude_code_reads() {
        let i = inc(b"inc-aaaaaa");
        let r = |d| json::to_string(&render(d).expect("json"));
        assert_eq!(render(Decision::Pass), None);
        let deny = render(Decision::Deny(Msg::Hiccup)).expect("json");
        let inner = deny.get("hookSpecificOutput").expect("inner");
        assert_eq!(
            inner.get("permissionDecision").and_then(Value::as_str),
            Some("deny")
        );
        assert_eq!(
            inner.get("hookEventName").and_then(Value::as_str),
            Some("PreToolUse")
        );
        assert_eq!(deny.get("continue"), None);
        let stop = render(Decision::DenyAndStop(Msg::StillPaused(i))).expect("json");
        assert_eq!(stop.get("continue").and_then(Value::as_bool), Some(false));
        assert!(
            r(Decision::BlockPrompt {
                msg: Msg::Held(phi::RuleId::R01),
                suppress: true
            })
            .contains(r#""suppressOriginalPrompt":true"#)
        );
        let fault = r(Decision::BlockPrompt {
            msg: Msg::Hiccup,
            suppress: false,
        });
        assert!(
            fault.starts_with(r#"{"decision":"block","reason":"#) && !fault.contains("suppress")
        );
        let replace = render(Decision::ReplaceBash(Msg::Hiccup)).expect("json");
        let out = replace
            .get("hookSpecificOutput")
            .and_then(|o| o.get("updatedToolOutput"))
            .expect("output");
        assert_eq!(out.get("isImage").and_then(Value::as_bool), Some(false));
        assert_eq!(
            r(Decision::Halt(Msg::Sealed)),
            format!(
                r#"{{"continue":false,"stopReason":"{}"}}"#,
                hook::text(&Msg::Sealed)
            )
        );
    }
}
