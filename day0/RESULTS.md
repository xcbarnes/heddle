# Day-0 hook tests: Claude Code 2.1.321

Run 2026-11-13 on Xavier's Mac, with the terminal Claude Code pinned at 2.1.321 (the same build the Desktop app bundles). `run.sh` drove 38 real `claude -p` sessions: Haiku, the Bash tool only, project settings only (`--setting-sources project`), and a clean terminal-like environment. In each, `probe.sh` stands in for `heddle hook` and fails on purpose in one way.

The evidence is physical, never the model's word:

- **PreToolUse and UserPromptSubmit cases** ask for `touch marker`. Did the file appear?
- **PostToolUse and PostToolBatch cases** ask for `cat secret.txt`, which holds a random token the model can learn only from the tool result. Did the token reach the tool result in the stream, the model's reply, or the transcript on disk?

## What this settles

| Open item | Result |
|---|---|
| **new-15: a hung hook must block (gate for first daily use)** | **Passes, by both routes.** (1) *The harness:* a PreToolUse hook killed at its 5 s timeout, with `onFailure: "block"`, blocked the call: `timed out; blocking because onFailure is "block"`. UserPromptSubmit blocked the prompt the same way. (2) *heddle's watchdog:* a hook that ended itself with exit 3 after 2 s also blocked |
| **B9: the PHI lines fail closed** | **Holds on every line that can block.** With `onFailure: "block"`, PreToolUse and UserPromptSubmit blocked on exit 1, exit 101, `kill -9`, a timeout, a missing binary, invalid JSON, and JSON cut off by `kill -9` mid-write. Without the key, every one of those let the action go ahead (only exit 2 blocks then) |
| **new-14: heddle dies after the tool has run** | **PostToolUse cannot hold the result back.** With `onFailure: "block"`, exit 1, `kill -9`, a timeout and a missing binary each let the original result through: the token was in the reply and the transcript. **PostToolBatch can.** Each of the same failures stopped the loop before the next model call: the token never reached a reply. With both lines installed, a PostToolUse death followed by a PostToolBatch stop kept the token out of the reply too. So the limit narrows to a stated edge. A result that slips past a dead PostToolUse is scanned by PostToolBatch, and a dead PostToolBatch stops the loop. The remaining case is a dead PostToolBatch, which stops the loop but leaves no label. The result is then sent with the next prompt, unless the transcript scan at that prompt's UserPromptSubmit finds it first. The transcript scan is the net there, and it can lag (`transcript_path` "may lag") |
| **Result replacement (`updatedToolOutput`, Bash)** | **Works.** The model saw only the replacement |
| **Does a replaced result still land in the transcript?** | **No.** The transcript holds the replacement, not the original. Telemetry still captures the original (docs), which is why setup pins telemetry off (decision new-19) |
| **Stop before the next model call (PostToolBatch)** | **Works**, with `{"decision":"block"}` and on any failure under `onFailure: "block"` |
| **Fork payloads (M12)** | SessionStart says `source: "fork"` and carries a **new** `session_id`. **No field names the parent.** The fork's transcript rewrites every entry to the new session id, but keeps the parent's message `uuid`s: its first user message has the parent's root uuid. `--resume` keeps the same `session_id` and says `source: "resume"` |
| **Who warns when the binary is gone** | The harness. Wording on 2.1.321: `PreToolUse:Bash hook error: [<command>]: failed; blocking because onFailure is "block"`, then `Error occurred while executing hook command: ENOENT: no such file or directory, posix_spawn '<path>'` |
| **Does a blocked prompt echo its text?** | Not in the block message, in `-p` mode, either with `suppressOriginalPrompt` or after a crash. **The transcript on disk always holds the prompt text**, as the docs warn. That is why the spec marks the conversation `Residue` until `heddle phi scrub`. Interactive mode is not tested yet |

## What it means for the build

