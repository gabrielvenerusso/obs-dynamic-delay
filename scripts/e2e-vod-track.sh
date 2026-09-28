#!/usr/bin/env bash
# End-to-end test of the Twitch VOD track: ffmpeg plays OBS with a second audio track
# (Enhanced RTMP multitrack, as OBS sends the VOD track), a second ffmpeg plays the
# platform. Toggles the delay mid-stream and checks both audio tracks arrive and decode.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/obs-dynamic-delay"
[ -x "$BIN" ] || [ -x "$BIN.exe" ] || cargo build --release --manifest-path "$ROOT/Cargo.toml"
WORK="$(mktemp -d)"
cd "$WORK"

cat > test.toml <<'EOF'
listen = "127.0.0.1:1937"
upstream_url = "rtmp://127.0.0.1:1941/app"
delay_seconds = 5
http_listen = "127.0.0.1:8799"
udp_listen = "127.0.0.1:8800"
EOF

ffmpeg -hide_banner -loglevel error -y -timeout 120 -listen 1 \
  -i rtmp://127.0.0.1:1941/app/testkey -map 0:v -map 0:a -c copy out.flv &
RECV=$!
sleep 1
"$BIN" test.toml > relay.log 2>&1 &
RELAY=$!
trap 'kill $RELAY $RECV 2>/dev/null || true' EXIT
sleep 1

# track 0: the stream audio (440 Hz); track 1: the VOD track (880 Hz)
ffmpeg -hide_banner -loglevel error -re \
  -f lavfi -i testsrc=size=640x360:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -map 0:v -map 1:a -map 2:a -c:v libx264 -preset veryfast -g 60 -b:v 800k -c:a aac -b:a 128k -t 25 \
  -f flv rtmp://127.0.0.1:1937/live/testkey &
SRC=$!

TOKEN="$(sed -n 's/^api_token = "\(.*\)"/\1/p' test.toml)"
api() { curl -fsS -H "x-dd-token: $TOKEN" -X POST "http://127.0.0.1:8799/api/cmd/$1" > /dev/null; }
phase() { curl -fsS -H "x-dd-token: $TOKEN" http://127.0.0.1:8799/api/status | sed -n 's/.*"phase":"\([a-z_]*\)".*/\1/p'; }

sleep 5;  api on
sleep 9;  [ "$(phase)" = delayed ] || { echo "FAIL: expected delayed, got $(phase)"; exit 1; }
api off
sleep 3;  [ "$(phase)" = live ] || { echo "FAIL: expected live, got $(phase)"; exit 1; }
wait $SRC
sleep 4
kill $RELAY; wait $RECV || true

# tags received: main AAC (0xAF), VOD track header (0x95 0x00) and frames (0x95 0x01)
python3 - out.flv <<'PY' || python - out.flv <<'PY2'
import sys
d = open(sys.argv[1], 'rb').read(); i = 13; c = {}
while i + 11 <= len(d):
    t = d[i]; n = int.from_bytes(d[i+1:i+4], 'big'); b = d[i+11:i+13]
    if t == 8: c[(b[0], b[1] if len(b) > 1 else -1)] = c.get((b[0], b[1] if len(b) > 1 else -1), 0) + 1
    i += 11 + n + 4
main, hdr, vod = c.get((0xAF, 1), 0), c.get((0x95, 0), 0), c.get((0x95, 1), 0)
print(f"main audio frames {main}, VOD header {hdr}, VOD frames {vod}")
sys.exit(0 if hdr >= 1 and vod > 500 and abs(main - vod) < main * 0.1 else 1)
PY
import sys
d = open(sys.argv[1], 'rb').read(); i = 13; c = {}
while i + 11 <= len(d):
    t = d[i]; n = int.from_bytes(d[i+1:i+4], 'big'); b = d[i+11:i+13]
    if t == 8: c[(b[0], b[1] if len(b) > 1 else -1)] = c.get((b[0], b[1] if len(b) > 1 else -1), 0) + 1
    i += 11 + n + 4
main, hdr, vod = c.get((0xAF, 1), 0), c.get((0x95, 0), 0), c.get((0x95, 1), 0)
print(f"main audio frames {main}, VOD header {hdr}, VOD frames {vod}")
sys.exit(0 if hdr >= 1 and vod > 500 and abs(main - vod) < main * 0.1 else 1)
PY2

STREAMS="$(ffprobe -v error -show_entries stream=codec_type,codec_name -of csv=p=0 out.flv | tr '\n' ' ')"
echo "streams received: $STREAMS"
[ "$(echo "$STREAMS" | grep -o 'aac,audio' | wc -l)" -ge 2 ] || { echo "FAIL: the VOD audio track is missing"; exit 1; }
# both audio tracks must decode without a single error
ERRORS="$(ffmpeg -v error -i out.flv -map 0:a -f null - 2>&1 || true)"
if [ -n "$ERRORS" ]; then
  echo "FAIL: audio decode errors:"; echo "$ERRORS" | head -20; exit 1
fi
# video packets: increasing DTS and no repeated PTS (ffmpeg's decoder may warn on the
# repeated keyframe of a freeze, which is not what is checked here)
ffprobe -v error -select_streams v -show_entries packet=pts,dts -of csv=p=0 out.flv > video.csv
python3 - video.csv <<'PY' || python - video.csv <<'PY2'
import sys
rows = [l.strip().split(",") for l in open(sys.argv[1]) if l.strip() and "N/A" not in l]
dts = [int(r[1]) for r in rows]; pts = [int(r[0]) for r in rows]
bad = sum(1 for a, b in zip(dts, dts[1:]) if b < a) + (len(pts) - len(set(pts)))
print(f"video packets {len(rows)}, bad timestamps {bad}"); sys.exit(1 if bad else 0)
PY
import sys
rows = [l.strip().split(",") for l in open(sys.argv[1]) if l.strip() and "N/A" not in l]
dts = [int(r[1]) for r in rows]; pts = [int(r[0]) for r in rows]
bad = sum(1 for a, b in zip(dts, dts[1:]) if b < a) + (len(pts) - len(set(pts)))
print(f"video packets {len(rows)}, bad timestamps {bad}"); sys.exit(1 if bad else 0)
PY2
DUR="$(ffprobe -v error -show_entries format=duration -of csv=p=0 out.flv)"
echo "OK: received ${DUR}s with both audio tracks decoding cleanly and valid video timestamps (work dir: $WORK)"
