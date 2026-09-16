#!/usr/bin/env bash
# Regenerate docs/demo.gif, from the repo root:
#   nix develop -c ./docs/record-demo.sh
set -euo pipefail

for v in $(env | grep -o '^TCOMP_[A-Z_]*' || true); do unset "$v"; done

PORT=7778
CDP_PORT=9334
W=1400
H=560
GIF_W=900
TOKEN=$(openssl rand -hex 16)
NAMES=(api web worker)
SELECTED=worker
CLIENT_TYPED='echo typed on the real terminal'
BROWSER_TYPED='echo typed right here in the browser'

OUT=$(mktemp -d)
FAKE_CWD=/tmp/project
mkdir -p "$FAKE_CWD"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  rmdir "$FAKE_CWD" 2>/dev/null || true
  rm -rf "$OUT"
}
trap cleanup EXIT

cargo build --release -p tcomp
BIN=$PWD/target/release/tcomp

TCOMP_BIND="127.0.0.1:$PORT" TCOMP_TOKEN="$TOKEN" "$BIN" serve >/dev/null 2>"$OUT/relay.err" &
PIDS+=($!)

for _ in $(seq 1 80); do
  curl -s -o /dev/null "http://127.0.0.1:$PORT/healthz" && break
  sleep 0.25
done

for name in "${NAMES[@]}"; do
  fifo="$OUT/fifo-$name"
  mkfifo "$fifo"
  (
    cd "$FAKE_CWD"
    exec "$BIN" --relay "http://127.0.0.1:$PORT" --token "$TOKEN" --allow-input --exit-on-end \
      --name "$name" -- bash --noprofile --norc -i
  ) <"$fifo" >/dev/null 2>"$OUT/$name.err" &
  PIDS+=($!)
  eval "exec {fd_$name}>\"\$fifo\""
  export "fd_$name"
done

count=0
for _ in $(seq 1 80); do
  count=$(curl -s -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT/api/sessions" \
    | jq -r '. | length' 2>/dev/null || echo 0)
  [ "$count" -ge "${#NAMES[@]}" ] && break
  sleep 0.25
done
[ "$count" -ge "${#NAMES[@]}" ] || { echo "sessions never appeared on :$PORT" >&2; exit 1; }
echo "recording with ${#NAMES[@]} floating sessions on :$PORT"

# Session ids, so the floating windows can be pinned to a tidy non-overlapping
# layout instead of the dashboard's default (collision-unaware) random spread.
for _ in $(seq 1 40); do
  ID_API=$(grep -o '/s/[a-f0-9]*' "$OUT/api.err" 2>/dev/null | head -1 | sed 's#/s/##') || true
  ID_WEB=$(grep -o '/s/[a-f0-9]*' "$OUT/web.err" 2>/dev/null | head -1 | sed 's#/s/##') || true
  ID_WORKER=$(grep -o '/s/[a-f0-9]*' "$OUT/worker.err" 2>/dev/null | head -1 | sed 's#/s/##') || true
  [ -n "${ID_API:-}" ] && [ -n "${ID_WEB:-}" ] && [ -n "${ID_WORKER:-}" ] && break
  sleep 0.1
done
[ -n "${ID_API:-}" ] && [ -n "${ID_WEB:-}" ] && [ -n "${ID_WORKER:-}" ] \
  || { echo "could not read back session ids" >&2; exit 1; }

positions_json=$(jq -cn --arg api "$ID_API" --arg web "$ID_WEB" --arg worker "$ID_WORKER" \
  '{($api): {x:30,y:90,z:1}, ($web): {x:490,y:90,z:2}, ($worker): {x:950,y:90,z:3}}')
POSITIONS_JS="localStorage.setItem('tcomp.floating-positions', JSON.stringify($positions_json)); location.reload();"
eval "SELECTED_ID=\$ID_${SELECTED^^}"
CLICK_JS="(function(){var r=document.querySelector('a.window[data-session-id=\"$SELECTED_ID\"]').getBoundingClientRect(); return Math.round(r.x+60)+','+Math.round(r.y+22);})()"
export POSITIONS_JS CLICK_JS

cat >"$OUT/inner.sh" <<INNER_EOF
#!/usr/bin/env bash
set -uo pipefail
kill_tree() {
  local pid=\$1 child
  for child in \$(pgrep -P "\$pid" 2>/dev/null); do
    kill_tree "\$child"
  done
  kill -9 "\$pid" 2>/dev/null || true
}
trap '[ -n "\${CHROME_PID:-}" ] && kill_tree "\$CHROME_PID"; true' EXIT

page_entry() { curl -s "http://127.0.0.1:$CDP_PORT/json" | jq -c '.[] | select(.type=="page")'; }
cdp_eval() {
  jq -cn --arg expr "\$1" '{id:2,method:"Runtime.evaluate",params:{expression:\$expr,returnByValue:true}}' \
    | timeout 5 websocat -1 -n "\$(page_entry | jq -r '.webSocketDebuggerUrl')"
}
type_into_fd() {
  local text=\$1 target=\$2 i ch
  for (( i=0; i<\${#text}; i++ )); do
    ch="\${text:i:1}"
    printf '%s' "\$ch" >&"\$target"
    sleep 0.03
  done
  printf '\n' >&"\$target"
}

setsid chromium --no-sandbox --disable-gpu --no-first-run --test-type --hide-scrollbars \
         --force-device-scale-factor=1 --kiosk --window-position=0,0 --window-size=$W,$H \
         --user-data-dir="$OUT/profile" \
         --remote-debugging-port=$CDP_PORT --remote-debugging-address=127.0.0.1 \
         --app="http://127.0.0.1:$PORT/?token=$TOKEN" >"$OUT/chromium.log" 2>&1 &
CHROME_PID=\$!
sleep 3

cdp_eval "\$POSITIONS_JS" >/dev/null
sleep 2

click_at=\$(cdp_eval "\$CLICK_JS" | jq -r '.result.result.value')
[ -n "\$click_at" ] && [ "\$click_at" != "null" ] || { echo "could not locate the selected window" >&2; exit 1; }
echo "selecting $SELECTED ($SELECTED_ID) at \$click_at"

ffmpeg -y -loglevel error -f x11grab -draw_mouse 1 -video_size ${W}x${H} -framerate 10 \
       -i "\${DISPLAY}.0+0,0" "$OUT/browser.mp4" &
FFMPEG_PID=\$!

sleep 1.5
x=\${click_at%,*}; y=\${click_at#*,}
xdotool mousemove --sync "\$x" "\$y" click 1
sleep 1.5

type_into_fd "$CLIENT_TYPED" "\$fd_$SELECTED"
sleep 2

xdotool type --delay 30 "$BROWSER_TYPED"
xdotool key Return
sleep 2.5

kill -INT "\$FFMPEG_PID"
wait "\$FFMPEG_PID" 2>/dev/null || true
INNER_EOF
chmod +x "$OUT/inner.sh"

xvfb-run -a -s "-screen 0 ${W}x${H}x24" "$OUT/inner.sh"

ffmpeg -y -loglevel error -i "$OUT/browser.mp4" -filter_complex \
  "fps=10,scale=${GIF_W}:-2:flags=lanczos,split[a][b];\
   [a]palettegen=max_colors=96:stats_mode=diff[p];\
   [b][p]paletteuse=dither=bayer:bayer_scale=3" "$OUT/demo.gif"

gifsicle -O3 --lossy=90 "$OUT/demo.gif" -o docs/demo.gif
ls -lh docs/demo.gif
