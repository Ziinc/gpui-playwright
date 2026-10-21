#!/usr/bin/env bash
# Films demo.js driving the to-do example and writes docs/demo.gif.
# Needs Xvfb, xdotool, ffmpeg and Python with Pillow. Run from the repo root:
#   cargo build -p todo && (cd e2e && npm ci) && e2e/demo/record.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
display=:${DEMO_DISPLAY:-98}
port=${GPUI_PLAYWRIGHT_PORT:-9333}
fps=15
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; rm -rf "$work"' EXIT

Xvfb "$display" -screen 0 1280x800x24 >/dev/null 2>&1 &
pids+=($!)
sleep 1

DISPLAY=$display GPUI_PLAYWRIGHT=$port "$root/target/debug/todo" >"$work/app.log" 2>&1 &
pids+=($!)
until curl -fs "http://127.0.0.1:$port/" >/dev/null; do sleep 0.5; done

window=$(DISPLAY=$display xdotool search --sync --name '^Todos$' | head -1)
eval "$(DISPLAY=$display xdotool getwindowgeometry --shell "$window")"
# Let the first frame settle before filming.
sleep 1

mkdir "$work/frames"
date +%s.%N >"$work/start"
ffmpeg -loglevel error -f x11grab -framerate "$fps" -video_size "${WIDTH}x${HEIGHT}" \
  -i "$display+$X,$Y" "$work/frames/%05d.png" &
ffmpeg=$!
pids+=($ffmpeg)

(cd "$root/e2e" && GPUI_PLAYWRIGHT_PORT=$port node demo/demo.js) >"$work/steps.jsonl"
kill -INT "$ffmpeg"
wait "$ffmpeg" || true

python3 "$root/e2e/demo/caption.py" "$work" "$fps"
mkdir -p "$root/docs"
ffmpeg -loglevel error -y -framerate "$fps" -i "$work/captioned/%05d.png" \
  -vf "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=none" \
  "$root/docs/demo.gif"
echo "wrote docs/demo.gif"
