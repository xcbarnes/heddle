//! The stage-1 PHI pre-filter (phi.md §6): fixed rules as one pure, total,
//! linear-time function over bytes.
//!
//! Every decision is made at one anchor position `p` and reads only the window
//! `[p - LEFT, p + RIGHT)`. The streaming form decides each position once,
//! with exactly the bytes a whole-text scan would use. So folding
//! [`detect_step`] over any cutting of a text into chunks, then
//! [`detect_finish`], gives the same verdict as [`detect`] on the whole.
//!
//! A verdict holds rule ids and offsets, never the matched text. Four rules are
//! decided at the edge, not here: R-00 (the detector did not run), R-06 (a
//! restricted path), R-07 (too large, or undecodable) and R-08 (a content type
//! the rules cannot read). Exemptions are by value, never by folder.

use alloc::vec::Vec;

use crate::phi::RuleId;

/// How far back a decision may read.
pub const LEFT: usize = 512;
/// How far ahead a decision may read: the longest rule window.
pub const RIGHT: usize = 2048;
/// "Near": within this many bytes before or after.
const NEAR: usize = 40;
/// Hits kept in a verdict; a ninth sets `more`.
pub const MAX_HITS: usize = 8;

/// One hit: a rule and where. Never the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The rule.
    pub rule: RuleId,
    /// The byte offset of the anchor, saturating at `u32::MAX`.
    pub offset: u32,
}

/// The pre-filter's answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// The first hits, in offset order.
    pub hits: [Option<Hit>; MAX_HITS],
    /// More hits than fit. The verdict still blocks.
    pub more: bool,
}

impl Verdict {
    /// A verdict with one hit at offset 0: for the rules the edge decides.
    pub fn of(rule: RuleId) -> Verdict {
        let mut v = Verdict::default();
        v.push(Hit { rule, offset: 0 });
        v
    }
    /// Any hit at all.
    pub fn fired(&self) -> bool {
        self.more || self.hits.iter().any(Option::is_some)
    }
    /// The first hit.
    pub fn first(&self) -> Option<Hit> {
        self.hits.iter().flatten().next().copied()
    }
    fn push(&mut self, h: Hit) {
        match self.hits.iter_mut().find(|s| s.is_none()) {
            Some(slot) => *slot = Some(h),
            None => self.more = true,
        }
    }
}

// ---- byte helpers: every read is `get`, every step checked ----

fn at(t: &[u8], i: usize) -> Option<u8> {
    t.get(i).copied()
}
fn at_off(t: &[u8], i: usize, k: usize) -> Option<u8> {
    i.checked_add(k).and_then(|j| at(t, j))
}
fn before(t: &[u8], i: usize) -> Option<u8> {
    i.checked_sub(1).and_then(|j| at(t, j))
}
fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn alnum_at(t: &[u8], i: usize) -> bool {
    at(t, i).is_some_and(is_alnum)
}
fn digit_at(t: &[u8], i: usize) -> Option<u8> {
    at(t, i).filter(u8::is_ascii_digit).and_then(|b| b.checked_sub(b'0'))
}
fn word_start(t: &[u8], i: usize) -> bool {
    !before(t, i).is_some_and(is_alnum)
}
fn line_start(t: &[u8], i: usize) -> bool {
    before(t, i).is_none_or(|b| b == b'\n' || b == b'\r')
}
/// Case-insensitive `pat` (lowercase) at `i`.
fn starts_ci(t: &[u8], i: usize, pat: &[u8]) -> bool {
    pat.iter().enumerate().all(|(k, c)| at_off(t, i, k).map(|b| b.to_ascii_lowercase()) == Some(*c))
}

/// N digits at `i`, as values.
fn digits<const N: usize>(t: &[u8], i: usize) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    for (k, slot) in out.iter_mut().enumerate() {
        *slot = digit_at(t, i.checked_add(k)?)?;
    }
    Some(out)
}

