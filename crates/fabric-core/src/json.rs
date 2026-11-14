//! A JSON reader and writer, modelled closely on `serde_json` (Xavier's
//! decision 6, 2026-11-14): its tokenizer's structure, its escape table, its
//! surrogate-pair handling, its number grammar and its depth limit of 128.
//! Two differences, both deliberate:
//!
//! - **No recursion.** Nesting is an explicit stack, so a deep document costs
//!   heap, never stack, and the limit is checked before each push.
//! - **Objects keep every entry, in document order.** [`Value::get`] returns
//!   the *last* entry with a key, as `serde_json` and JavaScript's
//!   `JSON.parse` do, but a scanner walking the value sees every string,
//!   duplicates included. A duplicate key cannot hide text from the brake.
//!
//! Numbers keep their source text; nothing here touches a float.
//!
//! The proof is a differential test: every string this reader decodes must
//! equal what Python's `json` module decodes, on the recorded payloads and on
//! fuzzed input (`spec/jsondiff/`).
//!
//! Portions adapted from serde_json (https://github.com/serde-rs/json):
//! Copyright (c) David Tolnay and the serde_json contributors, licensed under
//! either of the Apache License, Version 2.0 or the MIT license, at your
//! option. The MIT notice: Permission is hereby granted, free of charge, to
//! any person obtaining a copy of this software and associated documentation
//! files (the "Software"), to deal in the Software without restriction,
//! including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to the
//! following conditions: The above copyright notice and this permission notice
//! shall be included in all copies or substantial portions of the Software.
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.

use alloc::string::String;
use alloc::vec::Vec;

/// The deepest nesting accepted, as in `serde_json`.
pub const MAX_DEPTH: usize = 128;

/// A JSON value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` or `false`
    Bool(bool),
    /// A number, as its source text, which matches the JSON number grammar.
    Number(String),
    /// A string, decoded.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object: every entry, in document order, duplicates kept.
    Object(Vec<(String, Value)>),
}

/// Why a document was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input ended inside a value.
    Eof,
    /// A byte that cannot start or continue a value here.
    Syntax,
    /// A string with a raw control character, a bad escape, a lone
    /// surrogate or bytes that are not UTF-8.
    BadString,
    /// A number outside the JSON grammar.
    BadNumber,
    /// More than [`MAX_DEPTH`] levels.
    Depth,
    /// Something after the value.
    Trailing,
}

impl Value {
    /// The last entry with this key, if this is an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => None,
        }
    }
    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => None,
        }
    }
    /// The elements, if this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Object(_) => None,
        }
    }
    /// The entries, if this is an object.
    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(o) => Some(o),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => None,
        }
    }
    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Null
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_) => None,
        }
    }
    /// The number as a `u64`, if it is a non-negative integer that fits.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => n.parse::<u64>().ok(),
            Value::Null
            | Value::Bool(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_) => None,
        }
    }
    /// An object from entries, for the writer.
    pub fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Object(
            entries
                .into_iter()
                .map(|(k, v)| (String::from(k), v))
                .collect(),
        )
    }
    /// A string, for the writer.
    pub fn str(s: &str) -> Value {
        Value::String(String::from(s))
    }
    /// Every string in the value, keys excluded, in document order.
    pub fn strings(&self) -> Vec<&str> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![self];
        while let Some(v) = stack.pop() {
            match v {
                Value::String(s) => out.push(s.as_str()),
                Value::Array(a) => stack.extend(a.iter().rev()),
                Value::Object(o) => stack.extend(o.iter().rev().map(|(_, v)| v)),
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
            }
        }
        out
    }
}

