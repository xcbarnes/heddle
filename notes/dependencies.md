# Dependencies: what heddle uses, and a path to the standard library only

2026-11-13, at commit `4843369`, for the redesign panel. Facts are taken from `Cargo.lock`, `cargo tree` for each target, and the vendored sources.

## In one paragraph

heddle has **three direct third-party dependencies**: `serde_json` (to read Claude Code's hook payloads and write JSON), `sha2` (SHA-256) and `ed25519-dalek` (Ed25519 signatures). Together they bring **27 more crates** into `Cargo.lock`, 30 in all. On this Mac **21 of them are compiled**; on CI's Linux runner, 25; **4 are never compiled anywhere**, being version pins behind `cfg(any())`. Everything else heddle does already uses only Rust's standard library: files, locks, processes, `/dev/urandom` and the system `git`. HTTPS is also planned through the system `curl`. **A std-only heddle is reachable**: a hand-written JSON reader and writer (about 400 lines), a hand-written SHA-256 (about 120 lines), and dropping Ed25519, which only the rejected admin-user and release-record design needs.

## Who asked for what

| Our crate | Direct dependencies | Why |
|---|---|---|
| `fabric-core` (the pure core) | `sha2`, `ed25519-dalek` | SHA-256 of seat proofs; verifying admin-signed records (release records; planned: admin commands to the room, room snapshots) |
| `heddle` (the hook binary) | `fabric-core`, `serde_json`; dev only: `ed25519-dalek` | parsing hook payloads, writing hook output and the managed-settings file; tests sign stand-in records |
| `heddle-admin` (the admin user's tool, now parked) | `fabric-core`, `ed25519-dalek` | signing release records |

## Every crate in `Cargo.lock`

"Built" columns show where the crate is compiled: on this Mac (aarch64 macOS), on CI's Linux runner (x86_64), and inside the core's WebAssembly build for the room. "Script" means it has a build script, which runs code at compile time; "macro" means a procedural macro, which also runs at compile time.

| Crate | Version (published) | For | Pulled in by | Mac | Linux | Wasm | Script or macro | Licence |
|---|---|---|---|---|---|---|---|---|
| **serde_json** | 1.0.154 (2026-10-11) | JSON parsing and writing | heddle | yes | yes | no | script | MIT or Apache-2.0 |
| serde_core | 1.0.229 (2026-07-18) | serde's traits, which serde_json implements | serde_json | yes | yes | no | script | MIT or Apache-2.0 |
| itoa | 1.0.18 (2026-03-20) | formatting integers | serde_json | yes | yes | no | | MIT or Apache-2.0 |
| memchr | 2.8.3 (2026-07-08) | fast byte search inside the parser | serde_json | yes | yes | no | | Unlicense or MIT |
| zmij | 1.0.23 (2026-07-13) | formatting floating-point numbers | serde_json | yes | yes | no | script | MIT |
| serde | 1.0.229 | a version pin only, behind `cfg(any())` | serde_json | **never** | **never** | **never** | (script) | MIT or Apache-2.0 |
| serde_derive | 1.0.229 | a version pin only, behind `cfg(any())` | serde_core | **never** | **never** | **never** | (macro) | MIT or Apache-2.0 |
| syn | 3.0.9 | serde_derive's parser; never built for the same reason | serde_derive | **never** | **never** | **never** | | MIT or Apache-2.0 |
| **sha2** | 0.11.1 (2026-10-12) | SHA-256 | fabric-core, ed25519-dalek | yes | yes | yes | | MIT or Apache-2.0 |
| digest | 0.11.3 (2026-05-03) | the hash-function traits | sha2, ed25519-dalek | yes | yes | yes | | MIT or Apache-2.0 |
| block-buffer | 0.12.1 (2026-06-10) | buffering input into hash blocks | digest | yes | yes | yes | | MIT or Apache-2.0 |
| crypto-common | 0.2.2 (2026-05-18) | shared cryptography traits | digest | yes | yes | yes | | MIT or Apache-2.0 |
| hybrid-array | 0.4.15 (2026-09-08) | fixed-size arrays sized by types | digest, block-buffer | yes | yes | yes | | MIT or Apache-2.0 |
| typenum | 1.20.1 (2026-05-29) | numbers as types, for those arrays | hybrid-array | yes | yes | yes | | MIT or Apache-2.0 |
| cpufeatures | 0.3.1 (2026-08-25) | detecting CPU instructions for speed | sha2, curve25519-dalek | yes | yes | no | | MIT or Apache-2.0 |
| libc | 0.2.191 (2026-10-16) | the C library calls cpufeatures makes on macOS | cpufeatures | yes | no | no | script | MIT or Apache-2.0 |
| cfg-if | 1.0.5 (2026-09-15) | a conditional-compilation macro | sha2, curve25519-dalek | yes | yes | yes | | MIT or Apache-2.0 |
| **ed25519-dalek** | 3.0.0 (2026-07-06) | Ed25519 sign and verify | fabric-core, heddle-admin | yes | yes | yes | | BSD-3-Clause |
| ed25519 | 3.0.0 (2026-05-03) | the Ed25519 signature type | ed25519-dalek | yes | yes | yes | | Apache-2.0 or MIT |
| signature | 3.0.0 (2026-05-02) | the sign and verify traits | ed25519 | yes | yes | yes | | Apache-2.0 or MIT |
| curve25519-dalek | 5.0.0 (2026-07-06) | the elliptic-curve arithmetic under Ed25519 | ed25519-dalek | yes | yes | yes | script | BSD-3-Clause |
| subtle | 2.6.1 (2024-06-24) | constant-time comparisons | curve25519-dalek, ed25519-dalek | yes | yes | yes | | BSD-3-Clause |
| rustc_version | 0.4.1 (2024-08-05) | curve25519-dalek's build script reads the compiler version | curve25519-dalek (build) | yes | yes | yes | | MIT or Apache-2.0 |
| semver | 1.0.28 (2026-04-04) | version parsing for rustc_version | rustc_version | yes | yes | yes | | MIT or Apache-2.0 |
| curve25519-dalek-derive | 0.1.1 (2023-10-31) | generates the x86_64 vector code | curve25519-dalek | no | yes | no | macro | MIT/Apache-2.0 |
| proc-macro2 | 1.0.107 (2026-07-19) | used by that macro | curve25519-dalek-derive | no | yes | no | script | MIT or Apache-2.0 |
| quote | 1.0.47 (2026-07-19) | used by that macro | curve25519-dalek-derive | no | yes | no | script | MIT or Apache-2.0 |
| syn | 2.0.119 (2026-07-16) | used by that macro | curve25519-dalek-derive | no | yes | no | | MIT or Apache-2.0 |
| unicode-ident | 1.0.27 (2026-10-12) | used by that macro | proc-macro2 | no | yes | no | | (MIT or Apache-2.0) and Unicode-3.0 |
| fiat-crypto | 0.3.0 | an alternative curve backend, never selected | curve25519-dalek | **never** | **never** | **never** | | MIT, Apache-2.0 or BSD-1-Clause |

What the supply chain checks today:
- **The checks in CI:** `cargo deny` (an allow-list of these crates, licences, known advisories, and a named list of build scripts and the one macro), `cargo audit`, a 40-crate budget, and a refusal of any locked version younger than seven days.
- **Sources:** vendored by CI and checked against the lockfile's checksums. This Mac never fetches crate code.

## Supply chain outside `Cargo.lock`

- **The Rust toolchain:** rustup, cargo and rustc 1.99.1, from the official Rust distribution. Xavier installed rustup himself.
- **GitHub Actions**, pinned by commit: `actions/checkout`, `actions/upload-artifact`, and `actions/attest-build-provenance` (used only by the parked release workflow).
- **CI-only tools,** pinned by SHA-256 and run only on GitHub's runners, never on a laptop: cargo-deny, cargo-audit, Betterleaks and Quint.

## A path to the standard library only

| Need | Today | Std-only replacement | Size and risk |
|---|---|---|---|
| Read hook payloads (JSON on stdin) | `serde_json` and its 4 crates (plus 3 never built) | A hand-written, total JSON reader: objects, arrays, strings with every escape (including `\u` surrogate pairs), integers, true, false, null. A depth cap and a size cap, no recursion, no panics under the lint wall. The spec already planned a hand-written decoder for the room's wire (decision A6), and one reader can serve both | About 300 lines. The risk is parser bugs. It would be checked by property tests, by fuzzing in CI only (`cargo-fuzz`, nothing on the laptop), and by a differential test against Python's `json` run from a script, which needs no crate |
| Write hook output and the settings file | `serde_json` | A hand-written writer for a few fixed shapes; only string escaping needs care | About 100 lines. Low risk: the output shapes are fixed and tested |
| SHA-256 | `sha2` and its 8 crates (libc only on macOS) | A hand-written SHA-256 in the core, about 120 lines, checked against the published NIST test vectors and against `/usr/bin/shasum` on random inputs from a script | Low risk: the algorithm is short and fully specified. It runs a little slower without CPU-specific instructions, which nothing here notices |
| Ed25519 signatures | `ed25519-dalek` and its 12 crates (5 only on x86_64, 1 never built) | **Not recommended to hand-write**: the curve arithmetic is about 1,000 lines, and the subtle part. Only the rejected design needs it: admin-signed commands, release records and signed room snapshots. Without those, **HTTPS through the system `curl` authenticates the room**. An HMAC-SHA-256 with a shared secret is one std-only option if some other integrity check is wanted | Zero lines if dropped. What is lost: verifying signed snapshots offline. Without a root-owned key, that protects little against a process running as the same user anyway |
| HTTPS to the room | (planned) | `/usr/bin/curl`, with the token passed on standard input, never in arguments: the plan already (decision C22) | No new code beyond the process call |
| Random values | `/dev/urandom` through `std::fs` | already std-only | |
| File locks, atomic writes, processes | `std::fs::File::lock`, rename, `std::process` | already std-only | |
| A clock that keeps counting during sleep (age of a cached snapshot) | not built yet | std has none on macOS: `Instant` stops during sleep. Use the wall clock with a backwards-step check, and the live, nonce-checked room call for anything that matters | No crate needed |
| The room, if it stays a Cloudflare Worker | the planned WebAssembly core plus a TypeScript shell | The platform's own WebCrypto gives SHA-256, HMAC and Ed25519 with no npm package | No dependency |

**The result:** heddle and the core with **zero third-party crates**, only the Rust standard library and the system's `curl` and `git`. In exchange: about 520 lines of hand-written, tested JSON and SHA-256 code, and dropping signatures. `cargo deny` and `cargo audit` would then have nothing left to check, and could stay as a guard against a crate being added later.

## Parked by the redesign hold

These were committed before the hold and add no crates of their own. They can be deleted if the redesign drops them:
- `crates/heddle-admin/`;
- `crates/heddle/src/setup.rs`;
- `.github/workflows/release.yml`;
- `crates/fabric-core/src/sig.rs`.

The hook binary (`heddle hook`) does not depend on any of them.
