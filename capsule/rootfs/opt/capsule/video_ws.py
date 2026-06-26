#!/usr/bin/env python3.11
"""H.264 video WebSocket on :6903.

Each binary frame is one byte of keyframe flag plus one Annex-B access unit.
"""
import asyncio
import subprocess
import websockets

DISPLAY = ":1.0"
PORT = 6903
FPS = 30
GOP = 60  # keyframe every 2s


def _screen_size():
    """Current root-window size for display :1."""
    try:
        from Xlib import display as _xd
        d = _xd.Display(":1")
        s = d.screen()
        w, h = int(s.width_in_pixels), int(s.height_in_pixels)
        d.close()
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
         # AUDs split frames; repeated headers let late joiners decode.
         "-x264-params", f"keyint={GOP}:min-keyint={GOP}:scenecut=0:aud=1:repeat-headers=1",
         "-f", "h264", "pipe:1"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )


def _find_aud(buf, start):
    """Index of the next AUD boundary after the buffer's opening AUD."""
    pos = start
    while True:
        i = buf.find(b"\x00\x00\x01\x09", pos)
        if i < 0:
            return None
        j = i - 1 if (i > 0 and buf[i - 1] == 0) else i
        if j <= 0:
            pos = i + 4
            continue
        return j


def _is_keyframe(au):
    """Return true if the access unit can start decoding."""
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
            # read1() avoids waiting for a full 64 KB buffer.
            chunk = await loop.run_in_executor(None, proc.stdout.read1, 65536)
            if not chunk:
                # Surface startup stderr to the browser.
                if not sent_any:
                    try:
                        err = proc.stderr.read().decode("utf-8", "replace") if proc.stderr else ""
                    except Exception:
                        err = ""
                    rc = proc.poll()
                    await ws.send("ffmpeg-exit rc=%s: %s" % (rc, (err or "(no stderr)")[:1500]))
                break
            buf += chunk
            # Keep the last partial AU in the buffer.
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
    async with websockets.serve(handler, "127.0.0.1", PORT, max_size=None):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
