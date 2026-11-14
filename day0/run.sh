#!/bin/bash
# Day-0 hook tests. Drives real `claude -p` sessions against probe.sh, one
# scratch repo per case, and records what Claude Code does with each hook
# outcome. The layout is the shipped wiring (test T1): the hook lines sit in
# the main checkout's .claude/settings.local.json, and every session runs in a
# linked worktree with --setting-sources local. Evidence is physical, never
# the model's say-so:
#   - PreToolUse and UserPromptSubmit cases ask for `touch marker`: did the file appear?
#   - PostToolUse and PostToolBatch cases ask for `cat secret.txt`, a random token
#     the model can only learn from the tool result: did it reach the reply,
#     the tool_result in the stream, or the transcript on disk?
# Usage: day0/run.sh OUT_DIR [CASE_NAME_PREFIX...]   (bash 3.2 compatible)
set -u
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: run.sh OUT_DIR [CASE_NAME_PREFIX...]}
shift
mkdir -p "$out"
probe="$here/probe.sh"
claude_bin=${CLAUDE_BIN:-$HOME/.local/bin/claude}
model=${PROBE_MODEL:-haiku}

# A terminal-like environment: none of the Desktop app's variables leak in.
cleanenv=(env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin"
  USER="$USER" LOGNAME="${LOGNAME:-$USER}" TMPDIR="${TMPDIR:-/tmp}" SHELL=/bin/zsh TERM=dumb)

# name event mode onFailure timeout kind
cases="
pre-ok            PreToolUse       ok        block 10 touch
pre-exit1         PreToolUse       exit1     block 10 touch
pre-exit2         PreToolUse       exit2     block 10 touch
pre-exit101       PreToolUse       exit101   block 10 touch
pre-hang          PreToolUse       hang      block 5  touch
pre-watchdog      PreToolUse       watchdog  block 10 touch
pre-kill9         PreToolUse       kill9     block 10 touch
pre-missing       PreToolUse       missing   block 10 touch
pre-plaintext     PreToolUse       plaintext block 10 touch
pre-badjson       PreToolUse       badjson   block 10 touch
pre-partial       PreToolUse       partial   block 10 touch
pre-deny          PreToolUse       deny      block 10 touch
pre-exit1-unset   PreToolUse       exit1     unset 10 touch
pre-exit101-unset PreToolUse       exit101   unset 10 touch
pre-hang-unset    PreToolUse       hang      unset 5  touch
pre-kill9-unset   PreToolUse       kill9     unset 10 touch
pre-missing-unset PreToolUse       missing   unset 10 touch
ups-ok            UserPromptSubmit ok        block 10 touch
ups-exit1         UserPromptSubmit exit1     block 10 touch
ups-hang          UserPromptSubmit hang      block 5  touch
ups-kill9         UserPromptSubmit kill9     block 10 touch
ups-missing       UserPromptSubmit missing   block 10 touch
ups-block         UserPromptSubmit blockprompt block 10 touch
post-ok           PostToolUse      ok        block 10 cat
post-replace      PostToolUse      replace   block 10 cat
post-exit1        PostToolUse      exit1     block 10 cat
post-kill9        PostToolUse      kill9     block 10 cat
post-hang         PostToolUse      hang      block 5  cat
post-missing      PostToolUse      missing   block 10 cat
batch-ok          PostToolBatch    ok        block 10 cat
batch-stop        PostToolBatch    stoploop  block 10 cat
batch-exit1       PostToolBatch    exit1     block 10 cat
batch-kill9       PostToolBatch    kill9     block 10 cat
batch-hang        PostToolBatch    hang      block 5  cat
batch-missing     PostToolBatch    missing   block 10 cat
combo-post-kill9  PostToolUse+PostToolBatch kill9+stoploop block 10 cat
"

# The settings for one hook line, or several joined by "+" (EVENT and MODE in step).
hook_json() {
  case $1 in
    *+*) local evs=$1 modes=$2 acc='{"hooks":{}}' e m
         while [ -n "$evs" ]; do
           e=${evs%%+*}; m=${modes%%+*}
           acc=$(jq -s '.[0] * .[1]' <(printf '%s' "$acc") <(hook_json "$e" "$m" "$3" "$4"))
           case $evs in *+*) evs=${evs#*+}; modes=${modes#*+} ;; *) evs= ;; esac
         done
         printf '%s\n' "$acc"; return ;;
  esac
  jq -n --arg ev "$1" --arg mode "$2" --arg onfail "$3" --argjson to "$4" --arg probe "$probe" '
    ( (if $mode == "missing" then {type: "command", command: "/nonexistent/heddle-probe", args: []}
       else {type: "command", command: "/bin/sh", args: [$probe, $ev, $mode]} end)
      + {timeout: $to}
      + (if $onfail == "block" then {onFailure: "block"} else {} end) ) as $h
    | {hooks: {($ev): [ (if $ev == "PreToolUse" or $ev == "PostToolUse" then {matcher: "Bash"} else {} end)
                        + {hooks: [$h]} ]}}'
}