// ---- the reader ----

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn bump(&mut self) {
        self.i = self.i.saturating_add(1);
    }
    fn next(&mut self) -> Result<u8, Error> {
        let c = self.peek().ok_or(Error::Eof)?;
        self.bump();
        Ok(c)
    }
    /// JSON whitespace only: space, tab, newline, carriage return.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }
    fn expect(&mut self, lit: &[u8]) -> Result<(), Error> {
        for c in lit {
            if self.next()? != *c {
                return Err(Error::Syntax);
            }
        }
        Ok(())
    }

    /// Four hex digits after `\u`.
    fn hex4(&mut self) -> Result<u32, Error> {
        let mut n = 0u32;
        for _ in 0..4 {
            let d = char::from(self.next().map_err(|_| Error::BadString)?)
                .to_digit(16)
                .ok_or(Error::BadString)?;
            n = n
                .checked_mul(16)
                .and_then(|x| x.checked_add(d))
                .ok_or(Error::BadString)?;
        }
        Ok(n)
    }

    /// A `\u` escape, with `serde_json`'s surrogate rules for a UTF-8
    /// string: a trailing surrogate alone, or a leading one not followed by
    /// `\u` and a trailing one, is refused.
    fn unicode_escape(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        let n = self.hex4()?;
        let code = if (0xD800..=0xDBFF).contains(&n) {
            if self.next().map_err(|_| Error::BadString)? != b'\\'
                || self.next().map_err(|_| Error::BadString)? != b'u'
            {
                return Err(Error::BadString);
            }
            let n2 = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&n2) {
                return Err(Error::BadString);
            }
            let hi = n.checked_sub(0xD800).ok_or(Error::BadString)?;
            let lo = n2.checked_sub(0xDC00).ok_or(Error::BadString)?;
            hi.checked_mul(0x400)
                .and_then(|x| x.checked_add(lo))
                .and_then(|x| x.checked_add(0x1_0000))
                .ok_or(Error::BadString)?
        } else if (0xDC00..=0xDFFF).contains(&n) {
            return Err(Error::BadString);
        } else {
            n
        };
        let c = char::from_u32(code).ok_or(Error::BadString)?;
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        Ok(())
    }

    /// A string, the opening quote already read.
    fn string(&mut self) -> Result<String, Error> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.next().map_err(|_| Error::BadString)?;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| Error::BadString),
                b'\\' => match self.next().map_err(|_| Error::BadString)? {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => self.unicode_escape(&mut out)?,
                    _ => return Err(Error::BadString),
                },
                0x00..=0x1F => return Err(Error::BadString),
                _ => out.push(c),
            }
        }
    }

    /// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`, kept as text.
    fn number(&mut self) -> Result<String, Error> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.bump();
        }
        match self.peek() {
            Some(b'0') => self.bump(),
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return Err(Error::BadNumber),
        }
        if self.peek() == Some(b'.') {
            self.bump();
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error::BadNumber);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Error::BadNumber);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        let text = self.b.get(start..self.i).ok_or(Error::BadNumber)?;
        String::from_utf8(text.to_vec()).map_err(|_| Error::BadNumber)
    }

    /// A key and its colon, the reader at the key's opening quote.
    fn key(&mut self) -> Result<String, Error> {
        self.skip_ws();
        if self.next()? != b'"' {
            return Err(Error::Syntax);
        }
        let k = self.string()?;
        self.skip_ws();
        if self.next()? != b':' {
            return Err(Error::Syntax);
        }
        Ok(k)
    }
}

enum Frame {
    Array(Vec<Value>),
    Object(Vec<(String, Value)>, String),
}

