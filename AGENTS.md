# Working in heddle

Read this before changing anything. `CLAUDE.md` points here.

## Ground rules

- **Never patient data, secrets or tokens**, anywhere: code, tests, fixtures, commit messages, issues, logs. Invent test data, and use the synthetic forms the PHI rules exempt (`SYN-` record ids, Social Security numbers in area `000` or `666`).
- **This repo is public.** Never name or describe other projects, other repos or other people's machines. Keep paths generic in anything you commit (`/Users/me`, `/tmp/day0`).
- **Commit small and often, to `main`, and push.** Stage files by name, never `git add -A`. Never force-push, stash, reset or rewrite history.

## The core (`crates/fabric-core`)

- `#![no_std]` with `alloc`, `#![forbid(unsafe_code)]`, and the lint wall in the workspace `Cargo.toml`: no `unwrap`, `expect`, `panic!`, indexing, string slicing, unchecked arithmetic or `as`. Use `get`, `checked_*` and `saturating_*`, and `From` or `TryFrom`.
- Every function is total. A refusal is a value, never an error path or a panic.
- No `HashMap` or `HashSet` (iteration order); no float, `usize` or `String` in any stored or wire type.
- Each grammar and witness type has exactly one checked constructor, in its own module, and derives no `Default`, `Deserialize` or `From`.
- Invariant ids (K1, C5, T11, ...) are the specification's; a test that checks one says so in its name.

## Dependencies

`deny.toml` is the allow-list. The core may use `sha2` and `ed25519-dalek` (verify only). Elsewhere: `serde`, `serde_json`, `getrandom` and one monotonic-clock wrapper. `regex` and `proptest` are dev-only. No git library, argument parser, async runtime or TLS stack: heddle calls `/usr/bin/curl` and `git`. No crate version younger than seven days.

## Checks

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs those, a WebAssembly build of the core, `cargo deny`, `cargo audit`, a crate count and Betterleaks over every commit.
