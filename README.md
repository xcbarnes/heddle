# heddle

A small coordination layer for many parallel AI coding agents: expiring claims, live status and a fail-closed PHI brake, wired into harness hooks. Part of the Fabric project: "air traffic control at the speed of agentic development".

**Status: v0.1 is being built.** Nothing here is installed or released yet.

## What it is

Fabric has two planes. The *lasting* plane is git and markdown: task files and records. The *live* plane is **the room**: one small Cloudflare server per repo that holds who is on what, right now. **heddle** is the command-line tool wired into each harness's hooks, so agents coordinate as a side effect of working.

- **One pure Rust core.** Every rule is a total function `step(room, input, now) -> (room, outcome, effects)`: no I/O, no clock, no panics, no `unsafe`, built without the standard library. The same code runs in the room (as WebAssembly), in heddle and under the model checker.
- **Claims are efficiency locks.** A lease lasts 15 minutes on a logical clock that a silence can move by at most 2 minutes. Git's refusal of a push onto a moved main is what protects the code.
- **The PHI brake fails closed.** A local pre-filter watches prompts, tool inputs, tool results and the transcript. When it fires, or cannot run, the thread stops. It has two exits only: rewind, or migrate.
- **If Cloudflare is down, Fabric is off.** There is no offline claim.

## Layout

| Path | Holds |
|---|---|
| `crates/fabric-core` | the pure core: types, grammars, every state machine, `render`, `detect`, `flow` |
| `day0/` | the day-0 hook tests against the pinned Claude Code, and [their results](day0/RESULTS.md) |
| `adapters/claude-code/` | recorded hook payloads, for the decoder tests |
| `crates/heddle` | the command-line tool: the hook lines, and `setup`, `doctor`, `repair`, `uninstall`, `status` and `phi recheck` |
| `spec/` | the Quint model of the room, the detector's rules, and the JSON differential test |
| `.github/workflows/ci.yml` | lints, tests, the WebAssembly build, the JSON differential test and the no-third-party-crates check |

Still to come in v0.1: the local room and the agent verbs (claims, news, `done`).

## Building

Rust is pinned in `rust-toolchain.toml` (1.99.1). Binaries that get installed come only from a CI release; a local build is for tests.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
spec/jsondiff/run.sh
```

## Rules for this repo

- No secrets, tokens or patient data, ever, in code, tests, fixtures, commits or issues. Test data is invented, and uses the reserved synthetic forms (`SYN-` ids, Social Security numbers in area 000 or 666).
- No third-party crates. CI fails if `Cargo.lock` lists any package that is not this workspace's own.

Licensed under Apache-2.0.
