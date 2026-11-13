//! `heddle hook claude-code <Event>`: the PHI checks of every hook line.
//!
//! The order of the first three things is the safety argument:
//! 1. the watchdog is armed before any read (decision new-15): a heddle that
//!    hangs ends itself non-zero before the line's timeout, and the harness's
//!    `onFailure: "block"` then blocks the action (day 0 verified both routes);
//! 2. the panic hook prints one fixed line, so a panic message, which can
//!    quote data, never reaches a model;
//! 3. everything else runs inside `catch_unwind`, and any fault becomes the
//!    decision a hit would give (rule R-00). Bad arguments, an unknown harness
//!    and an unknown event are faults too.
//!
//! Exit codes (§5.4): 0 with one complete JSON object, or with nothing for a
//! pass. The only deliberate non-zero exits are the watchdog's and a stop
//! that could not be printed; both make the harness block.

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use fabric_core::detect;
use fabric_core::hook::{self, Decision, HookKind, Msg, ScanCtx};
use fabric_core::ids::{ConvId, IncidentId};
use fabric_core::phi::{self, Brake, ConvPhi, Phi, PhiEvent};
use serde_json::{Value, json};

use crate::state::Machine;

/// Shorter than the 10-second timeout `setup` gives every PHI line.
const WATCHDOG: Duration = Duration::from_millis(8_000);
/// The one deliberate non-zero status.
const STOPPED: u8 = 3;
/// Payload caps (§5.4): 256 KiB, and 4 MiB for the events that carry results.
const CAP: usize = 256 * 1024;
const CAP_RESULTS: usize = 4 * 1024 * 1024;
/// Root-owned, written by `setup`: one enrolled repo root per line.
const ENROLLED: &str = "/usr/local/fabric/etc/enrolled";

/// A PHI check that could not complete.
#[derive(Debug)]
pub struct Fault;

/// The entry point. Never returns an error: it decides, or it fails closed.
pub fn run(harness: Option<&str>, event: Option<&str>) -> ExitCode {
    let armed = arm_watchdog();
    std::panic::set_hook(Box::new(|_| {
        let _ = std::io::stderr().write_all(b"heddle: internal fault (R-00)\n");
    }));
    let kind = event.and_then(|e| HookKind::parse(e.as_bytes()));
    let decision = match (harness, kind) {
        (Some("claude-code"), Some(k)) if armed => {
            std::panic::catch_unwind(|| decide(k)).unwrap_or_else(|_| hook::fault(k))
        }
        (Some("claude-code"), Some(k)) => hook::fault(k),
        _ => Decision::Halt(Msg::Fault),
    };
    emit(decision)
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

fn decide(kind: HookKind) -> Decision {
    let enrolled = match read_enrolled(Path::new(ENROLLED)) {
        Ok(e) => e,
        Err(Fault) => return hook::fault(kind),
    };
    let payload = match read_payload(kind) {
        Ok(p) => p,
        Err(Fault) => return hook::fault(kind),
    };
    let machine = match Machine::open() {
        Ok(m) => m,
        Err(_) => return hook::fault(kind),
    };
    match fresh_incident() {
        Some(inc) => decide_with(kind, &payload, &machine, &enrolled, inc)
            .unwrap_or_else(|Fault| hook::fault(kind)),
        None => hook::fault(kind),
    }
}

/// Read the payload, capped. A payload over the cap is R-07 and blocks.
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

/// The enrolled roots. No file means nothing is enrolled; a file that cannot
/// be read is a fault.
fn read_enrolled(path: &Path) -> Result<Vec<String>, Fault> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| Fault)?;
            Ok(text
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with('/'))
                .map(String::from)
                .collect())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(Fault),
    }
}