1. **Every PHI hook line carries `onFailure: "block"`.** Without it, a crash, a hang or a missing binary lets the action through on every event.
2. **heddle prints exactly one complete JSON object, and exits 0, on every path that decides anything.** Plain text on stdout counts as success even under `onFailure: "block"` (docs, confirmed: `pre-plaintext`). Output cut off mid-write blocks only because the process then dies non-zero.
3. **The watchdog exits non-zero before the line's timeout.** Either route blocks; heddle keeps its own so the brake does not rest on one vendor behaviour.
4. **T3 for forks:** a fork cannot be traced to its parent from the payload. heddle can still trace it. Record the root message `uuid` of each quarantined conversation, and at `SessionStart(fork)` (or on any hook from an unknown conversation) read the first entries of the new transcript and compare. M12 itself needs only `source: "fork"`.
5. **"A turn a person typed":** the UserPromptSubmit payload has no field for the prompt's source (keys: `cwd`, `hook_event_name`, `permission_mode`, `prompt`, `prompt_id`, `session_id`, `transcript_path`). The transcript has `promptSource` and `turnOrigin` (`"sdk"` for `-p`), but it can lag. The conservative rule in M13 stays.

## The runs

`fired` counts the hook payloads probe.sh logged. `marker`: the PreToolUse or UserPromptSubmit case's file appeared (the action ran). `result`, `reply`, `transcript`: the token was in the tool result, the model's reply, the transcript.

| case | event | failure | onFailure | fired | marker | result | reply | transcript |
|---|---|---|---|---|---|---|---|---|
| pre-ok | PreToolUse | none (control) | block | 1 | **yes** | | | |
| pre-exit1 | PreToolUse | exit 1 | block | 1 | no | | | |
| pre-exit2 | PreToolUse | exit 2 | block | 1 | no | | | |
| pre-exit101 | PreToolUse | exit 101 | block | 1 | no | | | |
| pre-hang | PreToolUse | hangs past a 5 s timeout | block | 1 | no | | | |
| pre-watchdog | PreToolUse | exits 3 after 2 s | block | 1 | no | | | |
| pre-kill9 | PreToolUse | `kill -9` itself | block | 1 | no | | | |
| pre-missing | PreToolUse | binary missing | block | 0 | no | | | |
| pre-plaintext | PreToolUse | prints `{…` with no closing brace, exit 0 | block | 1 | **yes** | | | |
| pre-badjson | PreToolUse | invalid JSON, exit 0 | block | 1 | no | | | |
| pre-partial | PreToolUse | half a deny, then `kill -9` | block | 1 | no | | | |
| pre-deny | PreToolUse | a JSON deny (control) | block | 1 | no | | | |
| pre-exit1-unset | PreToolUse | exit 1 | unset | 1 | **yes** | | | |
| pre-exit101-unset | PreToolUse | exit 101 | unset | 1 | **yes** | | | |
| pre-hang-unset | PreToolUse | hangs past a 5 s timeout | unset | 1 | **yes** | | | |
| pre-kill9-unset | PreToolUse | `kill -9` itself | unset | 1 | **yes** | | | |
| pre-missing-unset | PreToolUse | binary missing | unset | 0 | **yes** | | | |
| ups-ok | UserPromptSubmit | none (control) | block | 1 | **yes** | | | |
| ups-exit1 | UserPromptSubmit | exit 1 | block | 1 | no | | | |
| ups-hang | UserPromptSubmit | hangs past a 5 s timeout | block | 1 | no | | | |
| ups-kill9 | UserPromptSubmit | `kill -9` itself | block | 1 | no | | | |
| ups-missing | UserPromptSubmit | binary missing | block | 0 | no | | | |
| ups-block | UserPromptSubmit | a JSON block with `suppressOriginalPrompt` (control) | block | 1 | no | | | |
| post-ok | PostToolUse | none (control) | block | 1 | | yes | **yes** | yes |
| post-replace | PostToolUse | `updatedToolOutput` (control) | block | 1 | | no | no | no |
| post-exit1 | PostToolUse | exit 1 | block | 1 | | yes | **yes** | yes |
| post-kill9 | PostToolUse | `kill -9` itself | block | 1 | | yes | **yes** | yes |
| post-hang | PostToolUse | hangs past a 5 s timeout | block | 1 | | yes | **yes** | yes |
| post-missing | PostToolUse | binary missing | block | 0 | | yes | **yes** | n/a |
| batch-ok | PostToolBatch | none (control) | block | 1 | | yes | **yes** | yes |
| batch-stop | PostToolBatch | a JSON stop (control) | block | 1 | | yes | no | yes |
| batch-exit1 | PostToolBatch | exit 1 | block | 1 | | yes | no | yes |
| batch-kill9 | PostToolBatch | `kill -9` itself | block | 1 | | yes | no | yes |
| batch-hang | PostToolBatch | hangs past a 5 s timeout | block | 1 | | yes | no | yes |
| batch-missing | PostToolBatch | binary missing | block | 0 | | yes | no | n/a |
| combo-post-kill9 | PostToolUse, then PostToolBatch | PostToolUse `kill -9`; PostToolBatch stops | block | 2 | | yes | no | yes |