/// One or two digits at `i`: the value and the end.
fn num12(t: &[u8], i: usize) -> Option<(u8, usize)> {
    let d1 = digit_at(t, i)?;
    let j = i.checked_add(1)?;
    match digit_at(t, j) {
        Some(d2) => {
            if digit_at(t, j.checked_add(1)?).is_some() {
                return None;
            }
            Some((d1.checked_mul(10)?.checked_add(d2)?, j.checked_add(1)?))
        }
        None => Some((d1, j)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sep {
    Dash,
    Space,
    Dot,
}

/// A separator between digits at `i`: dashes, dots, spaces and their Unicode
/// look-alikes all count as one separator. Its kind and length in bytes.
fn sep_at(t: &[u8], i: usize) -> Option<(Sep, usize)> {
    let (b1, b2) = (at_off(t, i, 1), at_off(t, i, 2));
    match at(t, i)? {
        b'-' => Some((Sep::Dash, 1)),
        b' ' => Some((Sep::Space, 1)),
        b'.' => Some((Sep::Dot, 1)),
        0xC2 if b1 == Some(0xA0) => Some((Sep::Space, 2)),
        0xE2 => match (b1, b2) {
            (Some(0x80), Some(0x90..=0x95)) | (Some(0x88), Some(0x92)) => Some((Sep::Dash, 3)),
            (Some(0x80), Some(0x89 | 0xAF)) => Some((Sep::Space, 3)),
            (Some(0x80), Some(0xA4)) => Some((Sep::Dot, 3)),
            _ => None,
        },
        0xEF => match (b1, b2) {
            (Some(0xB9), Some(0xA3)) | (Some(0xBC), Some(0x8D)) => Some((Sep::Dash, 3)),
            (Some(0xBC), Some(0x8E)) => Some((Sep::Dot, 3)),
            _ => None,
        },
        _ => None,
    }
}

/// A keyword at `i`, case-insensitive and word-bounded. A space in `kw`
/// matches one or more of space, tab, `_` and `-`. Returns its end.
fn keyword_at(t: &[u8], i: usize, kw: &[u8]) -> Option<usize> {
    if !word_start(t, i) {
        return None;
    }
    let mut j = i;
    for c in kw {
        if *c == b' ' {
            let start = j;
            while at(t, j).is_some_and(|b| matches!(b, b' ' | b'\t' | b'_' | b'-')) {
                j = j.checked_add(1)?;
            }
            if j == start {
                return None;
            }
        } else {
            if at(t, j)?.to_ascii_lowercase() != *c {
                return None;
            }
            j = j.checked_add(1)?;
        }
    }
    if alnum_at(t, j) { None } else { Some(j) }
}

/// Whether any keyword ends within `NEAR` bytes before `start` or begins
/// within `NEAR` bytes after `end`.
fn keyword_near(t: &[u8], start: usize, end: usize, kws: &[&[u8]]) -> bool {
    let longest = kws.iter().map(|k| k.len()).max().unwrap_or(0);
    let lo = start.saturating_sub(NEAR.saturating_add(longest).saturating_add(8));
    let hi = end.saturating_add(NEAR).min(t.len());
    (lo..hi).any(|k| {
        kws.iter().any(|kw| {
            keyword_at(t, k, kw).is_some_and(|ke| ke.saturating_add(NEAR) >= start && k <= end.saturating_add(NEAR))
        })
    })
}

/// Up to four separator bytes after a label.
fn skip_label_seps(t: &[u8], i: usize) -> usize {
    let mut j = i;
    let limit = i.saturating_add(4);
    while j < limit && at(t, j).is_some_and(|b| matches!(b, b' ' | b'\t' | b':' | b'#' | b'=' | b'-' | b'.' | b',' | b';')) {
        j = j.saturating_add(1);
    }
    j
}

// ---- R-01, R-09: Social Security numbers ----

struct Ssn {
    end: usize,
    dashed: bool,
    reserved: bool,
}

/// `ddd-dd-dddd`, `ddd dd dddd`, `ddddddddd` (any separators, both or none),
/// not part of a longer number.
fn ssn_at(t: &[u8], p: usize) -> Option<Ssn> {
    if before(t, p).is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    let area = digits::<3>(t, p)?;
    let i = p.checked_add(3)?;
    let s1 = sep_at(t, i);
    let i = i.checked_add(s1.map_or(0, |s| s.1))?;
    let group = digits::<2>(t, i)?;
    let i = i.checked_add(2)?;
    let s2 = sep_at(t, i);
    if s1.is_some() != s2.is_some() {
        return None;
    }
    let i = i.checked_add(s2.map_or(0, |s| s.1))?;
    let serial = digits::<4>(t, i)?;
    let end = i.checked_add(4)?;
    if digit_at(t, end).is_some() {
        return None;
    }
    let dashed = s1.is_some_and(|s| s.0 == Sep::Dash) && s2.is_some_and(|s| s.0 == Sep::Dash);
    // The reserved synthetic forms: area 000 or 666, group 00, serial 0000.
    let reserved = area == [0, 0, 0] || area == [6, 6, 6] || group == [0, 0] || serial == [0, 0, 0, 0];
    Some(Ssn { end, dashed, reserved })
}

const SSN_WORDS: &[&[u8]] = &[b"ssn", b"social security"];

fn rule_ssn(t: &[u8], p: usize) -> Option<RuleId> {
    let s = ssn_at(t, p)?;
    if s.reserved {
        return None;
    }
    if keyword_near(t, p, s.end, SSN_WORDS) {
        Some(RuleId::R01)
    } else if s.dashed {
        Some(RuleId::R09)
    } else {
        None
    }
}

// ---- R-02: Medicare beneficiary identifiers ----

fn mbi_letter(b: u8) -> bool {
    let u = b.to_ascii_uppercase();
    u.is_ascii_uppercase() && !b"SLOIBZ".contains(&u)
}

/// The 11-character MBI shape, with the printed form's optional dashes after
/// the fourth and seventh characters.
fn mbi_at(t: &[u8], p: usize) -> Option<usize> {
    #[derive(Clone, Copy)]
    enum K {
        NonZero,
        Letter,
        Either,
        Digit,
    }
    const SHAPE: [K; 11] =
        [K::NonZero, K::Letter, K::Either, K::Digit, K::Letter, K::Either, K::Digit, K::Letter, K::Letter, K::Digit, K::Digit];
    if !word_start(t, p) {
        return None;
    }
    let mut j = p;
    for (n, k) in SHAPE.iter().enumerate() {
        if (n == 4 || n == 7) && at(t, j) == Some(b'-') {
            j = j.checked_add(1)?;
        }
        let b = at(t, j)?;
        let ok = match k {
            K::NonZero => (b'1'..=b'9').contains(&b),
            K::Letter => mbi_letter(b),
            K::Either => b.is_ascii_digit() || mbi_letter(b),
            K::Digit => b.is_ascii_digit(),
        };
        if !ok {
            return None;
        }
        j = j.checked_add(1)?;
    }
    if alnum_at(t, j) { None } else { Some(j) }
}

fn rule_mbi(t: &[u8], p: usize) -> Option<RuleId> {
    let end = mbi_at(t, p)?;
    keyword_near(t, p, end, &[b"medicare", b"mbi"]).then_some(RuleId::R02)
}

// ---- R-03: record numbers after their label ----

const MRN_WORDS: &[&[u8]] = &[b"mrn", b"medical record number", b"medical record no", b"patient id"];

fn rule_mrn(t: &[u8], p: usize) -> Option<RuleId> {
    let end = MRN_WORDS.iter().find_map(|kw| keyword_at(t, p, kw))?;
    let v = skip_label_seps(t, end);
    if starts_ci(t, v, b"syn-") {
        return None;
    }
    let mut n = 0usize;
    while alnum_at(t, v.checked_add(n)?) {
        n = n.checked_add(1)?;
        if n > 12 {
            return None;
        }
    }
    (n >= 6).then_some(RuleId::R03)
}

// ---- R-04: a date of birth ----

const MONTHS: &[&[u8]] = &[
    b"january", b"february", b"march", b"april", b"may", b"june", b"july", b"august", b"september", b"october",
    b"november", b"december", b"jan", b"feb", b"mar", b"apr", b"jun", b"jul", b"aug", b"sept", b"sep", b"oct",
    b"nov", b"dec",
];

fn date_sep(t: &[u8], i: usize) -> Option<u8> {
    at(t, i).filter(|b| matches!(b, b'-' | b'/' | b'.'))
}

/// `yyyy-mm-dd`, `m/d/yyyy`, or a month name, a day and a year.
fn date_at(t: &[u8], i: usize) -> bool {
    let iso = || -> Option<()> {
        digits::<4>(t, i)?;
        let s = date_sep(t, i.checked_add(4)?)?;
        let (m, j) = num12(t, i.checked_add(5)?)?;
        if at(t, j)? != s {
            return None;
        }
        let (d, _) = num12(t, j.checked_add(1)?)?;
        ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some(())
    };
    let us = || -> Option<()> {
        let (m, j) = num12(t, i)?;
        let s = date_sep(t, j)?;
        let (d, k) = num12(t, j.checked_add(1)?)?;
        if at(t, k)? != s {
            return None;
        }
        let y = k.checked_add(1)?;
        digits::<4>(t, y)?;
        if digit_at(t, y.checked_add(4)?).is_some() {
            return None;
        }
        ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some(())
    };
    let named = || -> Option<()> {
        let mut j = MONTHS.iter().find_map(|m| keyword_at(t, i, m))?;
        if at(t, j) == Some(b'.') {
            j = j.checked_add(1)?;
        }
        let k = j;
        while at(t, j) == Some(b' ') {
            j = j.checked_add(1)?;
        }
        if j == k {
            return None;
        }
        let (d, mut j) = num12(t, j)?;
        for suffix in [b"st", b"nd", b"rd", b"th"] {
            if starts_ci(t, j, suffix) {
                j = j.checked_add(2)?;
            }
        }
        if at(t, j) == Some(b',') {
            j = j.checked_add(1)?;
        }
        while at(t, j) == Some(b' ') {
            j = j.checked_add(1)?;
        }
        digits::<4>(t, j)?;
        if digit_at(t, j.checked_add(4)?).is_some() {
            return None;
        }
        (1..=31).contains(&d).then_some(())
    };
    iso().is_some() || us().is_some() || named().is_some()
}

const DOB_WORDS: &[&[u8]] = &[b"dob", b"date of birth", b"birthdate", b"birth date"];

fn rule_dob(t: &[u8], p: usize) -> Option<RuleId> {
    let end = DOB_WORDS.iter().find_map(|kw| keyword_at(t, p, kw))?;
    date_at(t, skip_label_seps(t, end)).then_some(RuleId::R04)
}

// ---- R-05: data-shaped text with direct-identifier field names ----

/// Direct-identifier field names, compared with every byte but letters and
/// digits removed, in lowercase.
const ID_FIELDS: &[&[u8]] = &[
    b"firstname", b"lastname", b"givenname", b"familyname", b"fullname", b"patientname", b"dob", b"dateofbirth",
    b"birthdate", b"ssn", b"socialsecuritynumber", b"mrn", b"medicalrecordnumber", b"memberid", b"subscriberid",
    b"address", b"streetaddress", b"address1", b"addressline1", b"phone", b"phonenumber", b"telephone",
];

/// The field name in `f`, compacted: letters and digits only, lowercase.
/// `None` unless `f` is a plain name (quotes and spaces around it allowed).
fn compact(f: &[u8]) -> Option<([u8; 32], usize)> {
    let mut out = [0u8; 32];
    let mut n = 0usize;
    for b in f {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {
                *out.get_mut(n)? = b.to_ascii_lowercase();
                n = n.checked_add(1)?;
            }
            b'_' | b' ' | b'-' | b'.' | b'"' | b'\'' | b'\t' => {}
            _ => return None,
        }
    }
    (n > 0).then_some((out, n))
}

fn is_id_field(name: &[u8]) -> bool {
    ID_FIELDS.contains(&name)
}

/// A delimited header row at a line start with two or more identifier
/// fields, unless it has a `synthetic` column or the next line has a `SYN-`
/// value.
fn rule_header(t: &[u8], p: usize) -> Option<RuleId> {
    if !line_start(t, p) {
        return None;
    }
    let rest = t.get(p..)?;
    let line_len = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
    let line = rest.get(..line_len)?;
    let delims = line.iter().filter(|b| matches!(b, b',' | b'\t' | b'|' | b';')).count();
    if delims < 2 {
        return None;
    }
    let mut found: Vec<([u8; 32], usize)> = Vec::new();
    let mut synthetic = false;
    for field in line.split(|b| matches!(b, b',' | b'\t' | b'|' | b';')) {
        let trimmed = field.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        let (name, n) = compact(trimmed)?;
        let name_bytes = name.get(..n)?;
        if name_bytes == b"synthetic" || name_bytes == b"issynthetic" {
            synthetic = true;
        }
        if is_id_field(name_bytes) && !found.iter().any(|(f, m)| f.get(..*m) == Some(name_bytes)) {
            found.push((name, n));
        }
    }
    let next_line = rest.get(line_len..).unwrap_or(&[]);
    let next_line = next_line.get(1..).unwrap_or(&[]);
    let next_len = next_line.iter().position(|b| *b == b'\n').unwrap_or(next_line.len());
    let syn_value = next_line.get(..next_len).is_some_and(|l| l.has_syn());
    (found.len() >= 2 && !synthetic && !syn_value).then_some(RuleId::R05)
}

/// `"name"` followed by optional spaces and `:`: a JSON key. Its compact
/// name and the index after the colon.
fn json_key_at(t: &[u8], p: usize) -> Option<(([u8; 32], usize), usize)> {
    if at(t, p)? != b'"' {
        return None;
    }
    let body = t.get(p.checked_add(1)?..)?;
    let len = body.iter().take(64).position(|b| *b == b'"')?;
    let key = body.get(..len)?;
    let mut j = p.checked_add(len)?.checked_add(2)?;
    while matches!(at(t, j), Some(b' ' | b'\t')) {
        j = j.checked_add(1)?;
    }
    if at(t, j)? != b':' {
        return None;
    }
    Some((compact(key)?, j.checked_add(1)?))
}

fn id_key_at(t: &[u8], p: usize) -> Option<usize> {
    let ((name, n), after) = json_key_at(t, p)?;
    is_id_field(name.get(..n)?).then_some(after)
}

/// The first identifier key of a JSON object with two or more, unless the
/// object also has `"synthetic": true` or a `"SYN-` value nearby.
fn rule_json_keys(t: &[u8], p: usize) -> Option<RuleId> {
    let after = id_key_at(t, p)?;
    const SPAN: usize = 512;
    // Report the object once: only at its first identifier key.
    let back = p.saturating_sub(SPAN);
    if (back..p).any(|k| id_key_at(t, k).is_some()) {
        return None;
    }
    let ahead = after.saturating_add(SPAN).min(t.len());
    let first = json_key_at(t, p).map(|(k, _)| k)?;
    let another = (after..ahead).any(|k| {
        json_key_at(t, k).is_some_and(|((name, n), _)| {
            name.get(..n).is_some_and(is_id_field) && name.get(..n) != first.0.get(..first.1)
        })
    });
    if !another {
        return None;
    }
    let window = t.get(back..ahead)?;
    let exempt = window.has_syn() || contains_synthetic_true(window);
    (!exempt).then_some(RuleId::R05)
}

fn contains_synthetic_true(w: &[u8]) -> bool {
    (0..w.len()).any(|k| {
        json_key_at(w, k).is_some_and(|((name, n), mut j)| {
            if name.get(..n) != Some(b"synthetic".as_slice()) {
                return false;
            }
            while matches!(at(w, j), Some(b' ' | b'\t')) {
                j = j.saturating_add(1);
            }
            starts_ci(w, j, b"true")
        })
    })
}

trait FindSyn {
    /// Whether `SYN-` (any case) appears.
    fn has_syn(&self) -> bool;
}

impl FindSyn for [u8] {
    fn has_syn(&self) -> bool {
        (0..self.len()).any(|k| starts_ci(self, k, b"syn-"))
    }
}

// ---- R-10, R-11, R-12: HL7, X12 and FHIR patient records ----

/// An HL7 v2 PID segment at a line start, unless its patient id starts `SYN-`.
fn rule_hl7(t: &[u8], p: usize) -> Option<RuleId> {
    if !line_start(t, p) || !t.get(p..)?.starts_with(b"PID|") {
        return None;
    }
    let line = t.get(p.checked_add(4)?..)?;
    let line = line.get(..line.iter().position(|b| *b == b'\r' || *b == b'\n').unwrap_or(line.len()))?;
    // PID-2, then PID-3: the first that is not empty is the patient id.
    let id = line.split(|b| *b == b'|').skip(1).take(2).find(|f| !f.is_empty()).unwrap_or(&[]);
    (!starts_ci(id, 0, b"syn-")).then_some(RuleId::R10)
}

/// An X12 subscriber or patient name segment with a date of birth segment
/// within 2 KiB, unless the name segment's id starts `SYN`.
fn rule_x12(t: &[u8], p: usize) -> Option<RuleId> {
    let starts_segment = before(t, p).is_none_or(|b| matches!(b, b'~' | b'\n' | b'\r'));
    let rest = t.get(p..)?;
    if !starts_segment || !(rest.starts_with(b"NM1*IL*") || rest.starts_with(b"NM1*QC*")) {
        return None;
    }
    let seg_len = rest.iter().position(|b| matches!(b, b'~' | b'\n')).unwrap_or(rest.len());
    let seg = rest.get(..seg_len)?;
    let id = seg.split(|b| *b == b'*').nth(9).unwrap_or(&[]);
    if starts_ci(id, 0, b"syn") {
        return None;
    }
    let window = rest.get(..RIGHT.min(rest.len()))?;
    (0..window.len()).any(|k| window.get(k..).is_some_and(|w| w.starts_with(b"DMG*D8*"))).then_some(RuleId::R11)
}

/// A FHIR Patient resource, unless its id starts `SYN-`.
fn rule_fhir(t: &[u8], p: usize) -> Option<RuleId> {
    let ((name, n), mut j) = json_key_at(t, p)?;
    if name.get(..n)? != b"resourcetype" {
        return None;
    }
    while matches!(at(t, j), Some(b' ' | b'\t')) {
        j = j.checked_add(1)?;
    }
    if !t.get(j..)?.starts_with(b"\"Patient\"") {
        return None;
    }
    let lo = p.saturating_sub(LEFT);
    let hi = p.saturating_add(LEFT).min(t.len());
    let window = t.get(lo..hi)?;
    let synthetic_id = (0..window.len()).any(|k| {
        json_key_at(window, k).is_some_and(|((name, n), mut j)| {
            if name.get(..n) != Some(b"id".as_slice()) {
                return false;
            }
            while matches!(at(window, j), Some(b' ' | b'\t')) {
                j = j.saturating_add(1);
            }
            starts_ci(window, j, b"\"syn-")
        })
    });
    (!synthetic_id).then_some(RuleId::R12)
}

/// Every rule, in a fixed order, at one anchor. The first that fires wins.
fn decide_at(t: &[u8], p: usize) -> Option<RuleId> {
    let first = at(t, p)?;
    let by_first = match first {
        b'0'..=b'9' => rule_ssn(t, p).or_else(|| rule_mbi(t, p)),
        b'"' => rule_fhir(t, p).or_else(|| rule_json_keys(t, p)),
        b'P' => rule_hl7(t, p),
        b'N' => rule_x12(t, p),
        _ => None,
    };
    let labelled = || {
        if first.is_ascii_alphabetic() { rule_mrn(t, p).or_else(|| rule_dob(t, p)) } else { None }
    };
    by_first.or_else(|| rule_header(t, p)).or_else(labelled)
}

// ---- the streaming scanner ----

fn u64_of(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The scanner's state: the bytes still needed as context, and the verdict so
/// far. Bounded by `LEFT + RIGHT` plus one chunk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanState {
    buf: Vec<u8>,
    base: u64,
    next: u64,
    end: u64,
    verdict: Verdict,
}

impl ScanState {
    /// A fresh scan.
    pub fn new() -> ScanState {
        ScanState::default()
    }
    /// The hits decided so far.
    pub fn verdict(&self) -> Verdict {
        self.verdict
    }
}

fn decide_upto(s: &mut ScanState, upto: u64) {
    while s.next < upto {
        let p = s.next;
        let lo = p.saturating_sub(u64_of(LEFT)).max(s.base);
        let hi = p.saturating_add(u64_of(RIGHT)).min(s.end);
        let idx = |x: u64| usize::try_from(x.saturating_sub(s.base)).ok();
        let fired = match (idx(lo), idx(hi), idx(p)) {
            (Some(l), Some(h), Some(q)) => s
                .buf
                .get(l..h)
                .and_then(|w| q.checked_sub(l).and_then(|qi| decide_at(w, qi))),
            _ => None,
        };
        if let Some(rule) = fired {
            let offset = u32::try_from(p).unwrap_or(u32::MAX);
            s.verdict.push(Hit { rule, offset });
        }
        s.next = p.saturating_add(1);
    }
}

/// Feed one chunk. Positions whose whole window has arrived are decided now.
pub fn detect_step(mut s: ScanState, chunk: &[u8]) -> ScanState {
    s.buf.extend_from_slice(chunk);
    s.end = s.end.saturating_add(u64_of(chunk.len()));
    let ready = s.end.saturating_sub(u64_of(RIGHT));
    decide_upto(&mut s, ready);
    let keep_from = s.next.saturating_sub(u64_of(LEFT)).max(s.base);
    if let Ok(k) = usize::try_from(keep_from.saturating_sub(s.base))
        && k > 0
    {
        s.buf = s.buf.get(k..).map(<[u8]>::to_vec).unwrap_or_default();
        s.base = keep_from;
    }
    s
}

/// End of input: decide the rest.
pub fn detect_finish(mut s: ScanState) -> Verdict {
    let end = s.end;
    decide_upto(&mut s, end);
    s.verdict
}

/// The whole text at once.
pub fn detect(text: &[u8]) -> Verdict {
    detect_finish(detect_step(ScanState::new(), text))
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    // Fixtures that must fire are split across literals, so that heddle's own
    // repository scan never sees them whole. 987-12-3456 is in a range issued
    // neither as a Social Security number nor as a taxpayer id.
    const SSN_DASHED: &str = concat!("987-12", "-3456");
    const SSN_SPACED: &str = concat!("987 12", " 3456");
    const SSN_BARE: &str = concat!("98712", "3456");

    const NO: [RuleId; 0] = [];

    fn rules(text: &str) -> Vec<RuleId> {
        detect(text.as_bytes()).hits.iter().flatten().map(|h| h.rule).collect()
    }

    #[test]
    fn r01_r09_social_security_numbers_and_their_synthetic_forms() {
        assert_eq!(rules(&alloc::format!("ssn: {SSN_DASHED}")), [RuleId::R01]);
        assert_eq!(rules(&alloc::format!("Social Security {SSN_SPACED}")), [RuleId::R01]);
        assert_eq!(rules(&alloc::format!("SSN {SSN_BARE} on file")), [RuleId::R01]);
        assert_eq!(rules(&alloc::format!("id {SSN_DASHED} here")), [RuleId::R09], "dashed, no keyword");
        assert_eq!(rules(&alloc::format!("id {SSN_SPACED} here")), NO, "spaced needs a keyword");
        for synthetic in ["000-12-3456", "666-12-3456", "987-00-3456", "987-12-0000"] {
            assert_eq!(rules(&alloc::format!("ssn {synthetic}")), NO, "{synthetic}");
        }
        assert_eq!(rules(&alloc::format!("ssn 1{SSN_DASHED}")), NO, "part of a longer number");
        // A Unicode en dash is the same separator.
        let en = concat!("987\u{2013}12", "\u{2013}3456");
        assert_eq!(rules(&alloc::format!("ssn {en}")), [RuleId::R01]);
        // The keyword more than 40 bytes away does not count.
        let far = alloc::format!("ssn{}{SSN_SPACED}", " ".repeat(60));
        assert_eq!(rules(&far), NO);
    }

    #[test]
    fn r02_r03_r04_identifiers_after_their_labels() {
        let mbi = concat!("1EG4", "-TE5-MK73");
        assert_eq!(rules(&alloc::format!("Medicare id {mbi}")), [RuleId::R02]);
        assert_eq!(rules(&alloc::format!("code {mbi}")), NO, "no keyword");
        assert_eq!(rules(concat!("MRN: A12", "34567")), [RuleId::R03]);
        assert_eq!(rules(concat!("patient id ", "00123456 seen")), [RuleId::R03]);
        assert_eq!(rules(concat!("MRN: SYN-", "1234567")), NO, "synthetic");
        assert_eq!(rules("MRN: 12345"), NO, "too short");
        assert_eq!(rules(concat!("DOB 1957", "-03-02")), [RuleId::R04]);
        assert_eq!(rules(concat!("date of birth: 3/2", "/1957")), [RuleId::R04]);
        assert_eq!(rules(concat!("birthdate March 2", ", 1957")), [RuleId::R04]);
        assert_eq!(rules("dob unknown"), NO);
    }

    #[test]
    fn r05_identifier_fields_in_headers_and_json() {
        assert_eq!(rules(concat!("first_name,last", "_name,visit\nA,B,C\n")), [RuleId::R05]);
        assert_eq!(rules(concat!("first_name,last", "_name,synthetic\n")), NO, "a synthetic column");
        assert_eq!(rules(concat!("first_name,last", "_name,id\nA,B,SYN-1\n")), NO, "a SYN- value");
        assert_eq!(rules("prose, with commas, about a first name and a last name"), NO);
        let json = concat!("{\"first_name\": \"A\", \"dob", "\": \"x\"}");
        assert_eq!(rules(json), [RuleId::R05]);
        let syn = concat!("{\"first_name\": \"A\", \"dob", "\": \"x\", \"synthetic\": true}");
        assert_eq!(rules(syn), NO);
    }

    #[test]
    fn r10_r11_r12_record_formats() {
        assert_eq!(rules(concat!("MSH|^~\\&|x\nPI", "D|1||123456^^^H||DOE^JANE\n")), [RuleId::R10]);
        assert_eq!(rules(concat!("PI", "D|1||SYN-123^^^H||DOE^JANE\n")), NO);
        let x12 = concat!("NM1*IL*1*DOE*JANE****MI*12345", "6789~DMG*D8*19570302*F~");
        assert_eq!(rules(x12), [RuleId::R11]);
        let x12_syn = concat!("NM1*IL*1*DOE*JANE****MI*SYN12", "3~DMG*D8*19570302*F~");
        assert_eq!(rules(x12_syn), NO);
        assert_eq!(rules(concat!("{\"resourceType\": \"Pat", "ient\", \"id\": \"p1\"}")), [RuleId::R12]);
        assert_eq!(rules(concat!("{\"resourceType\": \"Pat", "ient\", \"id\": \"SYN-1\"}")), NO);
    }

    #[test]
    fn nine_hits_set_more_and_the_offsets_name_the_anchor() {
        let mut text = String::new();
        for _ in 0..9 {
            text.push_str(&alloc::format!("id {SSN_DASHED} "));
        }
        let v = detect(text.as_bytes());
        assert!(v.more && v.fired());
        assert_eq!(v.first(), Some(Hit { rule: RuleId::R09, offset: 3 }));
    }

    #[test]
    fn streaming_equals_whole_text_for_every_cutting_tried() {
        let mut text = String::new();
        text.push_str(&"x".repeat(3000));
        text.push_str(&alloc::format!("\nssn: {SSN_DASHED}\n"));
        text.push_str(&"y ".repeat(1500));
        text.push_str(concat!("NM1*QC*1*DOE*JANE****MI*12345", "6789~"));
        text.push_str(&"z".repeat(1000));
        text.push_str("DMG*D8*19570302*F~\n");
        text.push_str(concat!("first_name,last", "_name,visit\n"));
        text.push_str(concat!("DOB 1957", "-03-02 and MRN: A12", "34567"));
        let bytes = text.as_bytes();
        let whole = detect(bytes);
        assert!(whole.fired());
        let mut seed = 7u64;
        for size in [1usize, 2, 3, 5, 64, 511, 512, 513, 2047, 2048, 2049, 4096, bytes.len()] {
            let mut s = ScanState::new();
            for chunk in bytes.chunks(size) {
                s = detect_step(s, chunk);
            }
            assert_eq!(detect_finish(s), whole, "chunks of {size}");
        }
        for _ in 0..50 {
            let mut s = ScanState::new();
            let mut i = 0;
            while i < bytes.len() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let n = 1 + usize::try_from(seed >> 52).unwrap_or(1);
                let j = (i + n).min(bytes.len());
                s = detect_step(s, &bytes[i..j]);
                i = j;
            }
            assert_eq!(detect_finish(s), whole, "random cutting");
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 1u64;
        let mut junk = Vec::new();
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            junk.push(u8::try_from(seed >> 56).unwrap_or(0));
        }
        let _ = detect(&junk);
        let _ = detect(&[]);
        let _ = detect(b"\"");
        let _ = detect(b"PID|");
        let _ = detect(b"NM1*IL*");
    }
}
