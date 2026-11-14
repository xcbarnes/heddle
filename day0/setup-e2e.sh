#!/bin/bash
# setup, end to end. `heddle setup` in a scratch repo, run from a linked
# worktree with HOME pointed at a scratch folder, so nothing real is
# installed. Then real `claude -p` sessions (with the real HOME, to log in;
# the hook lines carry absolute paths, so they do not care): the live check,
# and hook line 9 against a script that strips a hook line (deny rules do not
# cover scripts). Then doctor, setup again, and uninstall.
#
# Usage: day0/setup-e2e.sh HEDDLE_BINARY OUT_DIR   (bash 3.2 compatible)
set -u
bin=${1:?usage: setup-e2e.sh HEDDLE_BINARY OUT_DIR}
out=${2:?usage: setup-e2e.sh HEDDLE_BINARY OUT_DIR}
claude_bin=${CLAUDE_BIN:-$HOME/.local/bin/claude}
model=${PROBE_MODEL:-haiku}
rm -rf "$out"
mkdir -p "$out/home" "$out/runs" "$out/dl"
out=$(cd "$out" && pwd -P)
home="$out/home"
repo="$out/repo"
wt="$out/wt"
git init -q "$repo"
printf 'A project with nothing patient-shaped in it.\n' > "$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" -c user.name=day0 -c user.email=day0@invalid commit -q -m init
git -C "$repo" worktree add -q "$wt" 2>/dev/null
cp "$bin" "$out/dl/heddle" # stands in for the downloaded release

installed="$home/.local/bin/heddle"
fabric="$home/Library/Application Support/fabric"
settings="$repo/.claude/settings.local.json"
# h DIR CMD...: a heddle command as a person runs it, with the scratch HOME.
h() { local d=$1; shift; (cd "$d" && env -i HOME="$home" PATH=/usr/bin:/bin "$@"); }
cleanenv=(env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin"
  USER="$USER" LOGNAME="${LOGNAME:-$USER}" TMPDIR="${TMPDIR:-/tmp}" SHELL=/bin/zsh TERM=dumb)
session() { # NAME PROMPT: bypass mode, so only heddle and Claude Code's own rules decide
  ( cd "$wt" && "${cleanenv[@]}" "$claude_bin" -p "$2" --model "$model" --output-format stream-json --verbose \
      --include-hook-events --setting-sources local --permission-mode bypassPermissions --tools Bash \
      --max-turns 4 --max-budget-usd 0.15 ) > "$out/runs/$1.jsonl" 2> "$out/runs/$1.err" < /dev/null
}
hook_runs() { # NAME EVENT: how many times that hook line ran in the session
  jq -r --arg e "$2" 'select(.type=="system" and .subtype=="hook_response" and .hook_event==$e) | .hook_event' \
    "$out/runs/$1.jsonl" | wc -l | tr -d ' '
}
yn() { if "$@"; then echo yes; else echo no; fi; }

echo "claude: $("$claude_bin" --version)"
echo "== setup, from the worktree"
h "$wt" "$out/dl/heddle" setup; echo "  exit $?"
echo "  installed binary runs: $(yn h "$wt" "$installed" version)"
echo "  kept copy, receipt, REPAIR.txt: $(yn test -x "$fabric/kept/heddle") $(yn test -f "$fabric/receipt") $(yn test -f "$fabric/REPAIR.txt")"
echo "  heddle lines in the main checkout's settings: $(jq '[.hooks[][] | .hooks[] | select(.command | endswith("/heddle"))] | length' "$settings")"
echo "  git ignores the settings file: $(yn git -C "$wt" check-ignore -q .claude/settings.local.json)"

echo "== doctor before the live check (expect: not run yet, exit 1)"
h "$wt" "$installed" doctor | sed 's/^/  /'; echo "  exit ${PIPESTATUS[0]}"

echo "== the live check: an agent runs doctor --canary"
session canary "Run exactly this Bash command, once, and nothing else: \`$installed doctor --canary\` Then reply DONE or BLOCKED."
echo "  canary stamped by the hook: $(yn test -f "$fabric/machine/canary"); canary ran: $(yn test -f "$fabric/machine/canary-ran")"
echo "  reply: $(jq -r 'select(.type=="result") | .result // ""' "$out/runs/canary.jsonl" | tr '\n' ' ' | cut -c1-150)"
h "$wt" "$installed" doctor | sed 's/^/  /'; echo "  doctor exit ${PIPESTATUS[0]}"

echo "== hook line 9: a script strips the PreToolUse line mid-session"
cat > "$wt/strip.py" <<EOF
import json
p = "$settings"
d = json.load(open(p))
d["hooks"].pop("PreToolUse")
json.dump(d, open(p, "w"))
EOF
session line9 "Run exactly this Bash command, once, and nothing else: \`/usr/bin/python3 strip.py && sleep 3\` Then, as a second Bash call: \`touch $wt/marker-after\` Then reply DONE."
echo "  PreToolUse ran $(hook_runs line9 PreToolUse) of 2 times (stream-json does not report ConfigChange hooks)"
echo "  line 9 left its trace (config-refused): $(yn test -f "$fabric/machine/config-refused"); marker-after: $(yn test -e "$wt/marker-after")"
echo "== doctor sees the stripped file (expect BROKEN, exit 1), then setup restores it"
h "$wt" "$installed" doctor | grep -i wiring | sed 's/^/  /'
h "$wt" "$installed" setup > /dev/null; echo "  setup again: exit $?"
h "$wt" "$installed" doctor | grep -i -E 'wiring|live' | sed 's/^/  /'

echo "== repair: the installed binary is deleted; the kept copy puts it back"
rm -f "$installed"
h "$wt" "$fabric/kept/heddle" repair | sed 's/^/  /'
echo "  installed binary back and runs: $(yn h "$wt" "$installed" version)"
h "$wt" "$installed" doctor | grep -i -E '^binary|kept' | sed 's/^/  /'
echo "  repair from anything but the kept copy refuses: $(h "$wt" "$installed" repair 2>&1 | cut -c1-90)"

echo "== uninstall"
h "$wt" "$installed" uninstall | sed 's/^/  /'
echo "  binary gone: $(yn test ! -e "$installed"); state gone: $(yn test ! -e "$fabric")"
echo "  heddle lines left in the settings: $(jq '[.hooks[]?[]? | .hooks[]? | select(.command | endswith("/heddle"))] | length' "$settings" 2>/dev/null || echo n/a)"
echo "  settings file now: $(tr -d ' \n' < "$settings" | cut -c1-120)"