Three more runs recorded SessionStart for `startup`, `resume` and `fork`. Sanitized payloads for every event above are in [adapters/claude-code/payloads/](../adapters/claude-code/payloads/).

## Not tested yet, and why

| Test | Needs |
|---|---|
| Managed hooks under `claude --bare` and `claude --settings` (decision new-18) | a managed-settings drop-in, so `sudo`: Xavier. The `--bare` help on 2.1.321 says it skips "hooks (those defined in settings and by installed plugins)" and never reads OAuth or the keychain. With no `ANTHROPIC_API_KEY` set on this Mac, a nested `claude --bare` cannot log in, but that is a mitigation, not a control. The docs also say `--bare` "will become the default for `-p` in a future release": re-test on every upgrade |
| Whether managed hooks wait for folder trust | the same drop-in. The hooks page says an interactive session holds back hooks "from every settings file" until trust is accepted, and does not say whether that includes managed settings |
| What an out-of-range managed version shows | the same drop-in |
| The sandbox keys | reading the sandboxing page against a managed file |
| The wire test (phi.md §11, test 13) | a local recording proxy. Not run: the proxy would see the login token in transit, and only Xavier handles that token |
| UserPromptExpansion, PreCompact, ConfigChange payloads | interactive sessions |
| The same matrix in an interactive session | interactive sessions (the `-p` block message may differ) |

## How to re-run

```bash
day0/run.sh /tmp/day0-out              # every case, about 6 minutes
day0/run.sh /tmp/day0-out pre- batch-  # only cases whose names start so
```

It uses the subscription login of the terminal `claude` and costs a few cents of Haiku per run.

## heddle end to end, 2026-11-13

`heddle-e2e.sh` runs the CI-built heddle (`heddle-macos` job, test-only artifact) as every PHI hook line, in exec form with `onFailure: "block"` and a 10 s timeout. It does this in a scratch repo that heddle treats as enrolled, under real `claude -p` sessions on Claude Code 2.1.321. The fixture number is assembled at run time. Afterwards the script deletes the label entries its sessions created.

| case | what happened | label file afterwards |
|---|---|---|
| clean | `touch` ran; heddle printed nothing | none |
| prompt-held | a typed prompt with an SSN was blocked before sending: "Fabric held this prompt … (rule R-01). It was not sent." | `held` |
| result-replaced | `echo` printed the number; PostToolUse replaced the Bash result with the withheld sentence (R-09), and **the number appears nowhere in the session's stream** | `exposed … result_masked` |
| brake-persists | the same conversation resumed: the next prompt was blocked with the brake sentence and its incident id | unchanged |
| tool-input | the model wrote the number into a command; PreToolUse denied it | `exposed … tool_input` |
| unreadable-label | a label file in an unknown format was read as braked (V5); the first prompt was blocked | `exposed … found_later` |
| bad-argument | a hook line with an unknown harness argument stopped the session under R-00 | none |

Not covered yet: forks (T3 needs the root-uuid match), subagents' prompts, the transcript scan, and the room.

## T1: the shipped wiring, 2026-11-14

`run.sh` now lays each case out as setup will: the hook lines sit in the **main checkout's** `.claude/settings.local.json`, and every session runs in a **linked worktree** of it with `--setting-sources local`. Claude Code 2.1.321.

