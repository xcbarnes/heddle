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

None. `Cargo.lock` lists only this workspace's packages, and CI fails otherwise. The standard library does the work, with our own JSON reader (`crates/fabric-core/src/json.rs`, checked against Python's `json` by `spec/jsondiff/run.sh`) and hand-parsed arguments.

heddle opens no sockets and runs no program from a hook. clippy refuses `std::net` and `std::process::Command`: a process call goes in one edge module that allows the lint by name, starts each program by absolute path, and has a test listing them all.

## Checks

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo test --workspace --frozen
spec/jsondiff/run.sh
```

With nothing to fetch, builds need no network. A local binary is for tests only: anything installed comes from an attested CI release. Test launches of `claude` live in `day0/` scripts, never in `cargo test`.

CI runs those, a WebAssembly build of the core and the lockfile check. GitHub's secret scanning and push protection cover every push.
