#!/bin/bash
# A stand-in for Claude Code kept at work, for tui/tests/mid_turn_replies.rs.
# Installed as `claude`, so the daemon takes it for Claude Code: it names its
# conversation in $CLAUDE_CONFIG_DIR/sessions/<pid>.json, logs it under
# projects/ as Claude Code does (a prompt, tool calls and their results), and
# shows Claude Code's own screens (captured detection fixtures): working, a
# permission prompt, idle once it's done.
#
# It works one step every STEP_MS. What's typed while it works (a bracketed
# paste, then Enter) is queued; a step takes the oldest queued text once the
# test allows (control/allow: how many it may have taken by now) and logs it
# as Claude Code does once it takes it: a `queued_command` attachment, whose
# prompt is the text, or for a paste of several lines one paste block,
# <pasted_content id="N">…</pasted_content id="N">. Taking the text numbered
# STAND_IN_ASK_AFTER, its next tool call asks for permission until
# control/approve exists. Once control/finish exists and nothing is queued,
# it says STAND_IN_ANSWER and waits.
#
# For the test: control/events (`<ms> <event> [n]`), control/received.jsonl
# (each text typed in), control/input (every byte typed), and
# control/typed_while_asking (bytes typed while the permission prompt showed). Only builtins run in its loop, so it alone is
# the session's foreground.
set -u
export LC_ALL=C TZ=UTC

dir=$STAND_IN_DIR
ctl=$dir/control
conversation=$STAND_IN_CONVERSATION
log=$CLAUDE_CONFIG_DIR/projects/$STAND_IN_PROJECT/$conversation.jsonl
ask_after=$STAND_IN_ASK_AFTER
step_ms=1500
frame_ms=250

load() { IFS= read -r -d '' "$1" < "$dir/screens/$2" || true; }
load working_top working_top.txt
load working_bottom working_bottom.txt
load asking asking.txt
load idle idle.txt

esc=$'\e'
paste_open="$esc[200~"
paste_close="$esc[201~"

