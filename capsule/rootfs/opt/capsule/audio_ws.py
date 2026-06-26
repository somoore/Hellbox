#!/usr/bin/env python3
"""Audio WebSocket server (Opus, Lever 2).

On each client connection: capture the capsule sink's monitor with `parec`, pipe it
into `ffmpeg` which encodes **Opus @ ~96 kbps** in an Ogg stream, demux the Ogg pages
back into raw Opus packets, and stream those packets to the browser. The browser
decodes them with the WebCodecs `AudioDecoder` and plays via Web Audio.

Why Opus: raw s16le/44100/stereo PCM is ~1.4 Mbps (~0.635 GB/hr). Opus @96 kbps is
~0.045 GB/hr — a ~13-15x egress cut on the audio half — at transparent quality and
low latency (20 ms frames).

Wire protocol (binary WebSocket frames):
  * frame #1  = the Opus identification header (`OpusHead`) — the decoder description.
  * frames 2+ = one raw Opus packet each (a 20 ms audio frame).
The `OpusTags` comment header is dropped (the client doesn't need it). Because ffmpeg
starts a fresh Ogg stream per connection, `OpusHead` is always the first packet.

Listens on 0.0.0.0:6902.
"""
import asyncio
import subprocess
import websockets

DEVICE = "capsule.monitor"
PORT = 6902
RATE = 48000          # Opus runs natively at 48 kHz — match it to avoid resampling.
CHANNELS = 2
BITRATE = "96k"


def _start_pipeline():
    """parec -> ffmpeg(libopus/ogg). Returns (parec_proc, ffmpeg_proc)."""
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
         # Flush a tiny Ogg page per packet so latency stays ~one frame, not ~1 s.
         "-page_duration", "20000", "-flush_packets", "1",
         "-f", "ogg", "pipe:1"],
        stdin=parec.stdout, stdout=subprocess.PIPE,
    )
    # Let parec receive SIGPIPE if ffmpeg dies.
    if parec.stdout:
        parec.stdout.close()
    return parec, ffmpeg


def _read_exactly(stream, n):
    """Blocking read of exactly n bytes; b'' on EOF."""
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return b""
        buf += chunk
    return bytes(buf)


def _read_ogg_page(stream):
    """Read one Ogg page; return the list of complete Opus packets it carries.

    Ogg page = 'OggS' | ver(1) | type(1) | granule(8) | serial(4) | seq(4) |
    crc(4) | nsegs(1) | segtable(nsegs) | data. A packet ends at the first lacing
    segment < 255 (our 20 ms Opus frames are < 255 bytes, so one segment each).
    """
    # Resync to the 'OggS' capture pattern (robust against any partial read).
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
    # A trailing cur>0 means a packet continues on the next page; for 20 ms Opus
    # frames this never happens, so we ignore the (theoretical) continuation.
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
    async with websockets.serve(handler, "0.0.0.0", PORT, max_size=None):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