/// Read one JSON document from `input`. Total: refuses rather than panics.
pub fn from_slice(input: &[u8]) -> Result<Value, Error> {
    let mut r = Reader { b: input, i: 0 };
    let mut stack: Vec<Frame> = Vec::new();
    loop {
        // A value starts here.
        r.skip_ws();
        let mut v = match r.next()? {
            b'{' => {
                // As in serde_json, an empty container counts toward the depth too.
                if stack.len() >= MAX_DEPTH {
                    return Err(Error::Depth);
                }
                r.skip_ws();
                if r.peek() == Some(b'}') {
                    r.bump();
                    Value::Object(Vec::new())
                } else {
                    let k = r.key()?;
                    stack.push(Frame::Object(Vec::new(), k));
                    continue;
                }
            }
            b'[' => {
                if stack.len() >= MAX_DEPTH {
                    return Err(Error::Depth);
                }
                r.skip_ws();
                if r.peek() == Some(b']') {
                    r.bump();
                    Value::Array(Vec::new())
                } else {
                    stack.push(Frame::Array(Vec::new()));
                    continue;
                }
            }
            b'"' => Value::String(r.string()?),
            b't' => {
                r.expect(b"rue")?;
                Value::Bool(true)
            }
            b'f' => {
                r.expect(b"alse")?;
                Value::Bool(false)
            }
            b'n' => {
                r.expect(b"ull")?;
                Value::Null
            }
            b'-' | b'0'..=b'9' => {
                r.i = r.i.saturating_sub(1);
                Value::Number(r.number()?)
            }
            _ => return Err(Error::Syntax),
        };
        // The value is complete: attach it, closing every container it ends.
        loop {
            match stack.pop() {
                None => {
                    r.skip_ws();
                    return if r.peek().is_none() {
                        Ok(v)
                    } else {
                        Err(Error::Trailing)
                    };
                }
                Some(Frame::Array(mut items)) => {
                    items.push(v);
                    r.skip_ws();
                    match r.next()? {
                        b',' => {
                            stack.push(Frame::Array(items));
                            break;
                        }
                        b']' => v = Value::Array(items),
                        _ => return Err(Error::Syntax),
                    }
                }
                Some(Frame::Object(mut entries, key)) => {
                    entries.push((key, v));
                    r.skip_ws();
                    match r.next()? {
                        b',' => {
                            let k = r.key()?;
                            stack.push(Frame::Object(entries, k));
                            break;
                        }
                        b'}' => v = Value::Object(entries),
                        _ => return Err(Error::Syntax),
                    }
                }
            }
        }
    }
}

// ---- the writer ----

