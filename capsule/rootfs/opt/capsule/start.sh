#!/usr/bin/env bash
# Capsule supervisor with a render-aware readiness gate.
#
# The build snapshot is captured the moment /ready returns 200, so we must NOT
# return 200 until the display is up (Xvnc + websockify on :6901) AND DOOM has
# actually drawn a frame -- otherwise the snapshot freezes a blank screen and every
# launch/resume restores blank. The hook responder on :9000 holds 503 until then,
# and the gate gives the engine generous time to start and render.
set -u
LOG() { echo "[capsule] $*" >&2; }
READY_FLAG=/run/capsule.ready
rm -f "$READY_FLAG" 2>/dev/null || true

# 1. Lifecycle/readiness responder on :9000 (root) — 503 until ready.
cat > /opt/hook.py <<'PY'
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
FLAG = "/run/capsule.ready"
class H(BaseHTTPRequestHandler):
    def _resp(self):
        ready = os.path.exists(FLAG); code = 200 if ready else 503
        b = b'{"status":"ok"}' if ready else b'{"status":"starting"}'
        self.send_response(code); self.send_header("Content-Length", str(len(b))); self.end_headers()
        try: self.wfile.write(b)
        except Exception: pass
    def do_GET(self): self._resp()
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0) or 0)
        if n: self.rfile.read(n)
        self._resp()
    def log_message(self, *a): pass
ThreadingHTTPServer(("0.0.0.0", 9000), H).serve_forever()
PY
python3 /opt/hook.py & LOG "hook responder :9000 (503 until ready) pid $!"

# ---- 2. DISPLAY STACK (root): Xvnc :1, websockify on :6901 ----
# -SecurityTypes None so the app user can connect to :1 with no X auth.
export HOME=/root USER=root
rm -f /tmp/.X11-unix/X1 /tmp/.X1-lock 2>/dev/null || true
# Geometry matches Chocolate Doom's 640x400 window (it draws at the top-left with no
# window manager to place/scale it). A larger root would leave DOOM in a
# corner surrounded by black -- bad demo AND it starves the render probe, which then
# samples mostly the black border and never crosses the brightness gate.
Xvnc :1 -geometry 640x400 -depth 24 -SecurityTypes None -rfbport 5901 \
     -AlwaysShared -desktop capsule > /var/log/xvnc.log 2>&1 &
LOG "Xvnc starting (pid $!)"
for i in $(seq 1 100); do [ -S /tmp/.X11-unix/X1 ] && break; sleep 0.2; done
export DISPLAY=:1
sleep 1
xsetroot -solid black 2>/dev/null || true

websockify --web=/opt/novnc 0.0.0.0:6901 localhost:5901 > /var/log/websockify.log 2>&1 &
LOG "websockify :6901 -> 5901 (pid $!)"

# H.264 video stream on :6903 (Lever 3 — DIY codec, the low-egress alternative to
# VNC). Per-connection: grabs the :1 display, libx264 -> Annex-B AUs over WS. Runs
# as root with DISPLAY=:1 (Xvnc is -SecurityTypes None, so x11grab needs no auth).
DISPLAY=:1 python3 /opt/capsule/video_ws.py > /var/log/video_ws.log 2>&1 &
LOG "video_ws (H.264) :6903 (pid $!)"
DISPLAY=:1 python3 /opt/capsule/input_ws.py > /var/log/input_ws.log 2>&1 &
LOG "input_ws (XTEST) :6904 (pid $!)"
DISPLAY_OK=0
for i in $(seq 1 50); do
  curl -sf -o /dev/null --max-time 2 http://127.0.0.1:6901/vnc.html && { DISPLAY_OK=1; break; }
  sleep 0.2
done
LOG "display: websockify serving on :6901 (vnc.html reachable=$DISPLAY_OK)"

# ---- 3. AUDIO + APP STACK (app user): PulseAudio + audio_ws + Chocolate Doom ----
install -d -o app -g app /run/user/1000 2>/dev/null || true
runuser -u app -- bash /opt/capsule/run_app.sh &
LOG "run_app.sh launched as 'app' (pid $!)"

