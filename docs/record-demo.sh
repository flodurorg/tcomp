#!/usr/bin/env bash
# Regenerate docs/demo.gif, from the repo root:
#   nix develop -c ./docs/record-demo.sh
set -euo pipefail
trap 'exit 130' INT
trap 'exit 143' TERM

PIDS=()
cleanup() {
  local status=$? pid
  trap - EXIT
  for pid in "${PIDS[@]}"; do kill -TERM -- "-$pid" 2>/dev/null || true; done
  for pid in "${PIDS[@]}"; do
    kill -KILL -- "-$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  if (( status != 0 )) && [ "${1:-}" = remove ]; then
    for log in "$OUT"/*.log; do
      [ -f "$log" ] || continue
      printf '\n--- %s ---\n' "$log" >&2
      tail -30 "$log" >&2
    done
  fi
  if [ "${1:-}" = remove ]; then rm -rf "$OUT"; fi
  exit "$status"
}

wait_for() {
  local label=$1
  shift
  for _ in $(seq 1 100); do
    if "$@" 2>/dev/null; then return; fi
    sleep 0.1
  done
  printf 'Timed out: %s\n' "$label" >&2
  if [ -n "${PAGE_WS:-}" ]; then
    cdp_eval 'JSON.stringify({url:location.href,text:document.body.innerText})' >&2 || true
  fi
  return 1
}

session_ready() {
  curl -fsS -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/sessions" \
    | jq -e 'length == 1 and .[0].status == "live"' >/dev/null
}

page_ready() {
  PAGE_WS=$(curl -fsS "http://127.0.0.1:$CDP_PORT/json" \
    | jq -er 'first(.[] | select(.type == "page")) | .webSocketDebuggerUrl')
}

cdp_eval() {
  jq -cn --arg expr "$1" '{id:1,method:"Runtime.evaluate",params:{expression:$expr,returnByValue:true}}' \
    | timeout 5 websocat -1 -n "$PAGE_WS" \
    | jq -e 'select(.error == null and .result.exceptionDetails == null) | .result.result.value'
}

browser_has_line() {
  local text
  text=$(jq -Rn --arg text "$1" '$text')
  cdp_eval "Array.from(document.querySelectorAll('.xterm-rows > div')).some(row => row.textContent.trim() === $text)" >/dev/null
}

type_line() {
  xdotool type --clearmodifiers --delay 55 -- "$1"
  xdotool key --clearmodifiers Return
}

caption() {
  printf '%s' "$1" >"$OUT/caption.next"
  mv "$OUT/caption.next" "$OUT/caption.txt"
}

focus_window() {
  local time=0 X Y WIDTH HEIGHT
  xdotool windowfocus --sync "$1"
  eval "$(xdotool getwindowgeometry --shell "$1")"
  if [ -s "$OUT/progress" ]; then
    time=$(jq -Rrs '[split("\n")[] | select(startswith("out_time_us=")) | split("=")[1] | tonumber?] | (last // 0) / 1000000' "$OUT/progress")
  fi
  printf '%s %s %s %s %s\n' "$time" "$X" "$Y" "$WIDTH" "$HEIGHT" >>"$OUT/focus"
}

capture() {
  trap cleanup EXIT
  export TCOMP_RELAY="http://127.0.0.1:$PORT" TCOMP_TOKEN="$TOKEN" TCOMP_NAME=laptop
  export HOME="$OUT/home" HISTFILE=/dev/null INPUTRC=/dev/null
  unset PROMPT_COMMAND BASH_ENV ENV
  mkdir -p "$HOME" "$OUT/project"
  printf '%s\n' "PS1='\[\e[38;5;78m\]~/project\[\e[0m\] $ '" >"$HOME/.bashrc"
  cd "$OUT/project"

  setsid xterm -fa 'DejaVu Sans Mono' -fs 13 -geometry 58x19+384+136 \
    -bg '#05070b' -fg '#dbe3ef' -cr '#7ee787' -bc -b 16 -bw 2 \
    -xrm 'XTerm*borderColor: #334155' -xrm 'XTerm*metaSendsEscape: true' \
    -title 'Local terminal' \
    -e script -qefc 'bash --noprofile -i' "$OUT/client.log" \
    >"$OUT/xterm.log" 2>&1 &
  PIDS+=($!)
  wait_for 'terminal window' sh -c 'xdotool search --onlyvisible --name "^Local terminal$" >/dev/null'
  TERMINAL_WINDOW=$(xdotool search --onlyvisible --name '^Local terminal$')
  wait_for 'shell prompt' grep -q 'project' "$OUT/client.log"
  xsetroot -solid '#0d121c' -cursor_name left_ptr
  focus_window "$TERMINAL_WINDOW"
  xdotool mousemove 1040 630

  caption '01   Start in your terminal'
  setsid ffmpeg -y -loglevel error -f x11grab -draw_mouse 1 -video_size "${W}x${H}" -framerate 15 \
    -i "${DISPLAY}.0+0,0" -vf \
    "drawtext=font='DejaVu Sans':text='tcomp':fontsize=32:fontcolor=0x7ee787:x=32:y=24,\
     drawtext=font='DejaVu Sans':text='take your terminals on the go':fontsize=22:fontcolor=0xdbe3ef:x=156:y=32,\
     drawtext=font='DejaVu Sans':textfile='$OUT/caption.txt':reload=1:fontsize=24:fontcolor=0xdbe3ef:x=32:y=660" \
    -progress "$OUT/progress" -stats_period 0.05 \
    -c:v libx264 -preset ultrafast -tune zerolatency -crf 0 -pix_fmt yuv444p "$OUT/recording.mp4" \
    >"$OUT/ffmpeg.log" 2>&1 &
  FFMPEG_PID=$!
  PIDS+=("$FFMPEG_PID")
  sleep 0.8

  type_line 'tcomp --allow-input -- bash'
  wait_for 'live client session' session_ready
  sleep 0.5
  type_line 'echo "Hello from my terminal"'
  wait_for 'local output' grep -q $'\rHello from my terminal\r' "$OUT/client.log"
  sleep 1.5

  SESSION_ID=$(curl -fsS -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/sessions" | jq -er '.[0].id')
  caption '02   Open the same session in a browser'
  xdotool windowmove --sync "$TERMINAL_WINDOW" 26 136
  focus_window "$TERMINAL_WINDOW"
  setsid chromium --no-sandbox --disable-gpu --no-first-run --no-default-browser-check --test-type \
    --password-store=basic --disable-session-crashed-bubble --disable-features=Translate \
    --disable-signin-promo-on-avatar-pill-for-testing \
    --force-device-scale-factor=1 --window-position=732,90 --window-size=682,550 \
    --user-data-dir="$OUT/profile" \
    --remote-debugging-port="$CDP_PORT" --remote-debugging-address=127.0.0.1 \
    about:blank >"$OUT/chromium.log" 2>&1 &
  PIDS+=($!)
  wait_for 'browser debugger' page_ready
  wait_for 'browser window' sh -c 'xdotool search --onlyvisible --class chromium >/dev/null'
  BROWSER_WINDOW=$(xdotool search --onlyvisible --class chromium | tail -1)
  xdotool windowmove --sync "$BROWSER_WINDOW" 732 90
  xdotool windowsize --sync "$BROWSER_WINDOW" 682 550
  focus_window "$BROWSER_WINDOW"
  xdotool mousemove 1020 145
  xdotool key --clearmodifiers ctrl+l
  xdotool type --clearmodifiers --delay 15 -- "http://127.0.0.1:$PORT/s/$SESSION_ID?token=$TOKEN"
  sleep 0.5
  xdotool key --clearmodifiers Return
  wait_for 'existing terminal history in browser' browser_has_line 'Hello from my terminal'
  cdp_eval "document.getElementById('keyboard-toggle').click(); true" >/dev/null
  sleep 1.8

  caption '03   Type locally — both views update'
  xdotool mousemove --sync 240 350 click 1
  focus_window "$TERMINAL_WINDOW"
  type_line 'echo "Typed on the laptop"'
  wait_for 'local input mirrored in browser' browser_has_line 'Typed on the laptop'
  sleep 1.8

  caption '04   Type in the browser — same shell'
  xdotool mousemove --sync 960 370 click 1
  focus_window "$BROWSER_WINDOW"
  type_line 'echo "Typed in the browser"'
  wait_for 'browser input on local terminal' grep -q $'\rTyped in the browser\r' "$OUT/client.log"
  wait_for 'browser input round trip' browser_has_line 'Typed in the browser'
  sleep 1.5

  caption 'One live terminal. Input works from either side.'
  xdotool mousemove 1390 675
  sleep 2.5
  kill -INT "$FFMPEG_PID"
  wait "$FFMPEG_PID" || [ "$?" -eq 255 ]
}

if [ "${1:-}" = --capture ]; then
  capture
  exit
fi

for v in $(env | grep -o '^TCOMP_[A-Z_]*' || true); do unset "$v"; done

export PORT=7778 CDP_PORT=9334 W=1440 H=720
TOKEN=$(openssl rand -hex 16)
OUT=$(mktemp -d)
export TOKEN OUT
trap 'cleanup remove' EXIT

cargo build --release -p tcomp
export PATH="$PWD/target/release:$PATH"
export TCOMP_WEB_DIR="$PWD/web"

setsid env TCOMP_BIND="127.0.0.1:$PORT" TCOMP_TOKEN="$TOKEN" tcomp serve >"$OUT/relay.log" 2>&1 &
PIDS+=($!)
wait_for 'relay health' curl -fsS -o /dev/null "http://127.0.0.1:$PORT/healthz"

xvfb-run -a -s "-screen 0 ${W}x${H}x24" "$0" --capture

mapfile -t focus <"$OUT/focus"
duration=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$OUT/recording.mp4")
filters=()
for i in "${!focus[@]}"; do
  read -r start x y width height <<<"${focus[$i]}"
  end=$duration
  if (( i + 1 < ${#focus[@]} )); then read -r end _ <<<"${focus[$((i + 1))]}"; fi
  for spread in 10 7 4 1; do
    case $spread in
      10) alpha=0.03 ;;
      7) alpha=0.06 ;;
      4) alpha=0.12 ;;
      1) alpha=0.8 ;;
    esac
    filters+=("drawbox=x=$((x - spread)):y=$((y - spread)):w=$((width + spread * 2)):h=$((height + spread * 2)):color=0x7ee787@$alpha:t=2:enable='gte(t,$start)*lt(t,$end)'")
  done
done
filter=$(IFS=,; printf '%s' "${filters[*]}")
ffmpeg -y -loglevel error -i "$OUT/recording.mp4" -filter_complex \
  "$filter,fps=12,scale=1200:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3" \
  -loop 0 "$OUT/demo.gif"
gifsicle -O3 --lossy=35 "$OUT/demo.gif" -o docs/demo.gif
ls -lh docs/demo.gif
