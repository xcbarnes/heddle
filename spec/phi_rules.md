# Stage-1 PHI rules: reference definitions

These are the reference definitions for the hand-written matchers in [`crates/fabric-core/src/detect.rs`](../crates/fabric-core/src/detect.rs). Each is a regular expression in Rust `regex` syntax. A differential test, with `regex` as a dev-only dependency, will check the hand-written matcher against it. Where this file and the code differ, that test decides which is wrong.

Notation used below:
- `SEP` is one separator between digits: `[-. ]`, or a Unicode look-alike (U+00A0, U+2009, U+2010 to U+2015, U+2024, U+202F, U+2212, U+FE63, U+FF0D, U+FF0E).
- `NEAR(k)` means keyword `k` ends within 40 bytes before the match, or begins within 40 bytes after it.
- Every keyword is case-insensitive and word-bounded. A space inside a keyword matches `[ \t_-]+`.
- `LABEL` is up to four bytes of `[ \t:#=\-.,;]` after a label.

| Rule | Reference | Exempt (never fires) |
|---|---|---|
| R-01 | `(?<!\d)\d{3}(SEP\d{2}SEP\|\d{2})\d{4}(?!\d)` with `NEAR(ssn \| social security)` | area `000` or `666`, group `00`, serial `0000` |
| R-09 | `(?<!\d)\d{3}DASH\d{2}DASH\d{4}(?!\d)`, no keyword needed. `DASH` is the dash subset of `SEP` | the same. Areas 900 to 999 are not exempt |
| R-02 | `\b[1-9][A-Z][A-Z0-9]\d-?[A-Z][A-Z0-9]\d-?[A-Z]{2}\d{2}\b` (letters exclude S, L, O, I, B, Z; any case) with `NEAR(medicare \| mbi)` | none |
| R-03 | `\b(mrn\|medical record number\|medical record no\|patient id)\bLABEL[A-Za-z0-9]{6,12}\b`, where the value holds at least one digit | the value starts `SYN-`. A value with no digit is a field name or a type (`patient_id: string`), not a record |
| R-04 | `\b(dob\|date of birth\|birthdate\|birth date)\bLABEL(DATE)`, where `DATE` is `\d{4}[-/.]\d{1,2}[-/.]\d{1,2}`, or `\d{1,2}[-/.]\d{1,2}[-/.]\d{4}`, or a month name or its abbreviation, a day (`1` to `31`, optional `st`, `nd`, `rd`, `th`), an optional comma and a four-digit year. Months are 1 to 12 and days 1 to 31 | none |
| R-05 | a line of plain field names split by `,`, tab, `\|` or `;` (two or more delimiters) with two or more distinct direct-identifier names; or a JSON object with two or more distinct direct-identifier keys within 512 bytes | a `synthetic` column, or `SYN-` on the next line; in JSON, `"synthetic": true` or a `"SYN-` value nearby |
| R-10 | `(?m)^PID\|` | PID-2, or PID-3 if PID-2 is empty, starts `SYN-` |
| R-11 | `(^\|[~\n\r])NM1\*(IL\|QC)\*` with `DMG\*D8\*` within 2 KiB after | NM1-09 (the id) starts `SYN` |
| R-12 | `"resourceType"\s*:\s*"Patient"` | an `"id"` within 512 bytes whose value starts `SYN-` |

**Direct-identifier names** for R-05 are compared with everything but letters and digits removed, in lowercase: `firstname`, `lastname`, `givenname`, `familyname`, `fullname`, `patientname`, `dob`, `dateofbirth`, `birthdate`, `ssn`, `socialsecuritynumber`, `mrn`, `medicalrecordnumber`, `memberid`, `subscriberid`, `address`, `streetaddress`, `address1`, `addressline1`, `phone`, `phonenumber`, `telephone`.

**The raw-byte scans** (the transcript scan, and the second net over hook payloads) run every rule twice: over the raw bytes, and over `unescape` of them, one blind pass that turns JSON's escapes back into characters. In JSON a newline is the two bytes `\n`, so without it the line-shaped rules (R-10, R-11, R-05's header row) could not match.

**Decided at the edge**, not by these matchers:
- R-00: the detector did not run.
- R-06: a path on the restricted-paths list.
- R-07: too large to scan, or does not decode.
- R-08: a content type the rules cannot read, which v0.1 counts and does not block.

**Not yet built:** the shadow rules (phone numbers, email addresses, bare dates and names). They count, but do not block, in prompts and tool results, and they block on the path and slug channels.

## Synthetic data that never fires

Use these forms for fixtures, test data and examples:
- record ids that start `SYN-`;
- Social Security numbers in area `000` or `666`, group `00` or serial `0000`;
- `"synthetic": true` in JSON objects, or a `synthetic` column in tables.

Exemptions are by value, never by folder, so real rows copied into a synthetic folder still fire.
