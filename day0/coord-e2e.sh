#!/bin/bash
# Coordination, end to end, on a scratch repo with a scratch HOME (as in
# setup-e2e.sh), so nothing real is touched. First, heddle processes race
# for one key from the main checkout and a linked worktree. Then real
# `claude -p` sessions (with the real HOME, to log in) work the room beside
# a made-up conversation X that the script drives with the verbs and one
# hook payload: the digest, FABRIC_SEAT reaching Bash, the verbs running
# without a prompt, a claim X holds, the overlap notice across worktrees,
# news at the prompt, a push held while X's stop holds, and SessionEnd
# releasing a session's claims.
#
# Usage: day0/coord-e2e.sh HEDDLE_BINARY OUT_DIR   (bash 3.2 compatible)
# RACE_ROUNDS (default 1000) sets the race's rounds; SKIP_SESSIONS=1 stops
# after the race.
set -u
bin=${1:?usage: coord-e2e.sh HEDDLE_BINARY OUT_DIR}
out=${2:?usage: coord-e2e.sh HEDDLE_BINARY OUT_DIR}
rounds=${RACE_ROUNDS:-1000}
claude_bin=${CLAUDE_BIN:-$HOME/.local/bin/claude}
model=${PROBE_MODEL:-haiku}
rm -rf "$out"
mkdir -p "$out/home" "$out/runs" "$out/dl" "$out/race"
out=$(cd "$out" && pwd -P)
home="$out/home"
repo="$out/repo"
wt="$out/wt"
g() { local d=$1; shift; git -C "$d" -c user.name=day0 -c user.email=day0@invalid "$@"; }
git init -q -b main "$repo"
printf 'A project with nothing patient-shaped in it.\n' > "$repo/README.md"
g "$repo" add README.md
g "$repo" commit -q -m init
git init -q --bare "$out/origin.git"
g "$repo" remote add origin "$out/origin.git"
g "$repo" push -q origin main
g "$repo" worktree add -q -b side "$wt" 2>/dev/null
cp "$bin" "$out/dl/heddle" # stands in for the downloaded release

installed="$home/.local/bin/heddle"
state="$home/Library/Application Support/fabric/machine"
room="$repo/.git/fabric/room"
# h DIR CMD...: a command as a person runs it, with the scratch HOME.
h() { local d=$1; shift; (cd "$d" && env -i HOME="$home" PATH=/usr/bin:/bin "$@"); }
conv() { printf '%08x-19fa-41d2-8238-13269b9b3ca0' "$1"; }
# as DIR N VERB...: a verb run by made-up conversation N.
as() { local d=$1 n=$2; shift 2; h "$d" FABRIC_SEAT="$(conv "$n")" "$installed" "$@"; }
yn() { if "$@"; then echo yes; else echo no; fi; }
has() { case $1 in *"$2"*) return 0 ;; *) return 1 ;; esac; }

echo "claude: $("$claude_bin" --version)"
h "$wt" "$out/dl/heddle" setup > "$out/setup.txt"
echo "setup, from the worktree: exit $?"

echo "== the race: $rounds rounds of 8 processes claiming one key, 4 from each checkout"
start=$(date +%s)
bad=0
busy=0
for ((i = 1; i <= rounds; i++)); do
  for n in 1 2 3 4 5 6 7 8; do
    if [ "$n" -le 4 ]; then d=$repo; else d=$wt; fi
    as "$d" "$n" claim race > "$out/race/$n" 2>&1 &
  done
  wait
  won=$(grep -l '^You hold race' "$out/race/"[1-8] | wc -l | tr -d ' ')
  busy=$((busy + $(cat "$out/race/"[1-8] | grep -c 'busy')))
  if [ "$won" != 1 ]; then
    bad=$((bad + 1))
    echo "  round $i: $won winners: $(cat "$out/race/"[1-8] | sort | uniq -c | tr '\n' ' ' | cut -c1-200)"
  fi
  for f in $(grep -l '^You hold race' "$out/race/"[1-8]); do
    as "$repo" "${f##*/}" release race > /dev/null
  done
