#!/usr/bin/env python3.11
"""Opus audio WebSocket on :6902.

Frame 1 is OpusHead. Later frames are raw 20 ms Opus packets.
"""
import asyncio
import subprocess
import websockets

DEVICE = "capsule.monitor"
PORT = 6902
RATE = 48000
CHANNELS = 2
BITRATE = "96k"


def _start_pipeline():
    """Start parec -> ffmpeg(libopus/ogg)."""
    parec = subprocess.Popen(
        ["parec", "--format=s16le", f"--rate={RATE}", f"--channels={CHANNELS}",
         "--latency-msec=30", "-d", DEVICE],
        stdout=subprocess.PIPE,
    )
    ffmpeg = subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error",
         "-f", "s16le", "-ar", str(RATE), "-ac", str(CHANNELS), "-i", "pipe:0",
         "-c:a", "libopus", "-b:a", BITRATE, "-application", "audio",
         "-frame_duration", "20",
         # One Ogg page per packet keeps latency near one frame.
         "-page_duration", "20000", "-flush_packets", "1",
         "-f", "ogg", "pipe:1"],
        stdin=parec.stdout, stdout=subprocess.PIPE,
    )
    if parec.stdout:
        parec.stdout.close()
    return parec, ffmpeg


def _read_exactly(stream, n):
    """Read exactly n bytes, or b'' on EOF."""
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return b""
        buf += chunk
    return bytes(buf)


def _read_ogg_page(stream):
    """Read one Ogg page and return complete Opus packets."""
    # Resync to OggS after any partial read.
    cap = _read_exactly(stream, 4)
    if not cap:
        return None
    while cap != b"OggS":
        nxt = stream.read(1)
        if not nxt:
            return None
        cap = cap[1:] + nxt
    header = _read_exactly(stream, 23)  # everything up to and incl. nsegs
    if not header:
        return None
    nsegs = header[-1]
    seg_table = _read_exactly(stream, nsegs) if nsegs else b""
    data_len = sum(seg_table)
    data = _read_exactly(stream, data_len) if data_len else b""
    if data_len and not data:
        return None

    packets, pos, cur = [], 0, 0
    for lace in seg_table:
        cur += lace
        if lace < 255:  # packet boundary
            packets.append(data[pos:pos + cur])
            pos += cur
            cur = 0
    return packets


async def handler(ws):
    parec, ffmpeg = _start_pipeline()
    loop = asyncio.get_event_loop()
    sent_head = False
    try:
        while True:
            packets = await loop.run_in_executor(None, _read_ogg_page, ffmpeg.stdout)
            if packets is None:
                break
            for pkt in packets:
                if not pkt:
                    continue
                if pkt.startswith(b"OpusTags"):
                    continue  # comment header — client doesn't need it
                if pkt.startswith(b"OpusHead"):
                    if sent_head:
                        continue
                    sent_head = True
                await ws.send(pkt)
    except Exception:
        pass
    finally:
        for p in (ffmpeg, parec):
            try:
                p.terminate()
                p.wait(timeout=2)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass


async def main():
    async with websockets.serve(handler, "127.0.0.1", PORT, max_size=None):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
