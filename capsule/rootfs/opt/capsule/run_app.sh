#!/usr/bin/env bash
# Runs as the non-root 'app' user. PulseAudio user-mode + null-sink + the audio-over-WS
# server on :6902, then native aarch64 Chocolate Doom (SDL2) in a restart loop. SDL
# renders to the X server and routes audio to the PulseAudio sink that audio_ws captures,
# so the browser gets both video and sound.
set -u
LOG() { echo "[app] $*" >&2; }

export XDG_RUNTIME_DIR=/run/user/1000
mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
export HOME=/home/app
APPDIR=/home/app/app

# ---- PulseAudio user mode + null-sink (default) ----
pulseaudio --start --exit-idle-time=-1 --disallow-exit 2>&1 | sed 's/^/[pa] /' >&2 || LOG "pulseaudio --start rc=$?"
sleep 2
pactl load-module module-null-sink sink_name=capsule rate=48000 channels=2 sink_properties=device.description=capsule 2>&1 | sed 's/^/[pa] null-sink: /' >&2 || LOG "null-sink FAILED"
pactl set-default-sink capsule 2>&1 | sed 's/^/[pa] /' >&2 || true

# ---- PCM-over-WebSocket server on :6902 (captures capsule.monitor) ----
python3 /opt/capsule/audio_ws.py > /tmp/audiows.log 2>&1 &
LOG "audio_ws :6902 pid $!"

export DISPLAY=:1
# SDL2: render to the Xvnc X server, route audio through the PulseAudio session
# (default sink = the 'capsule' null-sink that audio_ws captures and streams).
export SDL_VIDEODRIVER=x11
export SDL_AUDIODRIVER=pulse

# ---- Locate the shareware IWAD staged under capsule/app/ ----
cd "$APPDIR" || { LOG "FATAL: app dir $APPDIR missing"; exit 1; }
WAD="$(ls "$APPDIR"/*.wad "$APPDIR"/*.WAD 2>/dev/null | head -1)"
[ -n "$WAD" ] || { LOG "FATAL: no .wad in $APPDIR"; LOG "contents: $(ls -la "$APPDIR" 2>&1 | tr '\n' '|')"; exit 1; }
LOG "iwad: $WAD"

# ---- Launch native Chocolate Doom in a restart loop ----
# -fullscreen: SDL2 fills the 640x400 root (and reliably takes X input focus on a
#   WM-less server, so XTEST keystrokes from input_ws land in the game).
# -nograbmouse: don't capture the pointer (it's streamed over VNC, not local).
( while :; do
    LOG "launching: chocolate-doom -iwad $WAD -fullscreen -nograbmouse (native SDL2 ARM64)"
    /usr/local/bin/chocolate-doom -iwad "$WAD" -fullscreen -nograbmouse 2>&1 | sed 's/^/[doom] /' >&2
    LOG "doom exited rc=$? -- restarting in 2s"
    sleep 2
  done ) &
APP_LOOP=$!
LOG "doom restart loop pid $APP_LOOP"

# ---- Input-focus asserter: no window manager runs, so nothing assigns X keyboard
# focus; XTEST-injected keys would land nowhere. Periodically focus the game window. ----
DISPLAY=:1 python3 /opt/capsule/focus.py > /tmp/focus.log 2>&1 &
LOG "input-focus asserter started (pid $!)"

# ---- Render watchdog (native should render reliably; kept as a safety net) ----
render_peek() {
  DISPLAY=:1 python3 -c 'from Xlib import display, X
try:
    s = display.Display(":1").screen()
    W = int(s.width_in_pixels); H = int(s.height_in_pixels)
    cw = min(W, 640); ch = min(H, 480); x0 = (W - cw) // 2; y0 = (H - ch) // 2
    b = s.root.get_image(x0, y0, cw, ch, X.ZPixmap, 0xffffffff).data
    print(sum(b) // max(1, len(b)))
except Exception:
    print(0)' 2>/dev/null
}
( RENDERED=0; BLACK=0
  while :; do
    sleep 12
    [ "$RENDERED" = 1 ] && continue
    M=$(render_peek)
    if [ "${M:-0}" -ge 20 ]; then RENDERED=1; LOG "WATCHDOG: doom rendered (mean=$M) -- standing down"; continue; fi
    BLACK=$((BLACK + 1))
    if [ "$BLACK" -ge 4 ] && pgrep -x chocolate-doom >/dev/null 2>&1; then
      LOG "WATCHDOG: chocolate-doom alive but black ~48s -- killing to re-roll render"
      pkill -x chocolate-doom 2>/dev/null; BLACK=0; sleep 2
    fi
  done ) &
LOG "render watchdog started"

# ---- heartbeat: prove DOOM stays alive ----
while :; do
  if pgrep -x chocolate-doom >/dev/null 2>&1; then ALIVE=yes; else ALIVE=NO; fi
  LOG "heartbeat: chocolate-doom alive=$ALIVE"
  sleep 20
done
