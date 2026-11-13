#!/bin/sh
# Day-0 stand-in for `heddle hook`. It logs the hook payload, then behaves as
# MODE says, so the tests can see what Claude Code does with each outcome.
# Exec form: "command": "/bin/sh", "args": ["probe.sh", EVENT, MODE].
# PROBE_LOG must name an existing folder.
ev=$1
mode=$2
dir=${PROBE_LOG:?PROBE_LOG is not set}
f="$dir/$ev.$mode.$$.json"

# heddle arms its watchdog before any read; so does this mode.
if [ "$mode" = watchdog ]; then
  trap 'echo "probe: watchdog fired" >&2; exit 3' USR1
  ( sleep 2; kill -USR1 $$ 2>/dev/null ) &
fi

cat > "$f"

case $mode in
  ok)       exit 0 ;;
  exit1)    echo "probe: exit 1" >&2; exit 1 ;;
  exit2)    echo "probe: exit 2" >&2; exit 2 ;;
  exit101)  echo "probe: exit 101" >&2; exit 101 ;;
  hang|watchdog) sleep 60 & wait $!; exit 0 ;;
  kill9)    kill -9 $$ ;;
  plaintext) printf '%s\n' '{"hookSpecificOutput": oops'; exit 0 ;;   # no closing brace: read as plain text
  badjson)  printf '%s\n' '{"hookSpecificOutput": oops}'; exit 0 ;;
  partial)  printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"de'; kill -9 $$ ;;
  deny)     printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"probe: deny"}}'; exit 0 ;;
  replace)  printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":{"stdout":"[probe replaced this output]","stderr":"","interrupted":false,"isImage":false}}}'; exit 0 ;;
  stoploop) printf '%s\n' '{"decision":"block","reason":"probe: loop stopped"}'; exit 0 ;;
  blockprompt) printf '%s\n' '{"decision":"block","reason":"probe: prompt held","hookSpecificOutput":{"hookEventName":"UserPromptSubmit","suppressOriginalPrompt":true}}'; exit 0 ;;
  *)        echo "probe: unknown mode" >&2; exit 1 ;;
esac