yn() { if "$@"; then echo yes; else echo no; fi; }

# layout DIR: a main checkout DIR/repo with one commit and an empty
# .claude/, its linked worktree DIR/wt, and DIR/log for the probe.
layout() {
  rm -rf "$1"; mkdir -p "$1/log"
  git init -q "$1/repo"
  git -C "$1/repo" -c user.name=day0 -c user.email=day0@invalid commit -q --allow-empty -m init
  git -C "$1/repo" worktree add -q "$1/wt" 2>/dev/null
  mkdir -p "$1/repo/.claude"
}

run_case() {
  local name=$1 ev=$2 mode=$3 onfail=$4 to=$5 kind=$6
  local d="$out/$name"
  layout "$d"
  hook_json "$ev" "$mode" "$onfail" "$to" > "$d/repo/.claude/settings.local.json"
  local w="$d/wt"
  local token; token="TOKEN-$(openssl rand -hex 8)"
  printf '%s\n' "$token" > "$w/secret.txt"
  local prompt
  if [ "$kind" = touch ]; then
    prompt="Run this exact command with the Bash tool, once: touch $w/marker . If the call is blocked or denied, do not retry and do not try another way. Then reply with one word: DONE or BLOCKED."
  else
    prompt="Run this exact command with the Bash tool, once: cat $w/secret.txt . Then reply with exactly the text it printed and nothing else. If you saw no output, reply NONE."
  fi
  local t0=$SECONDS
  ( cd "$w" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "$prompt" \
      --model "$model" --output-format stream-json --verbose --include-hook-events \
      --setting-sources local --tools Bash --max-turns 4 --max-budget-usd 0.25 \
      --allowedTools "Bash(touch *)" "Bash(cat *)" ) \
    > "$d/stream.jsonl" 2> "$d/stderr.txt" < /dev/null
  local rc=$? secs=$((SECONDS - t0))
  local fired; fired=$(find "$d/log" -name '*.json' | wc -l | tr -d ' ')
  local tpath; tpath=$(cat "$d"/log/*.json 2>/dev/null | jq -r '.transcript_path? // empty' 2>/dev/null | head -1)
  local marker; marker=$(yn test -e "$w/marker")
  local in_reply in_result in_transcript
  in_reply=$(yn grep -q "$token" <(jq -r 'select(.type=="result") | .result? // empty' "$d/stream.jsonl" 2>/dev/null))
  in_result=$(yn grep -q "$token" <(jq -c 'select(.type=="user") | .message.content[]? | select(.type=="tool_result")' "$d/stream.jsonl" 2>/dev/null))
  in_transcript=$( [ -n "$tpath" ] && [ -f "$tpath" ] && yn grep -q "$token" "$tpath" || echo n/a)
  local subtype turns; subtype=$(jq -r 'select(.type=="result") | .subtype' "$d/stream.jsonl" 2>/dev/null | tail -1)
  turns=$(jq -r 'select(.type=="result") | .num_turns' "$d/stream.jsonl" 2>/dev/null | tail -1)
  # The harness's own notice about the hook, as the model or the user saw it.
  local notice; notice=$(jq -r '
      (select(.type=="user") | .message.content[]? | select(.type=="tool_result") | .content | if type=="array" then (map(.text? // "") | join(" ")) else . end),
      (select(.type=="result") | .result? // empty),
      (select(.type=="system" and ((.subtype? // "") | test("hook"))) | (.outcome? // .output? // .stderr? // empty) | tostring)
    ' "$d/stream.jsonl" 2>/dev/null | grep -i -m1 -E 'hook|block|denied|timed out|failed' | tr '\t\n' '  ' | cut -c1-220)
  [ -z "$notice" ] && notice=$(grep -i -m1 -E 'hook|block|error' "$d/stderr.txt" | tr '\t\n' '  ' | cut -c1-220)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$ev" "$mode" "$onfail" "$rc" "$secs" "$fired" "$marker" "$in_result" "$in_reply" \
    "$in_transcript" "${subtype:-none}" "${turns:-?}" "$notice" >> "$out/summary.tsv"
  echo "done: $name rc=$rc marker=$marker result=$in_result reply=$in_reply transcript=$in_transcript"
}

# SessionStart payloads for startup, resume and fork (decision C19, M12 rows).
run_fork() {
  local d="$out/fork"
  layout "$d"
  jq -n --arg probe "$probe" '{hooks: {
      SessionStart: [{hooks: [{type: "command", command: "/bin/sh", args: [$probe, "SessionStart", "ok"], timeout: 10}]}],
      UserPromptSubmit: [{hooks: [{type: "command", command: "/bin/sh", args: [$probe, "UserPromptSubmit", "ok"], timeout: 10}]}]}}' \
    > "$d/repo/.claude/settings.local.json"
  local id; id=$(uuidgen | tr 'A-Z' 'a-z')
  local common=(--model "$model" --output-format stream-json --verbose --setting-sources local --tools "" --max-turns 1 --max-budget-usd 0.10)
  ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "Reply with the word one." --session-id "$id" "${common[@]}" ) > "$d/1.jsonl" 2>&1 < /dev/null
  ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "Reply with the word two." --resume "$id" "${common[@]}" ) > "$d/2.jsonl" 2>&1 < /dev/null
  ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "Reply with the word three." --resume "$id" --fork-session "${common[@]}" ) > "$d/3.jsonl" 2>&1 < /dev/null
  echo "$id" > "$d/first_session_id"
  echo "done: fork (payloads in $d/log)"
}

# T11 and T5: a custom slash command, then /compact and /clear, sent in -p
# mode on one conversation. Records the UserPromptExpansion, PreCompact and
# SessionStart payloads, and whether /clear starts a new session id.
run_t11() {
  local d="$out/t11"
  layout "$d"
  local ev; local hooks='{}'
  for ev in SessionStart UserPromptSubmit UserPromptExpansion PreCompact; do
    hooks=$(jq -c --arg ev "$ev" --arg probe "$probe" '. + {($ev): [{hooks: [{type: "command", command: "/bin/sh", args: [$probe, $ev, "ok"], timeout: 10}]}]}' <<< "$hooks")
  done
  jq -n --argjson h "$hooks" '{hooks: $h}' > "$d/repo/.claude/settings.local.json"
  local c; for c in "$d/repo" "$d/wt"; do
    mkdir -p "$c/.claude/commands" && printf 'Reply with the single word expanded.\n' > "$c/.claude/commands/hello.md"
  done
  local id; id=$(uuidgen | tr 'A-Z' 'a-z')
  local common=(--model "$model" --output-format stream-json --verbose --setting-sources project,local --tools "" --max-turns 2 --max-budget-usd 0.10)
  ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "Reply with the word one." --session-id "$id" "${common[@]}" ) > "$d/1.jsonl" 2>&1 < /dev/null
  local p n=2; for p in "/hello" "/compact" "/clear" "Reply with the word last."; do
    ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p "$p" --resume "$id" "${common[@]}" ) > "$d/$n.jsonl" 2>&1 < /dev/null
    n=$((n + 1))
  done
  echo "$id" > "$d/first_session_id"
  echo "done: t11 (payloads in $d/log)"
}

# T6: the session strips its own PreToolUse line from the local settings file
# mid-session, then runs one more command. CONFIG_MODE is the ConfigChange
# probe's mode: ok (record the payload) or stoploop (a JSON block). If the
# block holds, the PreToolUse probe logs both commands.
run_t6() {
  local mode=$1 d="$out/t6-$1"
  layout "$d"
  local f="$d/repo/.claude/settings.local.json"
  jq -n --arg probe "$probe" --arg mode "$mode" '{hooks: {
      ConfigChange: [{hooks: [{type: "command", command: "/bin/sh", args: [$probe, "ConfigChange", $mode], timeout: 10, onFailure: "block"}]}],
      PreToolUse: [{matcher: "Bash", hooks: [{type: "command", command: "/bin/sh", args: [$probe, "PreToolUse", "ok"], timeout: 10}]}]}}' > "$f"
  local strip="jq 'del(.hooks.PreToolUse)' '$f' > '$f.tmp' && mv '$f.tmp' '$f' && sleep 3"
  ( cd "$d/wt" && "${cleanenv[@]}" PROBE_LOG="$d/log" "$claude_bin" -p \
      "Run this exact Bash command, once: $strip . Then, as a second Bash call, run: touch $d/wt/marker-after . Then reply DONE." \
      --model "$model" --output-format stream-json --verbose --setting-sources local --permission-mode bypassPermissions \
      --tools Bash --max-turns 4 --max-budget-usd 0.10 ) > "$d/stream.jsonl" 2> "$d/stderr.txt" < /dev/null
  local pre; pre=$(ls "$d/log" | grep -c '^PreToolUse')
  local cc; cc=$(ls "$d/log" | grep -c '^ConfigChange')
  echo "done: t6-$mode ConfigChange fired $cc time(s); PreToolUse logged $pre of 2 commands; marker-after $(yn test -e "$d/wt/marker-after")"
}

# want NAME [PREFIX...]: no prefixes means every case.
want() {
  local name=$1; shift
  [ $# -eq 0 ] && return 0
  local p; for p in "$@"; do case $name in "$p"*) return 0 ;; esac; done
  return 1
}

"$claude_bin" --version > "$out/version.txt" 2>&1
[ -f "$out/summary.tsv" ] || printf 'case\tevent\tmode\tonFailure\trc\tsecs\tfired\tmarker\ttoken_in_tool_result\ttoken_in_reply\ttoken_in_transcript\tsubtype\tturns\tnotice\n' > "$out/summary.tsv"
echo "$cases" | while read -r name ev mode onfail to kind; do
  [ -z "${name:-}" ] && continue
  want "$name" "$@" && run_case "$name" "$ev" "$mode" "$onfail" "$to" "$kind"
done
want fork "$@" && run_fork
want t11 "$@" && run_t11
want t6 "$@" && { run_t6 ok; run_t6 stoploop; }
