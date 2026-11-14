//! Claude Code transcripts (JSONL), read for the brake: the scan of the bytes
//! added since an offset (the pending set and the count-only transcript
//! scan), and the rewind proof. Pure and total; heddle does the reading.
//!
//! A transcript is one JSON entry per line. Entries carry `uuid` and
//! `parentUuid`: the live conversation is the chain from the newest entry back
//! to the root. A fork or a resume keeps the ids; a rewind starts a new branch
//! from an earlier entry.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::detect;
use crate::json::{self, Value};
use crate::phi::RuleId;

/// One complete line: where it starts, its bytes, and its entry if it parses.
struct Line<'a> {
    offset: u64,
    raw: &'a [u8],
    entry: Option<Value>,
}

/// The complete lines of `bytes` (each ends in a newline), with offsets
/// counted from `base`, and how many bytes they cover. A partial last line,
/// still being written, is left for the next read.
fn lines(bytes: &[u8], base: u64) -> (Vec<Line<'_>>, usize) {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut used = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'\n' {
            continue;
        }
        let raw = bytes.get(start..i).unwrap_or_default();
        let offset = base.saturating_add(u64::try_from(start).unwrap_or(u64::MAX));
        if !raw.is_empty() {
            out.push(Line {
                offset,
                raw,
                entry: json::from_slice(raw).ok(),
            });
        }
        start = i.saturating_add(1);
        used = start;
    }
    (out, used)
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

/// A prompt a person typed: checked by UserPromptSubmit when it was sent.
pub fn typed_prompt(e: &Value) -> bool {
    str_of(e, "type") == Some("user")
        && !flag(e, "isMeta")
        && !flag(e, "isCompactSummary")
        && !flag(e, "isSidechain")
        && matches!(
            e.get("message").and_then(|m| m.get("content")),
            Some(Value::String(_))
        )
}

/// Entries the scan does not read. A held prompt waits in a queue entry
/// (scanning it would turn every Held into a pause); a typed prompt was
/// checked when it was typed; a message blob repeats a message the scan
/// already saw.
fn skipped(e: &Value) -> bool {
    matches!(
        str_of(e, "type"),
        Some("queue-operation" | "last-prompt" | "api-request-blob")
    ) || typed_prompt(e)
}

/// The scan of transcript bytes: each rule that fires on a complete line,
/// raw or unescaped, once, apart from the skipped entries. A line that does
/// not parse is scanned too. Returns the rules and how many bytes were read.
pub fn scan(bytes: &[u8]) -> (Vec<RuleId>, usize) {
    let (ls, used) = lines(bytes, 0);
    let mut rules: Vec<RuleId> = Vec::new();
    for l in ls.iter().filter(|l| !l.entry.as_ref().is_some_and(skipped)) {
        let unescaped = detect::unescape(l.raw);
        let raw = detect::detect(l.raw);
        let plain = detect::detect(&unescaped);
        for h in raw.hits.iter().chain(plain.hits.iter()).flatten() {
            if !rules.contains(&h.rule) {
                rules.push(h.rule);
            }
        }
    }
    (rules, used)
}