**All 36 cases reproduce the table above, and the fork run records `startup`, `resume` and `fork` again.** Each hook fired once (twice for the combination case; never for the five missing-binary cases), and every payload's `cwd` is the worktree. So one local settings file serves every worktree of a repo, and nothing is committed.

## heddle end to end on the ported hook, 2026-11-14

`heddle-e2e.sh` now wires a local test build of heddle the same way (`--root <main checkout> --state <scratch folder>`, sessions in a linked worktree), so nothing outside the output folder is touched. The hook reads JSON with fabric-core's own reader; heddle has no third-party crates. Decision 3 applies: withheld results let the conversation go on, and the first patient-shaped tool input gets a hint.

| case | what happened | label file afterwards |
|---|---|---|
| clean | `touch` ran; heddle printed nothing | none |
| prompt-held | a typed prompt with an SSN was held before sending (R-01) | `held` |
| result-withheld | `cat` of a scratch file holding a random number in the unissued 987 area: the Bash result was replaced with the withheld sentence (R-09), the model answered from the sentence, and **the number is nowhere in the stream** | `clean`, withheld 1 |
| three-withheld | three such `cat` calls in one turn: each result replaced, and PostToolBatch stopped the turn after the third | `clean`, withheld 3 |
| tool-input-hint | the model wrote the number into a `touch` command: denied with the made-up-data hint, **no file created**, the conversation went on | `clean`, strikes 1 |
| tool-input-second | the same with one strike already recorded: denied and stopped (`continue: false`), no file created | `exposed … tool_input` |
| brake-persists | that conversation resumed: the next prompt was blocked with the pause sentence and its reference | unchanged |
| unreadable-label | a label file in an unknown format was read as paused (V5); the first prompt was blocked | `exposed … found_later` |
| missing-state | `--state` names a folder that does not exist: the first prompt was blocked with the hiccup sentence (R-00); no label could be written | none |
| bad-argument | an unknown harness argument stopped the session (R-00) | none |

Three facts about Claude Code 2.1.321 that these runs settle:

- **PostToolBatch carries the replacement.** After PostToolUse replaced a Bash result, the batch's `tool_response` was the replacement: the batch scan found nothing in it.
- **PostToolBatch lists denied calls, with their input.** A first-strike denial was turned into a pause by the batch scan reading the denied command, until the batch scan was narrowed to results (PreToolUse already scanned every input, and the model wrote it).
- **A JSON prompt block without `suppressOriginalPrompt` echoes the prompt** in `-p` mode ("Original prompt: …"). heddle sets it only on a detector hit, so a hiccup leaves the prompt there to copy.

## More Saturday tests, 2026-11-14