/// Whether `cwd` is an enrolled root or inside one.
fn enforced(cwd: &str, enrolled: &[String]) -> bool {
    enrolled.iter().any(|root| {
        let root = root.trim_end_matches('/');
        cwd == root
            || cwd
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    })
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
fn scan_text(kind: HookKind, v: &Value) -> Result<Option<Vec<u8>>, Fault> {
    let target = match kind {
        HookKind::UserPromptSubmit => v.get("prompt"),
        HookKind::PreToolUse => v.get("tool_input"),
        HookKind::PostToolUse => v.get("tool_response"),
        HookKind::PostToolBatch => v.get("tool_calls"),
        HookKind::SessionStart
        | HookKind::UserPromptExpansion
        | HookKind::PreCompact
        | HookKind::ConfigChange => None,
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let mut out = Vec::new();
    if !target.is_string() {
        // A value that cannot be shown to the scanner is a fault, never a pass.
        out.extend_from_slice(serde_json::to_string(target).map_err(|_| Fault)?.as_bytes());
        out.push(b'\n');
    }
    let mut stack = vec![target];
    while let Some(x) = stack.pop() {
        match x {
            Value::String(s) => {
                out.extend_from_slice(s.as_bytes());
                out.push(b'\n');
            }
            Value::Array(a) => stack.extend(a.iter().rev()),
            Value::Object(o) => stack.extend(o.values().rev()),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(Some(out))
}

/// The decision, given everything read from the edge. Faults are `Err`.
pub fn decide_with(
    kind: HookKind,
    raw: &[u8],
    machine: &Machine,
    enrolled: &[String],
    incident: IncidentId,
) -> Result<Decision, Fault> {
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    let v: Value = serde_json::from_slice(raw).map_err(|_| Fault)?;
    if v.get("hook_event_name").and_then(Value::as_str) != Some(kind.name()) {
        return Err(Fault);
    }
    let conv = v
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|s| ConvId::parse(s.as_bytes()))
        .ok_or(Fault)?;
    let cwd = v.get("cwd").and_then(Value::as_str).ok_or(Fault)?;
    let tool = v.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let subagent = v.get("agent_id").is_some();

    let _lock = machine.lock().map_err(|_| Fault)?;
    let (mut st, mut dirty) = machine.read_conv(&conv, incident);

    // The conservative "a person typed it" rule (M13).
    let mut person_typed = true;
    match kind {
        HookKind::UserPromptSubmit => {
            person_typed = !st.sub_since_prompt;
            if st.sub_since_prompt {
                st.sub_since_prompt = false;
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

    // The pre-filter on this hook's text.
    let ctx = ScanCtx {
        enforce: enforced(cwd, enrolled),
        person_typed,
        can_replace: kind == HookKind::PostToolUse && tool == "Bash",
        passed_model: matches!(tool, "WebFetch" | "WebSearch" | "Task" | "Agent"),
    };
    if let Some(text) = scan_text(kind, &v)? {
        let verdict = detect::detect(&text);
        for h in verdict.hits.iter().flatten() {
            match hook::on_hit(kind, h.rule, &ctx, incident) {
                Some((decision, event)) => {
                    let (next, _) = phi::phi_step(st.phi, event);
                    st.phi = next;
                    machine.write_conv(&conv, &st).map_err(|_| Fault)?;
                    return Ok(decision);
                }
                None => machine.count(kind, h.rule),
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
    Ok(Decision::Pass)
}

/// The harness's JSON for a decision; `None` for a pass.
pub fn render(d: Decision) -> Option<Value> {
    let v = match d {
        Decision::Pass => return None,
        Decision::Inject(m) => json!({
            "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": hook::text(&m)}
        }),
        Decision::BlockPrompt(m) => json!({
            "decision": "block",
            "reason": hook::text(&m),
            "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "suppressOriginalPrompt": true}
        }),
        Decision::BlockExpansion(m) | Decision::BlockCompaction(m) | Decision::StopLoop(m) => {
            json!({"decision": "block", "reason": hook::text(&m)})
        }
        Decision::Deny(m) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": hook::text(&m)
            }
        }),
        Decision::ReplaceBash(m) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": {"stdout": hook::text(&m), "stderr": "", "interrupted": false, "isImage": false}
            }
        }),
        Decision::Halt(m) => json!({"continue": false, "stopReason": hook::text(&m)}),
    };
    Some(v)
}

/// Print the decision in one write. A stop that cannot be printed exits
/// non-zero, so the harness still blocks.
fn emit(d: Decision) -> ExitCode {
    let Some(v) = render(d) else {
        return ExitCode::SUCCESS;
    };
    let Ok(bytes) = serde_json::to_vec(&v) else {
        return ExitCode::from(STOPPED);
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(&bytes).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(STOPPED),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use super::*;
    use crate::state::Machine;

    // Split across literals so the repo's own scan never sees it whole.
    const SSN: &str = concat!("987-12", "-3456");

    fn inc(s: &[u8]) -> IncidentId {
        IncidentId::parse(s).expect("an incident id")
    }

    fn machine(name: &str) -> (Machine, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("heddle-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (
            Machine::at(dir.clone()).expect("a test machine folder"),
            dir,
        )
    }

    fn payload(event: &str, extra: Value) -> Vec<u8> {
        let mut v = json!({
            "session_id": "00893aaf-19fa-41d2-8238-13269b9b3ca0",
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/work/repo",
            "hook_event_name": event,
        });
        for (k, x) in extra.as_object().expect("an object") {
            v[k] = x.clone();
        }
        serde_json::to_vec(&v).expect("json")
    }

    fn enrolled() -> Vec<String> {
        vec!["/work/repo".to_string()]
    }

    #[test]
    fn a_tool_input_hit_denies_and_then_every_hook_is_braked() {
        let (m, dir) = machine("tool-input");
        let i = inc(b"inc-aaaaaa");
        let hit = payload(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": format!("echo id {SSN}")}}),
        );
        let d = decide_with(HookKind::PreToolUse, &hit, &m, &enrolled(), i).expect("decides");
        assert_eq!(d, Decision::Deny(Msg::Braked(i)));
        // The same conversation is now braked, in any folder, enrolled or not.
        let clean = payload(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/elsewhere"}),
        );
        let d = decide_with(HookKind::PreToolUse, &clean, &m, &[], inc(b"inc-bbbbbb"))
            .expect("decides");
        assert_eq!(d, Decision::Deny(Msg::Braked(i)));
        let prompt = payload("UserPromptSubmit", json!({"prompt": "hello"}));
        let d = decide_with(
            HookKind::UserPromptSubmit,
            &prompt,
            &m,
            &[],
            inc(b"inc-cccccc"),
        )
        .expect("decides");
        assert_eq!(d, Decision::BlockPrompt(Msg::Braked(i)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_typed_prompt_hit_is_held_and_a_clean_rewrite_releases_it() {
        let (m, dir) = machine("held");
        let i = inc(b"inc-aaaaaa");
        let hit = payload("UserPromptSubmit", json!({"prompt": format!("ssn {SSN}")}));
        let d = decide_with(HookKind::UserPromptSubmit, &hit, &m, &enrolled(), i).expect("decides");
        assert!(matches!(d, Decision::BlockPrompt(Msg::PromptHeld(_))));
        let exp = payload("UserPromptExpansion", json!({}));
        let d =
            decide_with(HookKind::UserPromptExpansion, &exp, &m, &enrolled(), i).expect("decides");
        assert_eq!(d, Decision::BlockExpansion(Msg::Held));
        let rewrite = payload(
            "UserPromptSubmit",
            json!({"prompt": "the same question, without the number"}),
        );
        let d =
            decide_with(HookKind::UserPromptSubmit, &rewrite, &m, &enrolled(), i).expect("decides");
        assert_eq!(d, Decision::Pass);
        let d =
            decide_with(HookKind::UserPromptExpansion, &exp, &m, &enrolled(), i).expect("decides");
        assert_eq!(d, Decision::Pass);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn outside_an_enrolled_repo_rule_hits_only_count() {
        let (m, dir) = machine("shadow");
        let hit = payload(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": format!("echo {SSN}")}}),
        );
        let d =
            decide_with(HookKind::PreToolUse, &hit, &m, &[], inc(b"inc-aaaaaa")).expect("decides");
        assert_eq!(d, Decision::Pass);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bash_result_is_replaced_and_a_read_result_halts() {
        let (m, dir) = machine("results");
        let i = inc(b"inc-aaaaaa");
        let bash = payload(
            "PostToolUse",
            json!({"tool_name": "Bash", "tool_response": {"stdout": format!("ssn {SSN}"), "stderr": ""}}),
        );
        let d = decide_with(HookKind::PostToolUse, &bash, &m, &enrolled(), i).expect("decides");
        assert!(matches!(
            d,
            Decision::ReplaceBash(Msg::ResultWithheld(_, _))
        ));
        let other = payload(
            "PostToolUse",
            json!({"session_id": "00893aaf-19fa-41d2-8238-13269b9b3ca1", "tool_name": "Read", "tool_response": {"file": {"content": format!("ssn {SSN}")}}}),
        );
        let d = decide_with(HookKind::PostToolUse, &other, &m, &enrolled(), i).expect("decides");
        assert!(matches!(d, Decision::Halt(Msg::ResultWithheld(_, _))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn h1e_bad_input_is_a_fault() {
        let (m, dir) = machine("faults");
        let i = inc(b"inc-aaaaaa");
        assert!(decide_with(HookKind::PreToolUse, b"not json", &m, &[], i).is_err());
        let wrong_event = payload("PostToolUse", json!({}));
        assert!(decide_with(HookKind::PreToolUse, &wrong_event, &m, &[], i).is_err());
        let no_session = br#"{"hook_event_name":"PreToolUse","cwd":"/x"}"#;
        assert!(decide_with(HookKind::PreToolUse, no_session, &m, &[], i).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_an_unreadable_label_file_is_read_as_braked() {
        let (m, dir) = machine("unreadable");
        let conv = ConvId::parse(b"00893aaf-19fa-41d2-8238-13269b9b3ca0").expect("conv");
        std::fs::write(m.conv_path(&conv), b"fabric-conv 9\nphi clean\nsub 0\n").expect("write");
        let clean = payload(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        );
        let i = inc(b"inc-dddddd");
        let d = decide_with(HookKind::PreToolUse, &clean, &m, &[], i).expect("decides");
        assert_eq!(d, Decision::Deny(Msg::Braked(i)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_rendered_json_has_the_shapes_claude_code_reads() {
        let i = inc(b"inc-aaaaaa");
        assert_eq!(render(Decision::Pass), None);
        let deny = render(Decision::Deny(Msg::Braked(i))).expect("json");
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        let block = render(Decision::BlockPrompt(Msg::Fault)).expect("json");
        assert_eq!(block["decision"], "block");
        assert_eq!(block["hookSpecificOutput"]["suppressOriginalPrompt"], true);
        let replace = render(Decision::ReplaceBash(Msg::Fault)).expect("json");
        assert_eq!(
            replace["hookSpecificOutput"]["updatedToolOutput"]["isImage"],
            false
        );
        let halt = render(Decision::Halt(Msg::Fault)).expect("json");
        assert_eq!(halt["continue"], false);
    }

    #[test]
    fn enforcement_follows_the_enrolled_roots_on_path_boundaries() {
        let roots = vec!["/work/repo".to_string()];
        assert!(enforced("/work/repo", &roots));
        assert!(enforced("/work/repo/src", &roots));
        assert!(!enforced("/work/repo-other", &roots));
        assert!(!enforced("/work", &roots));
    }
}