done
echo "  rounds without exactly one winner: $bad of $rounds; busy replies: $busy; $(($(date +%s) - start)) s"
echo "  a loser's reply: $(grep -h -v '^You hold' "$out/race/"[1-8] | head -1)"
[ "${SKIP_SESSIONS:-}" = 1 ] && exit 0

cleanenv=(env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin"
  USER="$USER" LOGNAME="${LOGNAME:-$USER}" TMPDIR="${TMPDIR:-/tmp}" SHELL=/bin/zsh TERM=dumb)
session() { # DIR NAME MODE TOOLS PROMPT
  (cd "$1" && "${cleanenv[@]}" "$claude_bin" -p "$5" --model "$model" --output-format stream-json --verbose \
    --include-hook-events --setting-sources local --permission-mode "$3" --tools "$4" \
    --max-turns 8 --max-budget-usd 0.25) > "$out/runs/$2.jsonl" 2> "$out/runs/$2.err" < /dev/null
}
said() { # NAME EVENT: what that event's hook lines printed
  jq -r --arg e "$2" 'select(.type=="system" and .subtype=="hook_response" and .hook_event==$e) | .stdout' \
    "$out/runs/$1.jsonl" | tr '\n' ' '
}
results() { # NAME: the session's tool results
  jq -r 'select(.type=="user") | .message.content[]? | select(.type=="tool_result")
    | .content | if type=="array" then map(.text? // "") | join(" ") else . end' "$out/runs/$1.jsonl" | tr '\n' ' '
}
denials() { jq -r 'select(.type=="result") | .permission_denials | length' "$out/runs/$1.jsonl"; }
seat() {
  jq -r 'select(.type=="system" and .subtype=="init") | .session_id' "$out/runs/$1.jsonl" | head -1 |
    tr -d '-' | cut -c1-8 | sed 's/^/s-/'
}
check() { # LABEL TEXT NEEDLE
  if has "$2" "$3"; then echo "  $1: yes"; else echo "  $1: NO (wanted: $3)"; fi
}
claude_verb() { printf 'Run exactly this Bash command, once: `%s %s`' "$installed" "$1"; }

mkdir -p "$repo/fabric/tasks"
printf 'status: ready\n\nWrite the tests.\n' > "$repo/fabric/tasks/write-tests.md"

echo "== A, in the main checkout, default permission mode: claims fix-login"
session "$repo" a default Bash "$(claude_verb 'claim fix-login') Then reply with its output, word for word."
a=$(seat a)
check "digest at SessionStart" "$(said a SessionStart)" "This one is $a."
check "digest lists ready work" "$(said a SessionStart)" "Ready work: write-tests."
check "the verb ran, knowing its conversation" "$(results a)" "You hold fix-login now."
echo "  permission denials: $(denials a)"
echo "  after its SessionEnd (expect no, no): fix-login still held: $(yn grep -q '^claim fix-login held' "$room"); its seat still in the room: $(yn grep -q "^seat $a " "$room")"

echo "== X, a made-up conversation: claims docs and edits README.md"
as "$repo" 170 claim docs | sed 's/^/  /'
printf '{"session_id":"%s","transcript_path":"%s/none.jsonl","cwd":"%s","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"%s/README.md","old_string":"A","new_string":"A"},"tool_response":{"filePath":"%s/README.md"}}' \
  "$(conv 170)" "$out" "$repo" "$repo" "$repo" |
  h "$repo" "$installed" hook claude-code PostToolUse --root "$repo" --state "$state" > /dev/null
echo "  X's footprint: $(grep "^path s-000000aa " "$room")"

echo "== B, in the worktree, acceptEdits: one session, two prompts, X acting between them"
mkfifo "$out/b.in"
(cd "$wt" && "${cleanenv[@]}" "$claude_bin" -p --input-format stream-json --model "$model" --output-format stream-json \
  --verbose --include-hook-events --setting-sources local --permission-mode acceptEdits --tools Bash,Read,Edit \
  --max-turns 8 --max-budget-usd 0.25) < "$out/b.in" > "$out/runs/b.jsonl" 2> "$out/runs/b.err" &
bpid=$!
exec 3> "$out/b.in"
send() { jq -nc --arg t "$1" '{type: "user", message: {role: "user", content: $t}}' >&3; }
turns() { local n; n=$(grep -c '"type":"result"' "$out/runs/b.jsonl" 2> /dev/null); echo "${n:-0}"; }
wait_turns() { local t=0; while [ "$(turns)" -lt "$1" ] && [ $t -lt 240 ]; do sleep 1; t=$((t + 1)); done; }
send "Do these steps in order, each once. 1. $(claude_verb 'claim docs') 2. $(claude_verb next) 3. Read README.md in this folder, then use the Edit tool to add the line \"Edited in the worktree.\" at its end. Then reply with the exact output of steps 1 and 2."
wait_turns 1
b=$(seat b)
check "digest shows X present" "$(said b SessionStart)" "s-000000aa"
check "digest shows docs in hand" "$(said b SessionStart)" "docs (s-000000aa)"
check "claim refused: X holds docs" "$(results b)" "s-000000aa holds docs."
check "next names the ready work" "$(results b)" "Ready work: write-tests."
check "overlap notice across worktrees" "$(said b PostToolUse)" "another conversation (s-000000aa) edited README.md"
echo "  waiting on its person after its turn (Stop): $(yn test "$(grep "^seat $b " "$room" | cut -d' ' -f4)" != -)"
as "$repo" 170 claim notes > /dev/null
printf 'Notes.\n' > "$repo/notes.md"
g "$repo" add notes.md
g "$repo" commit -q -m notes
g "$repo" push -q origin main
as "$repo" 170 done notes | sed 's/^/  X: /'
sleep 31 # the decisions watch looks at most every 30 seconds
send "Reply with the single word ok."
wait_turns 2
exec 3>&-
# With stream-json input, 2.1.321 stays up after its input closes; end it.
for ((t = 0; t < 20; t++)); do kill -0 "$bpid" 2> /dev/null || break; sleep 0.5; done
pkill -TERM -P "$bpid" 2> /dev/null
wait "$bpid"
check "news at the next prompt: done" "$(said b UserPromptSubmit)" "notes is done."
check "news at the next prompt: main moved" "$(said b UserPromptSubmit)" "Main moved to"
echo "  turns: $(turns); permission denials: $(denials b | tr '\n' ' ')"

echo "== C, in the main checkout, bypass mode: pushes while X's stop holds"
printf 'More.\n' >> "$repo/notes.md"
g "$repo" commit -q -am more
before=$(git -C "$out/origin.git" rev-parse main)
as "$repo" 170 stop | sed 's/^/  /'
session "$repo" c bypassPermissions Bash "Run exactly this Bash command, once: \`git push origin main\` Then reply with what happened, in one sentence."
check "the push was held" "$(said c PreToolUse)" "holding pushes for another"
echo "  origin's main unchanged: $(yn test "$(git -C "$out/origin.git" rev-parse main)" = "$before")"
as "$repo" 170 resume | sed 's/^/  /'

echo "== the room afterwards"
for s in a b c; do
  echo "  $s's seat left the room: $(yn test "$(grep -c "^seat $(seat "$s") " "$room")" = 0)"
done
h "$repo" "$installed" status | sed -n '1,4p' | sed 's/^/  /'
echo "  session costs (USD): $(for s in a b c; do jq -r 'select(.type=="result") | .total_cost_usd' "$out/runs/$s.jsonl"; done | tr '\n' ' ')"
