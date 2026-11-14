//! Test T20, the false-alarm count: fabric-core's detector over transcripts
//! and files. It prints rule ids and where they fired, never the matched text.
//!
//!   falsealarms hooks FILE...   Claude Code transcripts (JSONL): what the hook
//!                               lines would have scanned. One line per hit:
//!                               DATE SESSION SURFACE TOOL RULE, where a
//!                               file tool reads TOOL:PATH
//!   falsealarms raw FILE...     the raw-byte scan, count only: every line, raw
//!                               and unescaped, except typed prompts (checked
//!                               when typed) and queue entries. One line per hit:
//!                               DATE SESSION TYPE RULE
//!   falsealarms files FILE...   whole files, as the dry scan reads them. One
//!                               line per hit: RULE PATH
//!
//! Counts of what was read go to standard error.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::exit
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};

use fabric_core::detect;
use fabric_core::hook::rule_name;
use fabric_core::json::{self, Value};

fn rules(text: &[u8]) -> BTreeSet<&'static str> {
    detect::detect(text)
        .hits
        .iter()
        .flatten()
        .map(|h| rule_name(h.rule))
        .collect()
}

/// What a hook scans in a value: its JSON text, then each string raw.
fn hook_text(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    if !matches!(v, Value::String(_)) {
        out.extend_from_slice(json::to_string(v).as_bytes());
        out.push(b'\n');
    }
    for s in v.strings() {
        out.extend_from_slice(s.as_bytes());
        out.push(b'\n');
    }
    out
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

fn date(e: &Value) -> &str {
    let t = str_of(e, "timestamp");
    t.get(..10).filter(|d| !d.is_empty()).unwrap_or("undated")
}

/// A typed prompt: checked by UserPromptSubmit when it was sent.
fn typed_prompt(e: &Value) -> bool {
    str_of(e, "type") == "user"
        && !flag(e, "isMeta")
        && !flag(e, "isCompactSummary")
        && !flag(e, "isSidechain")
        && matches!(
            e.get("message").and_then(|m| m.get("content")),
            Some(Value::String(_))
        )
}

fn lines(path: &str, mut each: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        each(buf.strip_suffix(b"\n").unwrap_or(&buf));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, paths) = args
        .split_first()
        .expect("usage: falsealarms hooks|raw|files FILE...");
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut read: BTreeMap<&str, u64> = BTreeMap::new();
    for path in paths {
        match mode.as_str() {
            "hooks" => {
                let mut tools: BTreeMap<String, String> = BTreeMap::new();
                lines(path, |line| {
                    let Ok(e) = json::from_slice(line) else {
                        *read.entry("unparsed lines").or_default() += 1;
                        return;
                    };
                    *read.entry("entries").or_default() += 1;
                    let (d, session) = (date(&e), str_of(&e, "sessionId"));
                    let mut hit = |surface: &'static str, tool: &str, v: &Value| {
                        *read
                            .entry(if surface == "prompt" {
                                "prompts"
                            } else {
                                surface
                            })
                            .or_default() += 1;
                        for r in rules(&hook_text(v)) {
                            writeln!(out, "{d}\t{session}\t{surface}\t{tool}\t{r}").unwrap();
                        }
                    };
                    let content = e.get("message").and_then(|m| m.get("content"));
                    match (str_of(&e, "type"), content) {
                        ("user", Some(c @ Value::String(_))) if typed_prompt(&e) => {
                            hit("prompt", "-", c)
                        }
                        ("user", Some(Value::Array(items))) if !flag(&e, "isMeta") => {
                            for item in items {
                                match str_of(item, "type") {
                                    "tool_result" => {
                                        let tool = tools
                                            .get(str_of(item, "tool_use_id"))
                                            .map_or("?", String::as_str);
                                        if let Some(c) = item.get("content") {
                                            hit("result", tool, c);
                                        }
                                    }
                                    "text" if !flag(&e, "isSidechain") => {
                                        if let Some(t) = item.get("text") {
                                            hit("prompt", "-", t);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ("assistant", Some(Value::Array(items))) => {
                            for item in items.iter().filter(|i| str_of(i, "type") == "tool_use") {
                                // A file tool is named with its path, as the pause message names it.
                                let name = match item.get("input").map(|i| str_of(i, "file_path")) {
                                    Some(path) if !path.is_empty() => {
                                        format!("{}:{path}", str_of(item, "name"))
                                    }
                                    Some(_) | None => str_of(item, "name").to_string(),
                                };
                                if let Some(input) = item.get("input") {
                                    hit("input", &name, input);
                                }
                                tools.insert(str_of(item, "id").to_string(), name);
                            }
                        }
                        _ => {}
                    }
                })
                .expect("a readable file");
            }
            "raw" => lines(path, |line| {
                let e = json::from_slice(line).ok();
                let ty = e.as_ref().map_or("?", |e| str_of(e, "type"));
                if matches!(ty, "queue-operation" | "last-prompt")
                    || e.as_ref().is_some_and(typed_prompt)
                {
                    *read.entry("skipped lines").or_default() += 1;
                    return;
                }
                *read.entry("lines").or_default() += 1;
                *read.entry("bytes").or_default() += u64::try_from(line.len()).unwrap_or(u64::MAX);
                let (d, session) = e
                    .as_ref()
                    .map_or(("undated", "?"), |e| (date(e), str_of(e, "sessionId")));
                let mut found = rules(line);
                found.extend(rules(&detect::unescape(line)));
                for r in found {
                    writeln!(out, "{d}\t{session}\t{ty}\t{r}").unwrap();
                }
            })
            .expect("a readable file"),
            "files" => {
                let Ok(bytes) = std::fs::read(path) else {
                    *read.entry("unreadable files").or_default() += 1;
                    continue;
                };
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    *read.entry("binary files skipped").or_default() += 1;
                    continue;
                }
                *read.entry("files").or_default() += 1;
                for r in rules(&bytes) {
                    writeln!(out, "{r}\t{path}").unwrap();
                }
            }
            other => {
                eprintln!("unknown mode {other}");
                std::process::exit(2);
            }
        }
    }
    out.flush().unwrap();
    for (k, n) in read {
        eprintln!("{k}: {n}");
    }
}
