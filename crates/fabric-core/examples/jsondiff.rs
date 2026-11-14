//! The Rust side of the JSON differential test (see `spec/jsondiff/`): read
//! hex lines from standard input, parse each with fabric-core's reader, and
//! print ERR or the canonical token line Python's side also prints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::io::{BufRead, Write};

use fabric_core::json::{self, Value};

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(line: &str) -> Option<Vec<u8>> {
    let b = line.trim().as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    b.chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

fn canon(v: &Value) -> String {
    enum Step<'a> {
        V(&'a Value),
        T(&'static str),
        K(&'a str),
    }
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![Step::V(v)];
    while let Some(s) = stack.pop() {
        match s {
            Step::T(t) => out.push(t.to_string()),
            Step::K(k) => out.push(format!("k{}", hex(k))),
            Step::V(Value::Null) => out.push("z".into()),
            Step::V(Value::Bool(true)) => out.push("t".into()),
            Step::V(Value::Bool(false)) => out.push("f".into()),
            Step::V(Value::Number(n)) => out.push(format!("n{n}")),
            Step::V(Value::String(x)) => out.push(format!("s{}", hex(x))),
            Step::V(Value::Array(a)) => {
                out.push("[".into());
                stack.push(Step::T("]"));
                stack.extend(a.iter().rev().map(Step::V));
            }
            Step::V(Value::Object(o)) => {
                out.push("{".into());
                stack.push(Step::T("}"));
                for (k, x) in o.iter().rev() {
                    stack.push(Step::V(x));
                    stack.push(Step::K(k));
                }
            }
        }
    }
    out.join(" ")
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line.expect("a line");
        let shown = match unhex(&line).map(|b| json::from_slice(&b)) {
            Some(Ok(v)) => canon(&v),
            Some(Err(_)) | None => "ERR".to_string(),
        };
        writeln!(out, "{shown}").expect("stdout");
    }
}
