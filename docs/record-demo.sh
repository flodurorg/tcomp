#!/usr/bin/env bash
# Regenerate docs/demo.gif, from the repo root:
#   nix develop -c ./docs/record-demo.sh
set -euo pipefail

PORT=7777
W=1040
H=640
GIF_W=860
DURATION=11

OUT=$(mktemp -d)
cleanup() { [ -n "${TCOMP_PID:-}" ] && kill "$TCOMP_PID" 2>/dev/null; rm -rf "$OUT"; }
trap cleanup EXIT

cargo build --release -p tcomp

./target/release/tcomp standalone \
  --bind "127.0.0.1:$PORT" --name workstation -- ./docs/demo-workload.sh >/dev/null 2>&1 &
TCOMP_PID=$!

for _ in $(seq 1 80); do
  ID=$(curl -s "http://127.0.0.1:$PORT/api/sessions" 2>/dev/null \
       | sed -n 's/.*"id":"\([a-f0-9]\{8,\}\)".*/\1/p' | head -1) || true
  if [ -n "${ID:-}" ]; then break; fi
  sleep 0.25
done
[ -n "${ID:-}" ] || { echo "no session appeared on :$PORT" >&2; exit 1; }
echo "recording session $ID"

xvfb-run -a -s "-screen 0 ${W}x${H}x24" bash -c "
  chromium --no-sandbox --disable-gpu --no-first-run --test-type --hide-scrollbars --force-device-scale-factor=1 \
           --kiosk --window-position=0,0 --window-size=${W},${H} \
           --app='http://127.0.0.1:$PORT/s/$ID' >/dev/null 2>&1 &
  sleep 3
  ffmpeg -y -loglevel error -f x11grab -draw_mouse 0 -video_size ${W}x${H} -framerate 10 \
         -i \"\${DISPLAY}.0+0,0\" -t $DURATION '$OUT/browser.mp4'
"

ffmpeg -y -loglevel error -i "$OUT/browser.mp4" -filter_complex \
  "fps=10,scale=${GIF_W}:-2:flags=lanczos,split[a][b];\
   [a]palettegen=max_colors=96:stats_mode=diff[p];\
   [b][p]paletteuse=dither=bayer:bayer_scale=3" "$OUT/demo.gif"

gifsicle -O3 --lossy=90 "$OUT/demo.gif" -o docs/demo.gif
ls -lh docs/demo.gif