# ---- 4. READINESS GATE: display + DOOM RENDERED ----
# The snapshot is frozen the instant /ready returns 200, so we hold 503 until the display
# is serving AND DOOM is actually DRAWN to the screen. Snapshotting before DOOM draws
# would restore a blank screen. The stream service ports (6902 audio_ws, 6903 video_ws,
# 6904 input_ws) are logged for visibility but not gated on (see below).
audio_bytes() {
  runuser -u app -- env XDG_RUNTIME_DIR=/run/user/1000 \
    timeout 3 parec --format=s16le --rate=44100 --channels=2 -d capsule.monitor 2>/dev/null | wc -c
}
port_up() { python3 -c 'import socket,sys; socket.create_connection(("127.0.0.1",int(sys.argv[1])),2).close()' "$1" 2>/dev/null; }
# Mean brightness of a center crop of display :1 at its ACTUAL current size. DOOM
# switches the X mode to 640x480 on fullscreen, so query it (same as video_ws.py);
# never hardcode a size (a fixed 1280x720 grab silently fails once DOOM switches). A
# black root averages ~0; a drawn DOOM frame is far brighter.
render_mean() {
  DISPLAY=:1 python3 -c 'import sys
from Xlib import display, X
try:
    s = display.Display(":1").screen()
    W = int(s.width_in_pixels); H = int(s.height_in_pixels)
    cw = min(W, 640); ch = min(H, 480); x0 = (W - cw) // 2; y0 = (H - ch) // 2
    b = s.root.get_image(x0, y0, cw, ch, X.ZPixmap, 0xffffffff).data
    print(sum(b) // max(1, len(b)))
except Exception:
    print(0)' 2>/dev/null
}
RENDER_MIN=20
display_up() { curl -sf -o /dev/null --max-time 2 http://127.0.0.1:6901/vnc.html; }

# Bound by wall-clock (each probe costs a few seconds), finishing before the 300s
# ready-hook timeout so a never-ready capsule fails the build loudly.
NOW=$(date +%s); DEADLINE=$((NOW + 520))
DISPLAY_OK=0; AUDIO_OK=0; SVC_OK=0; MEAN=0; RENDER_SEEN=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  display_up && DISPLAY_OK=1
  [ "$(audio_bytes)" -gt 0 ] 2>/dev/null && AUDIO_OK=1
  if port_up 6902 && port_up 6903 && port_up 6904; then SVC_OK=1; else SVC_OK=0; fi
  MEAN=$(render_mean); [ -n "$MEAN" ] || MEAN=0
  # LATCH render: DOOM's on-screen brightness swings wildly by scene (a center-crop
  # mean flaps 0..110 between dark and lit frames), so once we've seen a clearly-DOOM
  # bright frame (>= RENDER_MIN) we treat "rendered" as established for good. A black
  # root stays ~0 and never latches. Without this, the gate waited for a lucky bright
  # cycle to coincide with the deadline, pushing /ready past AWS's ready-hook timeout.
  [ "${MEAN:-0}" -ge "$RENDER_MIN" ] && RENDER_SEEN=1
  LOG "gate: display=$DISPLAY_OK audio=$AUDIO_OK services(6902/3/4)=$SVC_OK render_mean=$MEAN render_seen=$RENDER_SEEN"
  # Gate on display + DOOM RENDERED (latched). Stream services (6902/3/4) start long
  # before DOOM draws, so render-gating already snapshots them up; the port probe is
  # logged but not blocked on (it read 0 even with all service pids alive). Chocolate
  # Doom's audio is event-driven, so audio is logged but not required for readiness.
  if [ "$DISPLAY_OK" = 1 ] && [ "$RENDER_SEEN" = 1 ]; then
    touch "$READY_FLAG"
    LOG "READY: display + DOOM rendered (render_seen; last mean=$MEAN); audio=$AUDIO_OK services_probe=$SVC_OK; /ready -> 200"
    break
  fi
  sleep 2
done
[ -f "$READY_FLAG" ] || LOG "STILL NOT READY after 520s: display=$DISPLAY_OK audio=$AUDIO_OK services=$SVC_OK render_seen=$RENDER_SEEN -- leaving 503 so the build FAILS rather than snapshotting a half-started capsule"

while :; do sleep 30; done