/// The prompt text of a user entry: a string, or the text blocks of an array.
fn prompt_text(e: &Value) -> Option<String> {
    match e.get("message")?.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let text: Vec<&str> = items
                .iter()
                .filter(|i| str_of(i, "type") == Some("text"))
                .filter_map(|i| str_of(i, "text"))
                .collect();
            (!text.is_empty()).then(|| text.concat())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

/// The newest user entry written at or after `from` whose prompt text is
/// `prompt`: the resent prompt, on disk. Its id.
pub fn resent_prompt(transcript: &[u8], prompt: &str, from: u64) -> Option<String> {
    let (ls, _) = lines(transcript, 0);
    ls.iter()
        .rev()
        .filter(|l| l.offset >= from)
        .filter_map(|l| l.entry.as_ref())
        .filter(|e| str_of(e, "type") == Some("user"))
        .find(|e| prompt_text(e).as_deref() == Some(prompt))
        .and_then(|e| str_of(e, "uuid").map(String::from))
}

/// The newest entry with an id written at or after `from`: what `heddle phi
/// recheck` walks from.
pub fn newest_entry(transcript: &[u8], from: u64) -> Option<String> {
    let (ls, _) = lines(transcript, 0);
    ls.iter()
        .rev()
        .filter(|l| l.offset >= from)
        .filter_map(|l| l.entry.as_ref())
        .find_map(|e| str_of(e, "uuid").map(String::from))
}

/// The rewind proof. `leaf` is the resent prompt, written after the pause.
/// It holds when every entry behind it on its `parentUuid` chain, up to the
/// root, was first written before `flagged_start`, the byte offset where the
/// flagged turn began. Then the flagged turn is off the live
/// chain, and no summary or compact boundary written after the hit is on it.
/// An id it cannot find, or a chain that does not end, is not proof.
pub fn chain_clear(transcript: &[u8], leaf: &str, flagged_start: u64) -> bool {
    let (ls, _) = lines(transcript, 0);
    // An id's first write: a later copy of the same entry adds nothing new.
    let mut index: BTreeMap<&str, (u64, Option<&str>)> = BTreeMap::new();
    for l in &ls {
        let Some(e) = &l.entry else {
            continue;
        };
        if let Some(id) = str_of(e, "uuid") {
            index
                .entry(id)
                .or_insert((l.offset, str_of(e, "parentUuid")));
        }
    }
    let Some((_, first)) = index.get(leaf) else {
        return false;
    };
    let mut next = *first;
    for _ in 0..=index.len() {
        let Some(at) = next else {
            return true;
        };
        let Some((offset, parent)) = index.get(at) else {
            return false;
        };
        if *offset >= flagged_start {
            return false;
        }
        next = *parent;
    }
    false
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use alloc::{format, vec};

    // Split across literals so heddle's own repository scan never sees it whole.
    const SSN: &str = concat!("987-12", "-3456");

    fn entry(ty: &str, id: &str, parent: Option<&str>, content: &str) -> String {
        let p = parent.map_or("null".into(), |p| format!("\"{p}\""));
        format!(
            r#"{{"type":"{ty}","uuid":"{id}","parentUuid":{p},"message":{{"content":{content}}}}}"#
        ) + "\n"
    }

    #[test]
    fn the_scan_reads_complete_lines_raw_and_unescaped_and_skips_what_was_checked() {
        let tool_result = entry(
            "user",
            "b",
            Some("a"),
            &format!(r#"[{{"type":"tool_result","content":"id {SSN}"}}]"#),
        );
        let typed = entry("user", "c", Some("b"), &format!(r#""typed {SSN}""#));
        let queued = format!("{{\"type\":\"queue-operation\",\"content\":\"held {SSN}\"}}\n");
        let blob = format!("{{\"type\":\"api-request-blob\",\"message\":\"{SSN}\"}}\n");
        // An HL7 segment shows only once its escaped carriage return is undone.
        let hl7 = entry(
            "user",
            "d",
            Some("c"),
            &format!(r#""MSH|^~\\&|A\rPID|1||{}^^^X""#, concat!("4455", "66")),
        );
        let (rules, used) = scan(tool_result.as_bytes());
        assert_eq!((rules, used), (vec![RuleId::R09], tool_result.len()));
        for skipped_line in [&typed, &queued, &blob] {
            assert_eq!(
                scan(skipped_line.as_bytes()).0,
                Vec::<RuleId>::new(),
                "{skipped_line}"
            );
        }
        // HL7 in a user string is a typed prompt; as a tool result it is read.
        let hl7_result = hl7
            .replace(
                r#""content":"MSH"#,
                r#""content":[{"type":"tool_result","content":"MSH"#,
            )
            .replace(r#"X""#, r#"X"}]"#);
        assert!(
            scan(hl7_result.as_bytes()).0.contains(&RuleId::R10),
            "{hl7_result}"
        );
        // A partial last line is left for the next read.
        let partial = format!("{tool_result}{{\"type\":\"user\",\"uuid\":\"e\"");
        assert_eq!(scan(partial.as_bytes()).1, tool_result.len());
    }

    #[test]
    fn the_rewind_proof_holds_only_when_the_chain_is_wholly_before_the_flagged_turn() {
        // a <- b <- c, where c is the flagged turn; after a rewind to b, d's parent is b.
        let a = entry("user", "a", None, r#""one""#);
        let b = entry("assistant", "b", Some("a"), r#""ok""#);
        let flagged_start = u64::try_from(a.len() + b.len()).unwrap();
        let c = entry("user", "c", Some("b"), r#""two""#);
        let c2 = entry("assistant", "c2", Some("c"), r#""flagged result""#);
        let paused_at = flagged_start + u64::try_from(c.len() + c2.len()).unwrap();
        let d = entry("user", "d", Some("b"), r#""two, again""#);
        let resent_on_old_chain = entry("user", "e", Some("c2"), r#""two, again""#);
        let rewound = format!("{a}{b}{c}{c2}{d}");
        let not_rewound = format!("{a}{b}{c}{c2}{resent_on_old_chain}");
        // The resent prompt is found only among entries written after the pause.
        let leaf = resent_prompt(rewound.as_bytes(), "two, again", paused_at).expect("found");
        assert_eq!(leaf, "d");
        assert!(chain_clear(rewound.as_bytes(), &leaf, flagged_start));
        let leaf = resent_prompt(not_rewound.as_bytes(), "two, again", paused_at).expect("found");
        assert!(!chain_clear(not_rewound.as_bytes(), &leaf, flagged_start));
        // Resending the flagged prompt's own text without a rewind finds nothing new.
        assert_eq!(resent_prompt(rewound.as_bytes(), "two", paused_at), None);
        // A summary written after the hit on the chain is not a rewind.
        let summary = entry("user", "s", Some("b"), r#""summary of two""#);
        let after_summary = entry("user", "f", Some("s"), r#""three""#);
        let summarized = format!("{a}{b}{c}{c2}{summary}{after_summary}");
        assert!(!chain_clear(summarized.as_bytes(), "f", flagged_start));
        // A later copy of an early entry does not move it after the hit.
        let copied = format!("{a}{b}{c}{c2}{b}{d}");
        assert!(chain_clear(copied.as_bytes(), "d", flagged_start));
        // An unknown id, a broken chain, or a cycle is not proof.
        assert!(!chain_clear(rewound.as_bytes(), "zz", flagged_start));
        let cycle = format!(
            "{}{}",
            entry("user", "x", Some("y"), r#""""#),
            entry("user", "y", Some("x"), r#""""#)
        );
        assert!(!chain_clear(cycle.as_bytes(), "x", u64::MAX));
        assert_eq!(
            newest_entry(rewound.as_bytes(), paused_at).as_deref(),
            Some("d")
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 7u64;
        for _ in 0..500 {
            let mut doc = Vec::new();
            for _ in 0..96 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let alphabet = b"{}[]\":,\\\nuuidparentUu 0123abc";
                doc.push(alphabet[usize::try_from(seed >> 59).unwrap() % alphabet.len()]);
            }
            let _ = scan(&doc);
            let _ = chain_clear(&doc, "a", 10);
            let _ = resent_prompt(&doc, "x", 0);
        }
    }
}