/// `serde_json`'s escapes: the quote, the backslash, the five short forms,
/// and `\u00XX` for every other control character. Everything else is
/// written as it is, UTF-8 included.
fn write_str(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{00}'..='\u{1f}' => {
                let b = u32::from(c);
                out.push_str("\\u00");
                for nib in [b.checked_shr(4).unwrap_or(0), b & 0xf] {
                    let i = usize::try_from(nib).unwrap_or(0);
                    out.push(char::from(*HEX.get(i).unwrap_or(&b'0')));
                }
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// The compact form, as `serde_json::to_string` writes it.
pub fn to_string(v: &Value) -> String {
    enum Step<'a> {
        Value(&'a Value),
        Text(&'static str),
        Key(&'a str),
    }
    let mut out = String::new();
    let mut stack = alloc::vec![Step::Value(v)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Text(t) => out.push_str(t),
            Step::Key(k) => {
                write_str(&mut out, k);
                out.push(':');
            }
            Step::Value(Value::Null) => out.push_str("null"),
            Step::Value(Value::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
            Step::Value(Value::Number(n)) => out.push_str(n),
            Step::Value(Value::String(s)) => write_str(&mut out, s),
            Step::Value(Value::Array(a)) => {
                out.push('[');
                stack.push(Step::Text("]"));
                for (i, x) in a.iter().enumerate().rev() {
                    stack.push(Step::Value(x));
                    if i > 0 {
                        stack.push(Step::Text(","));
                    }
                }
            }
            Step::Value(Value::Object(o)) => {
                out.push('{');
                stack.push(Step::Text("}"));
                for (i, (k, x)) in o.iter().enumerate().rev() {
                    stack.push(Step::Value(x));
                    stack.push(Step::Key(k));
                    if i > 0 {
                        stack.push(Step::Text(","));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn ok(s: &str) -> Value {
        from_slice(s.as_bytes()).expect("parses")
    }

    #[test]
    fn values_strings_and_escapes() {
        assert_eq!(ok(" null "), Value::Null);
        assert_eq!(
            ok("[true,false]"),
            Value::Array(alloc::vec![Value::Bool(true), Value::Bool(false)])
        );
        assert_eq!(
            ok(r#""a\"b\\c\/d\b\f\n\r\t""#),
            Value::str("a\"b\\c/d\u{8}\u{c}\n\r\t")
        );
        assert_eq!(ok(r#""\u00e9\u20AC\ud83d\ude00""#), Value::str("é€😀"));
        assert_eq!(ok("\"é😀\""), Value::str("é😀"));
        assert_eq!(ok("-0.5e+10").as_u64(), None);
        assert_eq!(
            ok("12345678901234567890").as_u64(),
            Some(12_345_678_901_234_567_890)
        );
    }

    #[test]
    fn refusals_match_the_json_grammar() {
        for bad in [
            "",
            " ",
            "nul",
            "tru",
            "[1,]",
            "{\"a\":1,}",
            "{\"a\" 1}",
            "{1:2}",
            "01",
            "-",
            "1.",
            "1e",
            ".5",
            "+1",
            "\"\\x\"",
            "\"\\u12\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\ud800\\u0041\"",
            "\"a\nb\"",
            "\"\u{1}\"",
            "[1 2]",
            "{} {}",
            "NaN",
            "Infinity",
            "\u{feff}{}",
            "'a'",
            "[1}",
            "{\"a\":1]",
        ] {
            assert!(
                from_slice(bad.as_bytes()).is_err(),
                "accepted: {:?}",
                bad.as_bytes()
            );
        }
        assert_eq!(from_slice(b"\"\xff\""), Err(Error::BadString));
    }

    #[test]
    fn depth_is_capped_without_recursion() {
        let deep_ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(from_slice(deep_ok.as_bytes()).is_ok());
        let too_deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(from_slice(too_deep.as_bytes()), Err(Error::Depth));
        // A million opening brackets is refused, not a stack overflow.
        assert_eq!(
            from_slice("[".repeat(1_000_000).as_bytes()),
            Err(Error::Depth)
        );
    }

    #[test]
    fn get_takes_the_last_duplicate_and_strings_see_every_one() {
        let v = ok(r#"{"prompt":"first","x":[1,{"y":"deep"}],"prompt":"second"}"#);
        assert_eq!(v.get("prompt").and_then(Value::as_str), Some("second"));
        assert_eq!(v.strings(), ["first", "deep", "second"]);
        assert_eq!(v.get("missing"), None);
    }

    #[test]
    fn the_writer_round_trips_and_escapes_like_serde_json() {
        let v = Value::object([
            ("decision", Value::str("block")),
            (
                "reason",
                Value::str("a \"quote\", a \\, a\nnewline, a \u{1} and é"),
            ),
            (
                "n",
                Value::Array(alloc::vec![
                    Value::Null,
                    Value::Bool(false),
                    Value::Number("12".to_string())
                ]),
            ),
        ]);
        let s = to_string(&v);
        assert_eq!(
            s,
            r#"{"decision":"block","reason":"a \"quote\", a \\, a\nnewline, a \u0001 and é","n":[null,false,12]}"#
        );
        assert_eq!(from_slice(s.as_bytes()), Ok(v));
    }

    #[test]
    fn the_recorded_payloads_parse() {
        for p in [
            include_str!("../../../adapters/claude-code/payloads/PreToolUse.json"),
            include_str!("../../../adapters/claude-code/payloads/PostToolUse.json"),
            include_str!("../../../adapters/claude-code/payloads/PostToolBatch.json"),
            include_str!("../../../adapters/claude-code/payloads/UserPromptSubmit.json"),
            include_str!("../../../adapters/claude-code/payloads/SessionStart-fork.json"),
        ] {
            let v = ok(p);
            assert!(v.get("session_id").and_then(Value::as_str).is_some());
            assert!(v.get("hook_event_name").and_then(Value::as_str).is_some());
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 3u64;
        for _ in 0..2_000 {
            let mut doc = Vec::new();
            for _ in 0..64 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let pick = b"{}[]\":,\\u0123456789abcdefnrtl -.eE \x01\xff";
                doc.push(pick[usize::try_from(seed >> 59).unwrap_or(0) % pick.len()]);
            }
            let _ = from_slice(&doc);
        }
    }
}
