# heddle

A small coordination layer for many parallel AI coding agents: expiring claims, live status and a fail-closed PHI brake, wired into harness hooks. Part of the Fabric project: "air traffic control at the speed of agentic development".

**Status: v0.1 release candidates.** rc.1 is the PHI brake; rc.2 adds coordination between the conversations on one computer, through a local room. The hosted room comes later.

## What it is

Fabric has two planes. The *lasting* plane is git and markdown: task files and records. The *live* plane is **the room**: one small Cloudflare server per repo that holds who is on what, right now. **heddle** is the command-line tool wired into each harness's hooks, so agents coordinate as a side effect of working.

- **One pure Rust core.** Every rule is a total function `step(room, input, now) -> (room, outcome, effects)`: no I/O, no clock, no panics, no `unsafe`, built without the standard library. The same code runs in the room (as WebAssembly), in heddle and under the model checker.
- **Claims are efficiency locks.** A lease lasts 15 minutes on a logical clock that a silence can move by at most 2 minutes. Git's refusal of a push onto a moved main is what protects the code.
- **The PHI brake fails closed.** A local pre-filter watches prompts, tool inputs, tool results and the transcript. When it fires, or cannot run, the thread stops. It has two exits only: rewind, or migrate.
- **If Cloudflare is down, Fabric is off.** There is no offline claim. (In v0.1 the room is a file on one computer, so this comes with the hosted room.)

## Coordination on one computer (v0.1)

The room is a file in the repository's git folder, `.git/fabric/room`, under a lock, so every worktree of a project shares it. A seat is one conversation. The hooks do the work as a side effect: a session starts with a short digest (who else is here, what they hold, what is ready), edits leave footprints and the second editor of a file in 15 minutes hears of the first, and the next prompt carries news (a takeover, work done, main moving, `DECISIONS.md` or `fabric/records/` changing). Coordination runs only after the PHI check lets an action go on, waits at most 20 ms for the room, and never blocks anything except a `git push` while a person's stop holds.

Agents run the verbs as Bash commands, allowed without a prompt:

| Verb | Does |
|---|---|
| `heddle next [--claim]` | lists ready work (`fabric/tasks/*.md` whose `status:` is ready), or claims the first |
| `heddle claim KEY` | takes a 15-minute lease that the conversation's hooks renew while it works; at most three per conversation |
| `heddle release KEY` | gives it back |
| `heddle done KEY` | closes it once this checkout's commit is on `origin/main` |
| `heddle pause` | holds new claims and done for 15 minutes (at most an hour); they complete when it lapses |
| `heddle status` | who is here, what they hold, and the install's health |

`heddle stop` (pushes held for 30 minutes, then a pause) and `heddle resume` are the person's: an agent asks before running them.

## Layout

| Path | Holds |
|---|---|
| `crates/fabric-core` | the pure core: types, grammars, every state machine, `render`, `detect`, `flow` |
| `day0/` | the day-0 hook tests against the pinned Claude Code, and [their results](day0/RESULTS.md) |
| `adapters/claude-code/` | recorded hook payloads, for the decoder tests |
| `crates/heddle` | the command-line tool: the hook lines, the agent verbs, and `setup`, `doctor`, `repair`, `uninstall`, `phi recheck` and `phi flag` |
| `spec/` | the Quint model of the room, the detector's rules, and the JSON differential test |
| `.github/workflows/ci.yml` | lints, tests, the WebAssembly build, the JSON differential test and the no-third-party-crates check |

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
