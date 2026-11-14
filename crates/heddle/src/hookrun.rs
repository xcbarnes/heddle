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
use fabric_core::hook::{self, Decision, HookKind, Msg, ResultShape, ScanCtx};
use fabric_core::ids::{ConvId, IncidentId};
use fabric_core::json::{self, Value};
use fabric_core::phi::{self, Brake, ConvPhi, Phi, PhiEvent, RuleId};

use crate::state::{Machine, Pending};
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
        return emit(Decision::Halt(Msg::Hiccup), "");
    };
    let outcome = match parse_line(args.get(2..).unwrap_or_default()) {
        Some(line) if armed => std::panic::catch_unwind(|| decide(kind, &line))
            .unwrap_or_else(|_| Some((line.fault(kind), String::new()))),
        Some(line) => Some((line.fault(kind), String::new())),
        None => Some((hook::fault(kind), String::new())),
    };
    match outcome {
        Some((decision, file_path)) => emit(decision, &file_path),
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

/// The decision for this run, with the Read call's file path for a
/// replacement; `None` only for the live check's canary.
fn decide(kind: HookKind, line: &Line) -> Option<(Decision, String)> {
    let plain = |d: Decision| Some((d, String::new()));
    let Ok(machine) = Machine::open(line.state.clone()) else {
        return plain(line.fault(kind));
    };
    let machine = machine.with_second(line.root.join(".git/fabric/labels"));
    let payload = read_payload(kind);
    if line.count_only {
        count_hits(kind, payload.ok().as_deref(), &machine);
        return plain(Decision::Pass);
    }
    let (Ok(payload), Some(inc)) = (payload, fresh_incident()) else {
        return plain(hook::fault(kind));
    };
    if kind == HookKind::ConfigChange {
        return plain(config_change(&payload, line).unwrap_or_else(|Fault| hook::fault(kind)));
    }
    if kind == HookKind::PreToolUse && json::from_slice(&payload).is_ok_and(|v| is_canary(&v)) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = std::fs::write(line.state.join("canary"), now.to_string());
        return None;
    }
    let d = decide_with(kind, &payload, &machine, inc).unwrap_or_else(|Fault| hook::fault(kind));
    let file_path = if matches!(d, Decision::Replace(_, ResultShape::Read)) {
        json::from_slice(&payload)
            .ok()
            .and_then(|v| {
                v.get("tool_input")?
                    .get("file_path")?
                    .as_str()
                    .map(String::from)
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some((d, file_path))
}

/// The newest message id in a transcript, from its last 256 KiB.
fn last_message_id(path: &str) -> Option<String> {
    use std::io::Seek;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(std::io::SeekFrom::Start(len.saturating_sub(256 * 1024)))
        .ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let key = "\"uuid\":\"";
    let at = text.rfind(key)?.checked_add(key.len())?;
    let id = text.get(at..at.checked_add(36)?)?;
    ConvId::parse(id.as_bytes()).map(|_| id.to_string())
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

    // Fork matching (T13): a fork gets a new id and no label, but keeps its
    // parent's message ids. Its transcript is written only after its first
    // prompt, so it is checked at each blocking hook until it can be read.
    let transcript = v.get("transcript_path").and_then(Value::as_str);
    if kind == HookKind::SessionStart && v.get("source").and_then(Value::as_str) == Some("fork") {
        if !machine.flagged_ids().is_empty() {
            machine.mark_fork(&conv, true);
        }
    } else if machine.fork_pending(&conv)
        && let Some(bytes) = transcript.and_then(|t| std::fs::read(t).ok())
    {
        let text = String::from_utf8_lossy(&bytes);
        if machine
            .flagged_ids()
            .iter()
            .any(|id| text.contains(id.as_str()))
        {
            st.phi = phi::phi_step(st.phi, PhiEvent::Flag { incident }).0;
            dirty = true;
            note_pause(machine, &conv, transcript);
        }
        machine.mark_fork(&conv, false);
    }
    let t_meta = transcript.and_then(|t| std::fs::metadata(t).ok());
    let t_len = t_meta.as_ref().map_or(0, std::fs::Metadata::len);

    // The pending set (§5). A batch reports its calls back; one that never
    // does means a hook died after a tool ran.
    if kind == HookKind::PostToolBatch {
        let done: Vec<&str> = v
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| c.get("tool_use_id")?.as_str())
                    .collect()
            })
            .unwrap_or_default();
        machine.pending_keep(&conv, |p| !done.contains(&p.id.as_str()));
    }
    // At a prompt or a compaction every pending call is stale; before a tool,
    // a call from another prompt is. The bytes added since a stale call are
    // scanned: a hit, or a transcript heddle cannot read, pauses.
    let prompt_id = v.get("prompt_id").and_then(Value::as_str).unwrap_or("");
    let stale = |p: &Pending| match kind {
        HookKind::UserPromptSubmit | HookKind::PreCompact => true,
        HookKind::PreToolUse => !prompt_id.is_empty() && p.prompt != prompt_id,
        HookKind::SessionStart
        | HookKind::UserPromptExpansion
        | HookKind::PostToolUse
        | HookKind::PostToolBatch
        | HookKind::ConfigChange => false,
    };
    let stale_calls: Vec<Pending> = machine
        .pending(&conv)
        .into_iter()
        .filter(|p| stale(p))
        .collect();
    for p in &stale_calls {
        // A transcript that was not on disk when the call ran (sessions that
        // keep none) and still is not cannot hold the result: nothing to scan.
        // One that appeared since is scanned whole.
        let absent = p.offset == NO_TRANSCRIPT;
        if absent && !p.transcript.exists() {
            continue;
        }
        let from = if absent { 0 } else { p.offset };
        let found = read_from(&p.transcript, from).map(|b| fabric_core::transcript::scan(&b).0);
        let ev = match found.as_deref() {
            Some([]) => None,
            Some([rule, ..]) => Some(PhiEvent::TranscriptHit {
                rule: *rule,
                incident,
            }),
            None => Some(PhiEvent::Flag { incident }),
        };
        if let Some(ev) = ev {
            let was = matches!(st.phi, Phi::Exposed { .. } | Phi::Sealed { .. });
            st.phi = phi::phi_step(st.phi, ev).0;
            dirty = true;
            if !was {
                note_pause(machine, &conv, transcript);
            }
        }
    }
    if !stale_calls.is_empty() {
        machine.pending_keep(&conv, |p| !stale_calls.contains(p));
    }

    // At a prompt: the rewind proof first (a paused conversation whose resent
    // prompt's chain lies wholly before the flagged turn), then where this
    // turn starts.
    if kind == HookKind::UserPromptSubmit {
        if let Phi::Exposed {
            incident: flagged, ..
        } = st.phi
            && let Some(prompt) = v.get("prompt").and_then(Value::as_str)
            && let Some((start, paused_at)) = machine.rewind_marks(&conv)
            && let Some(bytes) = transcript.and_then(|t| std::fs::read(t).ok())
            && let Some(leaf) = fabric_core::transcript::resent_prompt(&bytes, prompt, paused_at)
            && fabric_core::transcript::chain_clear(&bytes, &leaf, start)
        {
            st.phi = phi::phi_step(st.phi, PhiEvent::ChainProvedClear { incident: flagged }).0;
            dirty = true;
        }
        machine.set_number(&conv, "turn", t_len);
    }

    // The transcript scan, count-only (§5): the bytes added since the last
    // scan, before each model call, and any @file a prompt names. It logs;
    // it never decides.
    if matches!(kind, HookKind::UserPromptSubmit | HookKind::PostToolBatch)
        && let Some(t) = transcript
    {
        match machine.number(&conv, "scanned") {
            Some(from) if from <= t_len => {
                let read = read_from(Path::new(t), from);
                let (rules, used) = read
                    .as_deref()
                    .map_or((Vec::new(), 0), fabric_core::transcript::scan);
                for r in rules {
                    machine.count("scan", kind, r);
                }
                if read.is_none() {
                    machine.count("scan", kind, RuleId::R07);
                }
                let next = if read.is_none() {
                    t_len
                } else {
                    from.saturating_add(u64::try_from(used).unwrap_or(0))
                };
                machine.set_number(&conv, "scanned", next);
            }
            // First sight, or a transcript that was rewritten: start here.
            Some(_) | None => machine.set_number(&conv, "scanned", t_len),
        }
    }
    if kind == HookKind::UserPromptSubmit {
        let cwd = v.get("cwd").and_then(Value::as_str).map(Path::new);
        for file in at_files(v.get("prompt").and_then(Value::as_str).unwrap_or(""), cwd) {
            let bytes = read_from(&file, 0).unwrap_or_default();
            let found = detect::detect(&bytes);
            for h in found.hits.iter().flatten() {
                machine.count("scan", kind, h.rule);
            }
        }
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
        result_shape: if kind == HookKind::PostToolUse {
            ResultShape::of(tool)
        } else {
            None
        },
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
                        if matches!(st.phi, Phi::Exposed { .. }) {
                            note_pause(machine, &conv, transcript);
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
    // This call will run: it joins the pending set until its batch reports.
    if kind == HookKind::PreToolUse
        && let (Some(id), Some(t)) = (v.get("tool_use_id").and_then(Value::as_str), transcript)
    {
        let p = Pending {
            id: id.to_string(),
            offset: if t_meta.is_some() {
                t_len
            } else {
                NO_TRANSCRIPT
            },
            prompt: prompt_id.to_string(),
            transcript: PathBuf::from(t),
        };
        machine.pending_add(&conv, &p).map_err(|_| Fault)?;
    }
    Ok(hook::turn_gate(kind, st.withheld).unwrap_or(Decision::Pass))
}

/// A pending call's offset when its transcript was not on disk yet.
const NO_TRANSCRIPT: u64 = u64::MAX;

/// When a conversation is paused: note its transcript (for `status` and
/// `uninstall`), its newest message id (for fork matching), and the rewind
/// proof's offsets: where the flagged turn began and how long the transcript
/// was. Best effort, apart from the offsets, which are synced.
fn note_pause(machine: &Machine, conv: &ConvId, transcript: Option<&str>) {
    let Some(t) = transcript else {
        return;
    };
    machine.note_transcript(conv, t);
    if let Some(id) = last_message_id(t) {
        machine.note_flagged(conv, &id);
    }
    let len = std::fs::metadata(t).map_or(0, |m| m.len());
    let start = machine
        .number(conv, "turn")
        .filter(|s| *s <= len)
        .unwrap_or(0);
    machine.set_rewind_marks(conv, start, len);
}

/// Up to 16 MiB of a file from `from` on. `None` if it cannot be read, is
/// shorter than `from`, or holds more than that.
fn read_from(path: &Path, from: u64) -> Option<Vec<u8>> {
    use std::io::Seek;
    const MAX: u64 = 16 * 1024 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < from || len.saturating_sub(from) > MAX {
        return None;
    }
    f.seek(std::io::SeekFrom::Start(from)).ok()?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

/// The files a prompt names with `@`: Claude Code adds them without a tool
/// call. Paths are taken relative to the session's folder.
fn at_files(prompt: &str, cwd: Option<&Path>) -> Vec<PathBuf> {
    prompt
        .split_whitespace()
        .filter_map(|w| w.strip_prefix('@'))
        .map(|w| w.trim_end_matches([',', '.', ';', ':', ')', '!', '?']))
        .filter(|w| !w.is_empty())
        .map(|w| match cwd {
            Some(c) if !w.starts_with('/') => c.join(w),
            Some(_) | None => PathBuf::from(w),
        })
        .filter(|p| p.is_file())
        .take(16)
        .collect()
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

/// The withheld sentence in the replaced tool's own output shape, matching
/// the recorded PostToolUse payloads (`adapters/claude-code/payloads/`).
fn replacement(shape: ResultShape, text: &str, file_path: &str) -> Value {
    let n = |x: &str| Value::Number(x.to_string());
    match shape {
        ResultShape::Bash => Value::object([
            ("stdout", Value::str(text)),
            ("stderr", Value::str("")),
            ("interrupted", Value::Bool(false)),
            ("isImage", Value::Bool(false)),
        ]),
        ResultShape::Read => Value::object([
            ("type", Value::str("text")),
            (
                "file",
                Value::object([
                    ("filePath", Value::str(file_path)),
                    ("content", Value::str(text)),
                    ("numLines", n("1")),
                    ("startLine", n("1")),
                    ("totalLines", n("1")),
                ]),
            ),
        ]),
        ResultShape::Grep => Value::object([
            ("mode", Value::str("content")),
            ("numFiles", n("0")),
            ("filenames", Value::Array(Vec::new())),
            ("content", Value::str(text)),
            ("numLines", n("1")),
        ]),
        ResultShape::Mcp => Value::Array(vec![Value::object([
            ("type", Value::str("text")),
            ("text", Value::str(text)),
        ])]),
    }
}

/// The harness's JSON for a decision; `None` for a pass. `file_path` is the
/// Read call's, for its replacement.
pub fn render(d: Decision, file_path: &str) -> Option<Value> {
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
        Decision::Replace(m, shape) => specific(
            HookKind::PostToolUse,
            [(
                "updatedToolOutput",
                replacement(shape, &hook::text(&m), file_path),
            )],
            vec![],
        ),
        Decision::Halt(m) => Value::Object(stop(&m)),
    };
    Some(v)
}

/// Print the decision in one write. A stop that cannot be printed exits
/// non-zero, so the harness still blocks.
fn emit(d: Decision, file_path: &str) -> ExitCode {
    let Some(v) = render(d, file_path) else {
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
                matches!(d, Decision::Replace(Msg::Withheld(_), ResultShape::Bash)),
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
    fn a_result_a_model_already_read_or_a_hit_in_the_batch_pauses_the_conversation() {
        let (m, dir) = machine("web");
        let web = payload(
            "PostToolUse",
            &format!(r#""tool_name":"WebFetch","tool_response":{{"result":"ssn {SSN}"}}"#),
        );
        let d = decide(HookKind::PostToolUse, &web, &m, b"inc-aaaaaa");
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
    fn t13_a_fork_of_a_paused_conversation_is_paused_once_its_transcript_exists() {
        let (m, dir) = machine("fork");
        let parent = "00893aaf-19fa-41d2-8238-13269b9b3ca0";
        let fork = "00893aaf-19fa-41d2-8238-13269b9b3ca1";
        let other = "00893aaf-19fa-41d2-8238-13269b9b3ca2";
        let flagged = "11111111-2222-4333-8444-555555555555";
        // The parent is paused by a real hit, and its newest message id noted.
        let parent_log = dir.join("parent.jsonl");
        std::fs::write(
            &parent_log,
            format!(r#"{{"type":"user","uuid":"{flagged}"}}"#),
        )
        .expect("write");
        let hit = format!(
            r#"{{"session_id":"{parent}","transcript_path":"{}","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"echo id {SSN}"}}}}"#,
            parent_log.display()
        );
        let conv = ConvId::parse(parent.as_bytes()).expect("conv");
        m.write_conv(
            &conv,
            &crate::state::ConvState {
                strikes: 1,
                ..crate::state::ConvState::default()
            },
        )
        .expect("seed");
        let d = decide(HookKind::PreToolUse, hit.as_bytes(), &m, b"inc-aaaaaa");
        assert!(matches!(d, Decision::DenyAndStop(Msg::Paused(..))), "{d:?}");
        // Two forks start; neither transcript exists yet, so both pass and stay pending.
        let fork_log = dir.join("fork.jsonl");
        let other_log = dir.join("other.jsonl");
        let ev = |sid: &str, event: &str, log: &std::path::Path, extra: &str| {
            format!(
                r#"{{"session_id":"{sid}","transcript_path":"{}","hook_event_name":"{event}"{extra}}}"#,
                log.display()
            )
        };
        for (sid, log) in [(fork, &fork_log), (other, &other_log)] {
            let start = ev(sid, "SessionStart", log, r#","source":"fork""#);
            assert_eq!(
                decide(HookKind::SessionStart, start.as_bytes(), &m, b"inc-bbbbbb"),
                Decision::Pass
            );
            let prompt = ev(sid, "UserPromptSubmit", log, r#","prompt":"hello""#);
            assert_eq!(
                decide(
                    HookKind::UserPromptSubmit,
                    prompt.as_bytes(),
                    &m,
                    b"inc-bbbbbb"
                ),
                Decision::Pass
            );
            assert!(m.fork_pending(&ConvId::parse(sid.as_bytes()).expect("conv")));
        }
        // Their transcripts appear: one carries the parent's message id.
        std::fs::write(&fork_log, format!(r#"{{"uuid":"{flagged}"}}"#)).expect("write");
        std::fs::write(
            &other_log,
            r#"{"uuid":"99999999-2222-4333-8444-555555555555"}"#,
        )
        .expect("write");
        let tool = |sid: &str, log: &std::path::Path| {
            ev(
                sid,
                "PreToolUse",
                log,
                r#","tool_name":"Bash","tool_input":{"command":"ls"}"#,
            )
        };
        let d = decide(
            HookKind::PreToolUse,
            tool(fork, &fork_log).as_bytes(),
            &m,
            b"inc-cccccc",
        );
        assert_eq!(
            d,
            Decision::DenyAndStop(Msg::StillPaused(inc(b"inc-cccccc")))
        );
        assert_eq!(
            decide(
                HookKind::PreToolUse,
                tool(other, &other_log).as_bytes(),
                &m,
                b"inc-dddddd"
            ),
            Decision::Pass
        );
        assert!(!m.fork_pending(&ConvId::parse(other.as_bytes()).expect("conv")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_grep_and_mcp_results_are_withheld_in_their_own_shape() {
        let (m, dir) = machine("shapes");
        let cases = [
            (
                "Read",
                format!(
                    r#"{{"type":"text","file":{{"filePath":"/r.txt","content":"ssn {SSN}","numLines":1,"startLine":1,"totalLines":1}}}}"#
                ),
                ResultShape::Read,
            ),
            (
                "Grep",
                format!(
                    r#"{{"mode":"content","numFiles":0,"filenames":[],"content":"r.txt:1:ssn {SSN}","numLines":1}}"#
                ),
                ResultShape::Grep,
            ),
            (
                "mcp__db__lookup",
                format!(r#"[{{"type":"text","text":"ssn {SSN}"}}]"#),
                ResultShape::Mcp,
            ),
        ];
        let batch = payload("PostToolBatch", r#""tool_calls":[]"#);
        for (n, (tool, response, shape)) in cases.into_iter().enumerate() {
            let p = payload(
                "PostToolUse",
                &format!(
                    r#""tool_name":"{tool}","tool_input":{{"file_path":"/r.txt"}},"tool_response":{response}"#
                ),
            );
            let d = decide(HookKind::PostToolUse, &p, &m, b"inc-aaaaaa");
            assert!(
                matches!(d, Decision::Replace(Msg::Withheld(_), s) if s == shape),
                "{tool}: {d:?}"
            );
            // They count toward the three-in-a-turn stop with Bash's.
            let stop = decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa");
            assert_eq!(
                stop == Decision::StopLoop(Msg::TurnStopped),
                n == 2,
                "{tool}"
            );
        }
        // A result heddle cannot replace still pauses.
        let glob = payload(
            "PostToolUse",
            &format!(r#""tool_name":"Glob","tool_response":{{"filenames":["/{SSN}"]}}"#),
        );
        assert!(matches!(
            decide(HookKind::PostToolUse, &glob, &m, b"inc-bbbbbb"),
            Decision::Halt(Msg::Paused(..))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn each_replacement_has_its_tools_output_shape() {
        let out = |shape| {
            render(Decision::Replace(Msg::Hiccup, shape), "/r.txt")
                .and_then(|v| {
                    v.get("hookSpecificOutput")
                        .and_then(|o| o.get("updatedToolOutput"))
                        .cloned()
                })
                .expect("a replacement")
        };
        let read = out(ResultShape::Read);
        assert_eq!(read.get("type").and_then(Value::as_str), Some("text"));
        let file = read.get("file").expect("file");
        assert_eq!(file.get("filePath").and_then(Value::as_str), Some("/r.txt"));
        assert_eq!(
            file.get("content").and_then(Value::as_str),
            Some(hook::text(&Msg::Hiccup).as_str())
        );
        let grep = out(ResultShape::Grep);
        assert_eq!(grep.get("mode").and_then(Value::as_str), Some("content"));
        assert_eq!(
            grep.get("filenames")
                .and_then(Value::as_array)
                .map(<[Value]>::len),
            Some(0)
        );
        let mcp = out(ResultShape::Mcp);
        let block = mcp.as_array().and_then(|a| a.first()).expect("one block");
        assert_eq!(block.get("type").and_then(Value::as_str), Some("text"));
        let bash = out(ResultShape::Bash);
        assert_eq!(bash.get("isImage").and_then(Value::as_bool), Some(false));
    }

    /// A payload for `event` whose transcript is `log`, plus `extra` members.
    fn at(event: &str, log: &Path, extra: &str) -> Vec<u8> {
        format!(
            r#"{{"session_id":"{CONV}","transcript_path":"{}","cwd":"{}","hook_event_name":"{event}","prompt_id":"p1"{}{extra}}}"#,
            log.display(),
            log.parent().expect("dir").display(),
            if extra.is_empty() { "" } else { "," }
        )
        .into_bytes()
    }

    fn append(log: &Path, line: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .expect("open");
        writeln!(f, "{line}").expect("write");
    }

    #[test]
    fn the_pending_set_catches_a_result_that_slipped_past_a_dead_batch_hook() {
        let (m, dir) = machine("pending");
        let log = dir.join("t.jsonl");
        append(
            &log,
            r#"{"type":"user","uuid":"a","parentUuid":null,"message":{"content":"hi"}}"#,
        );
        let conv = ConvId::parse(CONV.as_bytes()).expect("conv");
        let call = |id: &str| {
            at(
                "PreToolUse",
                &log,
                &format!(
                    r#""tool_name":"Bash","tool_use_id":"{id}","tool_input":{{"command":"ls"}}"#
                ),
            )
        };
        // A call that ran and was reported back leaves nothing behind.
        assert_eq!(
            decide(HookKind::PreToolUse, &call("t1"), &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        assert_eq!(m.pending(&conv).len(), 1);
        let batch = at(
            "PostToolBatch",
            &log,
            r#""tool_calls":[{"tool_use_id":"t1","tool_response":"fine"}]"#,
        );
        assert_eq!(
            decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        assert!(m.pending(&conv).is_empty());
        // A clean stale call is only dropped.
        assert_eq!(
            decide(HookKind::PreToolUse, &call("t2"), &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        append(
            &log,
            r#"{"type":"user","uuid":"b","message":{"content":[{"type":"tool_result","content":"fine"}]}}"#,
        );
        let prompt = at("UserPromptSubmit", &log, r#""prompt":"next""#);
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &prompt, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        assert!(m.pending(&conv).is_empty());
        // A result that landed while the batch hook was dead pauses at the next prompt.
        assert_eq!(
            decide(HookKind::PreToolUse, &call("t3"), &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        append(
            &log,
            &format!(
                r#"{{"type":"user","uuid":"c","message":{{"content":[{{"type":"tool_result","content":"id {SSN}"}}]}}}}"#
            ),
        );
        let d = decide(HookKind::UserPromptSubmit, &prompt, &m, b"inc-cccccc");
        assert_eq!(
            d,
            Decision::BlockPrompt {
                msg: Msg::StillPaused(inc(b"inc-cccccc")),
                suppress: false
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
        // A transcript that cannot be read is not proof of anything: it pauses too.
        let (m, dir) = machine("pending-gone");
        let log = dir.join("t.jsonl");
        append(&log, r#"{"type":"user","uuid":"a"}"#);
        let call = at(
            "PreToolUse",
            &log,
            r#""tool_name":"Bash","tool_use_id":"t9","tool_input":{"command":"ls"}"#,
        );
        assert_eq!(
            decide(HookKind::PreToolUse, &call, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        std::fs::remove_file(&log).expect("rm");
        // A call under another prompt: the earlier one is stale.
        let other_prompt = String::from_utf8(at(
            "PreToolUse",
            &log,
            r#""tool_name":"Bash","tool_use_id":"t10","tool_input":{"command":"ls"}"#,
        ))
        .expect("utf8")
        .replace(r#""prompt_id":"p1""#, r#""prompt_id":"p2""#);
        let d = decide(
            HookKind::PreToolUse,
            other_prompt.as_bytes(),
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
    fn a_session_that_keeps_no_transcript_is_not_paused_by_the_pending_set() {
        let (m, dir) = machine("no-transcript");
        let never = dir.join("never.jsonl");
        let call = at(
            "PreToolUse",
            &never,
            r#""tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"ls"}"#,
        );
        assert_eq!(
            decide(HookKind::PreToolUse, &call, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        let prompt = at("UserPromptSubmit", &never, r#""prompt":"next""#);
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &prompt, &m, b"inc-bbbbbb"),
            Decision::Pass
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_transcript_scan_and_at_files_only_count() {
        let (m, dir) = machine("scan");
        let log = dir.join("t.jsonl");
        append(
            &log,
            r#"{"type":"user","uuid":"a","message":{"content":"hi"}}"#,
        );
        let prompt = at("UserPromptSubmit", &log, r#""prompt":"hi""#);
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &prompt, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        // Text the hooks never saw lands in the transcript: it is counted, not braked.
        append(
            &log,
            &format!(
                r#"{{"type":"assistant","uuid":"b","message":{{"content":[{{"type":"text","text":"id {SSN}"}}]}}}}"#
            ),
        );
        let batch = at("PostToolBatch", &log, r#""tool_calls":[]"#);
        assert_eq!(
            decide(HookKind::PostToolBatch, &batch, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        // A file named with @ is read and counted too.
        std::fs::write(dir.join("record.txt"), format!("id {SSN}\n")).expect("write");
        let with_file = at(
            "UserPromptSubmit",
            &log,
            r#""prompt":"look at @record.txt please""#,
        );
        assert_eq!(
            decide(HookKind::UserPromptSubmit, &with_file, &m, b"inc-aaaaaa"),
            Decision::Pass
        );
        let log_lines = std::fs::read_to_string(dir.join("counts")).expect("counts");
        let seen: Vec<Vec<&str>> = log_lines
            .lines()
            .map(|l| l.split(' ').skip(1).collect())
            .collect();
        assert_eq!(
            seen,
            [
                ["scan", "PostToolBatch", "R-09"],
                ["scan", "UserPromptSubmit", "R-09"]
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_rewind_proof_lowers_a_pause_only_for_a_resent_prompt_off_the_flagged_turn() {
        for rewound in [true, false] {
            let (m, dir) = machine(if rewound { "rewind" } else { "no-rewind" });
            let log = dir.join("t.jsonl");
            append(
                &log,
                r#"{"type":"user","uuid":"a","parentUuid":null,"message":{"content":"one"}}"#,
            );
            append(
                &log,
                r#"{"type":"assistant","uuid":"b","parentUuid":"a","message":{"content":"ok"}}"#,
            );
            // The flagged turn: its prompt passes, then a result heddle cannot replace hits.
            let two = at("UserPromptSubmit", &log, r#""prompt":"two""#);
            assert_eq!(
                decide(HookKind::UserPromptSubmit, &two, &m, b"inc-aaaaaa"),
                Decision::Pass
            );
            append(
                &log,
                r#"{"type":"user","uuid":"c","parentUuid":"b","message":{"content":"two"}}"#,
            );
            let hit = at(
                "PostToolUse",
                &log,
                &format!(r#""tool_name":"Glob","tool_response":{{"filenames":["/{SSN}"]}}"#),
            );
            assert!(matches!(
                decide(HookKind::PostToolUse, &hit, &m, b"inc-bbbbbb"),
                Decision::Halt(Msg::Paused(..))
            ));
            append(
                &log,
                r#"{"type":"user","uuid":"c2","parentUuid":"c","message":{"content":[{"type":"tool_result","content":"hidden"}]}}"#,
            );
            // The person resends: after a rewind its parent is b, before the flagged turn.
            let parent = if rewound { "b" } else { "c2" };
            append(
                &log,
                &format!(
                    r#"{{"type":"user","uuid":"d","parentUuid":"{parent}","message":{{"content":"two, again"}}}}"#
                ),
            );
            let again = at("UserPromptSubmit", &log, r#""prompt":"two, again""#);
            let d = decide(HookKind::UserPromptSubmit, &again, &m, b"inc-cccccc");
            if rewound {
                assert_eq!(
                    d,
                    Decision::Pass,
                    "the proof holds: the conversation goes on"
                );
            } else {
                assert!(
                    matches!(
                        d,
                        Decision::BlockPrompt {
                            msg: Msg::StillPaused(_),
                            ..
                        }
                    ),
                    "{d:?}"
                );
            }
            let _ = std::fs::remove_dir_all(dir);
        }
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
        let r = |d| json::to_string(&render(d, "").expect("json"));
        assert_eq!(render(Decision::Pass, ""), None);
        let deny = render(Decision::Deny(Msg::Hiccup), "").expect("json");
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
        let stop = render(Decision::DenyAndStop(Msg::StillPaused(i)), "").expect("json");
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
        let replace = render(Decision::Replace(Msg::Hiccup, ResultShape::Bash), "").expect("json");
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