| test | result |
|---|---|
| **T2** (`t2.sh`) | **Passes.** In `bypassPermissions` mode a typed prompt with a fixture is still held, and a second patient-shaped tool input still pauses, with no file created. From a linked worktree, all eight ways to change each protected path are refused, 24 of 24: the Edit and Write tools, `sed -i`, `tee`, a `>` redirect, `cp`, `mv` and `rm`. That covers the two home-anchored rules (`~/…`, one with a space in its path like the real state folder) and the `//`-anchored rule on the main checkout's settings file. **Claude Code applies `Edit(…)` deny rules to Bash commands that name a protected path.** It even refused a `cp` that named the path only as a source. All eight operations work on an unprotected control file. How a block looks in an interactive bypass session waits for the interactive harness |
| **T5** (`run.sh t11`) | **`/clear` starts a new conversation.** Sent as a prompt in `-p` mode, it gave a new `session_id` and a SessionStart with `source: "clear"`; a later `--resume` of the old id continued the old conversation. So "start a new session" may name `/clear` as one way |
| **T11** (`run.sh t11`) | **UserPromptExpansion carries the typed command, not the expansion.** For a custom command its payload holds `prompt: "/hello"`, `command_name`, `command_args`, `command_source` (`projectSettings`) and `expansion_type`. UserPromptSubmit sees the same typed text first, so this line adds no text to scan. The expanded text comes from the command file: the dry scan covers a project's commands, and user and plugin commands are a gap. **PreCompact** fires for `/compact` with `trigger: "manual"` and `custom_instructions`, and is followed by a SessionStart with `source: "compact"`. The built-in commands pass no UserPromptSubmit. The four payloads are in `adapters/claude-code/payloads/` |
| **T6** (`run.sh t6`) | **A ConfigChange block holds, and without one a session can unhook itself.** In bypass mode a session stripped its own PreToolUse line from the main checkout's local settings, then ran one more command. With a non-blocking ConfigChange line, the change applied to the running session at once and the second command ran unhooked. With a JSON block from ConfigChange, the change was refused for the running session and both commands were hooked. The payload carries `source: "local_settings"` and `file_path` (recorded). So hook line 9 is viable and needed: it should refuse any change that drops heddle's lines. It also means running sessions pick up local-settings changes without a restart (part of T9) |
| **T7**, first part (`day0/cost.py`) | **Passes.** 1,000 runs of each hook line on the recorded payloads: 95th percentile **1.84 ms**, wall clock with process start, against a 30 ms gate. The cost grows about 29 µs per KiB scanned: a 32 KiB Bash result (about Claude Code's own cap on Bash output) takes 2.7 ms, 256 KiB 9.3 ms, 1 MiB 31 ms. The transcript scan, a 60-second lock holder and eight sessions come with the features they measure |
| **T13**, the scriptable half (`heddle-e2e.sh`, `t13`) | `/cd` and `/branch` sent in `-p` mode get "isn't available in this environment", so T13 proper needs an interactive session. Its stand-in for `/branch`, `--fork-session`, gives a paused conversation's fork **a new session id with no label, so every hook passes.** In the run the fork did not act only because the parent's injected pause notice was in the context it inherited. That is the model's cooperation, not enforcement; fork matching closes it |
| **T14**, at session start (`heddle-e2e.sh`, `project-off`) | **Passes.** A project `.claude/settings.json` carrying `"disableAllHooks": true` and an `env` that moves `HOME` did not switch the brake off: the local block's `"disableAllHooks": false` outranks it, and heddle finds its state by `--state`, not `HOME`. **The control proves the line is needed:** without it, the same project file switched every hook off and the blocked action ran (`project-off-control`). The live half (a pull into a running session) needs an interactive session |
| **Count mode**, decision 4 (`heddle-e2e.sh`, `count-only`) | **Works.** The same patient-shaped `touch` on lines carrying `--mode count` and no `onFailure`: the command ran, heddle printed nothing, the state folder's `counts` file logged `count PreToolUse R-09` with its time, and no label was written |
| **T15** (`run.sh t15`) | **A broken local settings file switches the brake off, silently.** Broken before the session starts (truncated JSON): no hook fired, the work ran, and nothing was printed. Broken by the session itself between two commands: the running session lost its hooks for the second command. With a ConfigChange line that blocks, the broken file was refused and both commands stayed hooked. So hook line 9 must also refuse a file that does not parse, and `doctor` and the tripwire must catch a file broken while no session runs |
| **T16** (`heddle-e2e.sh`, `t16`) | **A held prompt does not come back.** A typed prompt carrying a fixture and a random token was held. After a clean rewrite, a resume and a `/compact`, the model answered NONE when asked for any `TOKEN-` string, and the token is in no stream. In `-p` mode Held needs no chain proof. Interactive mode is still to check (T21) |
| **T19** (`run.sh t19`) | **A Bash call can learn its conversation, both ways.** A SessionStart hook that appends `export FABRIC_SEAT=<session id>` to `$CLAUDE_ENV_FILE` reaches later Bash calls, and so does a PreToolUse `updatedInput` that prefixes the command. Outside bypass mode Claude Code refuses an unallowed command that *expands* a variable ("can't be checked before it runs"). So the verbs read the id from their environment and never name it in the command. Seats per conversation work |
| **T24** (`heddle-e2e.sh`) | **Passes.** Every e2e session runs in a worktree outside the main checkout's folder, and its hits pause it (`tool-input-second`). After `cd /tmp`, Claude Code 2.1.321's Bash tool refused to leave the session's working directory, and the next call's result was withheld as usual (`after-cd`). heddle does not read `cwd` at all |