clock() {
  local t=${EPOCHREALTIME/./}
  now=$((10#$t / 1000))
}

# Now as Claude Code stamps its entries: UTC, milliseconds.
stamp() {
  local t=$EPOCHREALTIME
  local fraction=${t#*.}
  printf -v stamp '%(%Y-%m-%dT%H:%M:%S)T.%sZ' "${t%.*}" "${fraction:0:3}"
}

# $1 as a JSON string, in $json.
json() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  s=${s//$'\t'/\\t}
  json="\"$s\""
}

event() {
  clock
  printf '%s %s\n' "$now" "$*" >> "$ctl/events"
}

# One entry of the conversation: type $1, then the rest of its fields ($2).
uuids=0
parent=null
entry() {
  uuids=$((uuids + 1))
  local uuid
  printf -v uuid '%08x-e2e0-4000-8000-%012x' $$ $uuids
  stamp
  printf '{"parentUuid":%s,"isSidechain":false,"userType":"external","cwd":"%s","sessionId":"%s","version":"2.1.296","gitBranch":"main","type":"%s",%s,"uuid":"%s","timestamp":"%s"}\n' \
    "$parent" "$PWD" "$conversation" "$1" "$2" "$uuid" "$stamp" >> "$log"
  parent="\"$uuid\""
}

# The next tool call: command $1, described as $2.
tools=0
call() {
  tools=$((tools + 1))
  printf -v tool 'toolu_01E2E%06d' $tools
  json "$1"
  local command=$json
  json "$2"
  entry assistant "\"message\":{\"id\":\"msg_01E2E$tools\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"$tool\",\"name\":\"Bash\",\"input\":{\"command\":$command,\"description\":$json}}],\"stop_reason\":null,\"usage\":{\"input_tokens\":4,\"output_tokens\":12}}"
}

# The last call's result.
result() {
  entry user "\"message\":{\"role\":\"user\",\"content\":[{\"tool_use_id\":\"$tool\",\"type\":\"tool_result\",\"content\":\"ok\",\"is_error\":false}]},\"toolUseResult\":{\"stdout\":\"ok\",\"stderr\":\"\",\"interrupted\":false}"
}

# Text typed while the turn ran, as Claude Code logs it once it takes it.
pastes=0
queued() {
  local text=$1
  if [[ $text == *$'\n'* ]]; then
    pastes=$((pastes + 1))
    local id
    printf -v id '%04x' $((0x6523 + pastes))
    text="<pasted_content id=\"$id\">"$'\n'"$text"$'\n'"</pasted_content id=\"$id\">"
  fi
  json "$text"
  entry attachment "\"attachment\":{\"type\":\"queued_command\",\"prompt\":$json,\"commandMode\":\"prompt\",\"origin\":{\"kind\":\"human\"},\"humanTurn\":true}"
}

# The turn's last words.
say() {
  json "$1"
  entry assistant "\"message\":{\"id\":\"msg_01E2Esay\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":$json}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":4,\"output_tokens\":30}}"
}

# A whole screen in one write, so it's never seen half drawn: each line at
# its row, as bash's output is flushed at every newline.
draw() {
  local frame="" row=0 line
  local -a lines
  mapfile -t lines <<< "$1"
  for line in "${lines[@]}"; do
    row=$((row + 1))
    frame+="$esc[$row;1H$line$esc[K"
  done
  printf '%s' "$frame$esc[J"
}

glyphs=('·' '✢' '✳' '✶' '✻' '✽')
spin=0
working_frame() {
  spin=$(((spin + 1) % ${#glyphs[@]}))
  draw "$working_top${glyphs[spin]} Pondering… ($(((now - started) / 1000))s · ↓ 1.8k tokens)$working_bottom"
}

state=working
queue=()
received=0
taken=0
approved=0
buf=""
pending=""
take_at=0
allow=0

# What's typed: a paste then Enter is one text.
on_input() {
  local c=$1
  printf '%s' "$c" >> "$ctl/input"
  if [[ $state == asking ]]; then
    printf '%s' "$c" >> "$ctl/typed_while_asking"
  fi
  buf+=$c
  # Enter (bash's read turns its CR into LF); inside a paste, a line break
  # is the paste's.
  [[ $c == $'\r' || $c == $'\n' ]] || return 0
  if [[ $buf == *"$paste_open"* && $buf != *"$paste_close"* ]]; then
    return 0
  fi
  local text
  if [[ $buf == *"$paste_close"? ]]; then
    text=${buf#*"$paste_open"}
    text=${text%"$paste_close"*}
  else
    text=${buf%?}
  fi
  buf=""
  text=${text//$'\r'/$'\n'}
  [[ -n $text ]] || return 0
  received=$((received + 1))
  clock
  json "$text"
  printf '{"n":%d,"at":%d,"state":"%s","queued":%d,"text":%s}\n' \
    $received $now "$state" ${#queue[@]} "$json" >> "$ctl/received.jsonl"
  event received $received
  queue+=("$text")
}

# One step of work: the call before returns, what was typed is taken when
# allowed, the next call goes out; or, told to finish, the turn ends.
step() {
  if [[ -e $ctl/finish ]] && ((${#queue[@]} == 0)); then
    result
    say "$STAND_IN_ANSWER"
    state=idle
    draw "$idle"
    event finished
    return
  fi
  result
  if ((${#queue[@]} > 0 && taken < allow)); then
    local text=${queue[0]}
    queue=("${queue[@]:1}")
    taken=$((taken + 1))
    if ((taken == ask_after && !approved)); then
      # Its next call needs permission: the prompt comes up, and the take
      # is logged a moment later, while it shows.
      state=asking
      draw "$asking"
      event asking
      pending=$text
      clock
      take_at=$((now + 1200))
      return
    fi
    queued "$text"
    event took $taken
  fi
  call "cargo test -p parser" "Run the parser tests"
}

stty -icanon -echo -icrnl -ixon min 1 time 0
printf '%s' "$esc[?1049h$esc[?2004h"
clock
started=$now
printf '{"pid":%d,"sessionId":"%s","cwd":"%s","startedAt":%d}\n' \
  $$ "$conversation" "$PWD" "$now" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
entry user '"message":{"role":"user","content":"Fix the parser, and look at whatever I send you meanwhile."}'
call "cargo test -p parser" "Run the parser tests"
next_frame=0
next_step=$((now + step_ms))
event started $$

while :; do
  IFS= read -r -N 1 -t 0.05 c
  status=$?
  if ((status == 0)); then
    on_input "$c"
  elif ((status <= 128)); then
    exit 0
  fi
  clock
  # Never outlive a test that lost track of it.
  ((now - started < 600000)) || exit 0
  case $state in
    working)
      [[ -r $ctl/allow ]] && read -r allow < "$ctl/allow"
      if ((now >= next_step)); then
        next_step=$((now + step_ms))
        step
      fi
      if [[ $state == working ]] && ((now >= next_frame)); then
        working_frame
        next_frame=$((now + frame_ms))
      fi
      ;;
    asking)
      if [[ -n $pending ]] && ((now >= take_at)); then
        queued "$pending"
        pending=""
        event took $taken
        call "rm -rf target && cargo test" "Clean build artefacts and rerun the tests"
      fi
      if [[ -z $pending && -e $ctl/approve ]]; then
        approved=1
        state=working
        event approved
        next_step=$((now + step_ms))
        next_frame=0
      fi
      ;;
  esac
done
