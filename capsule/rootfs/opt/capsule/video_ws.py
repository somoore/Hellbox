#!/usr/bin/env python3
"""H.264 video WebSocket server (Lever 3 — DIY video codec, served on :6903).

Replaces VNC's frame-diffed JPEG rectangles with a real inter-frame codec. On each
client connection: grab the Xvnc display (:1) with ffmpeg's x11grab, encode H.264
with libx264 in `zerolatency` mode, split the Annex-B elementary stream into access
units (one per frame, delimited by AUDs), and send each AU as one binary WebSocket
frame. The browser decodes them with the WebCodecs `VideoDecoder` onto a canvas.

Why: VNC re-sends JPEG-ish dirty rects every frame for full-motion content (terrible
for DOOM). H.264 uses motion-compensated P-frames + rate control, cutting the video
half of egress a lot. Licensing-free, ARM-clean, no systemd — off-the-shelf
remote-desktop/streaming servers didn't fit the MicroVM's constraints (headless/no-systemd,
ARM64, licensing).

Wire protocol (binary WS frames): each frame is `<1 byte: 1=key,0=delta>` + the raw
Annex-B access unit. Keyframe AUs carry SPS+PPS+IDR (repeat-headers=1), so a client
can join mid-stream by waiting for the first key frame.

Listens on 0.0.0.0:6903.
"""
import asyncio
import subprocess
import websockets

DISPLAY = ":1.0"
PORT = 6903
FPS = 30
GOP = 60  # keyframe every 2s — bounds mid-stream join latency


def _screen_size():
    """Actual root-window size of display :1 — the app (DOOM via SDL) may switch the
    X mode on fullscreen (e.g. to 640x480), so we must capture whatever it really is,
    not a hardcoded guess. Falls back to 1280x720 if the query fails."""
    try:
        from Xlib import display as _xd
        d = _xd.Display(":1")
        s = d.screen()
        w, h = int(s.width_in_pixels), int(s.height_in_pixels)
        d.close()
        # x264 needs even dimensions for yuv420p.
        return (w - (w % 2)), (h - (h % 2))
    except Exception:
        return 1280, 720


def _start_ffmpeg():
    w, h = _screen_size()
    return subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error",
         "-fflags", "nobuffer", "-flags", "low_delay",
         "-f", "x11grab", "-framerate", str(FPS),
         "-video_size", f"{w}x{h}", "-i", DISPLAY,
         "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
         "-profile:v", "baseline", "-level", "3.1", "-pix_fmt", "yuv420p",
         # aud=1 delimits access units; repeat-headers=1 puts SPS/PPS before every
         # IDR so a late joiner can decode from the next keyframe.
         "-x264-params", f"keyint={GOP}:min-keyint={GOP}:scenecut=0:aud=1:repeat-headers=1",
         "-f", "h264", "pipe:1"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )


def _find_aud(buf, start):
    """Index of the next Access Unit Delimiter (start code + NAL type 9) whose
    boundary is strictly after position 0, adjusted to include a 4-byte start code's
    leading zero. Skips the AUD that opens the buffer (so we split into *whole* AUs,
    not empty slices). None if no further AUD is present."""
    pos = start
    while True:
        i = buf.find(b"\x00\x00\x01\x09", pos)
        if i < 0:
            return None
        j = i - 1 if (i > 0 and buf[i - 1] == 0) else i  # include 4-byte leading zero
        if j <= 0:
            pos = i + 4  # this is the buffer's opening AUD — keep looking
            continue
        return j


def _is_keyframe(au):
    """True if the access unit contains an IDR slice (NAL type 5) or SPS (7)."""
    i = 0
    while True:
        j = au.find(b"\x00\x00\x01", i)
        if j < 0:
            return False
        nal_type = au[j + 3] & 0x1F if j + 3 < len(au) else 0
        if nal_type in (5, 7):
            return True
        i = j + 3


async def handler(ws):
    proc = _start_ffmpeg()
    loop = asyncio.get_event_loop()
    buf = b""
    sent_any = False
    try:
        while True:
            # read1() returns whatever bytes are already available (vs read(), which
            # blocks until the full count) — critical: at ~3 Mbps a 64 KB read()
            # would buffer ~180 ms of video before we ever ship a frame.
            chunk = await loop.run_in_executor(None, proc.stdout.read1, 65536)
            if not chunk:
                # ffmpeg produced no/no-more output. If we never sent a frame, it
                # failed at startup — surface its stderr to the client (no MicroVM
                # shell otherwise) so the cause is visible.
                if not sent_any:
                    try:
                        err = proc.stderr.read().decode("utf-8", "replace") if proc.stderr else ""
                    except Exception:
                        err = ""
                    rc = proc.poll()
                    await ws.send("ffmpeg-exit rc=%s: %s" % (rc, (err or "(no stderr)")[:1500]))
                break
            buf += chunk
            # Emit every complete access unit: split at AUD boundaries, keeping the
            # last (possibly partial) AU in the buffer.
            while True:
                nxt = _find_aud(buf, 1)
                if nxt is None:
                    break
                au = buf[:nxt]
                buf = buf[nxt:]
                if au:
                    flag = b"\x01" if _is_keyframe(au) else b"\x00"
                    await ws.send(flag + au)
                    sent_any = True
    except Exception:
        pass
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=2)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass


async def main():
    async with websockets.serve(handler, "0.0.0.0", PORT, max_size=None):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
