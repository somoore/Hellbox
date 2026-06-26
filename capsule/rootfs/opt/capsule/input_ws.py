#!/usr/bin/env python3
"""Input WebSocket server (XTEST, Lever 3 — the reverse channel for H.264 mode).

The H.264 video path (video_ws.py :6903) streams the Xvnc display (:1) to the
browser one-way. This file is its reverse channel: the browser sends keyboard and
mouse events as JSON over a WebSocket, and we inject them into X display :1 using
the XTEST extension (python-xlib). Together they replace what noVNC gave us for
free in VNC mode — a fully interactive app — but over the DIY low-egress codec.

Xvnc :1 runs with `-SecurityTypes None`, so we open the display with no X auth.

Wire protocol (TEXT/JSON frames, client -> server):
  * {"t":"k","down":true|false,"key":"<JS KeyboardEvent.key>"}
      key is e.g. "ArrowUp"/"ArrowDown"/"ArrowLeft"/"ArrowRight", a modifier name
      ("Control","Alt","Shift","Meta"), a named key (" ","Enter","Escape","Tab",
      "Backspace"), or a single printable char ("a","Z","1",".").
  * {"t":"m","x":<int 0..1280>,"y":<int 0..720>}   absolute pointer move.
  * {"t":"b","button":0|1|2,"down":true|false}     0=left,1=middle,2=right.

Each event is wrapped in try/except so one malformed event never kills the
connection. The XTEST calls are blocking but trivially fast, so we call them
directly inside the async handler.

Listens on 0.0.0.0:6904.
"""
import asyncio
import json

import websockets
from Xlib import X, display, XK
from Xlib.ext import xtest

PORT = 6904

d = display.Display(':1')

# JS KeyboardEvent.key name -> X keysym for the non-printable / named keys.
SPECIAL_KEYS = {
    "ArrowUp": XK.XK_Up,
    "ArrowDown": XK.XK_Down,
    "ArrowLeft": XK.XK_Left,
    "ArrowRight": XK.XK_Right,
    "Control": XK.XK_Control_L,
    "Alt": XK.XK_Alt_L,
    "Shift": XK.XK_Shift_L,
    "Meta": XK.XK_Super_L,
    " ": XK.XK_space,
    "Enter": XK.XK_Return,
    "Escape": XK.XK_Escape,
    "Tab": XK.XK_Tab,
    "Backspace": XK.XK_BackSpace,
}


def _keysym_for(key):
    """Map a JS KeyboardEvent.key to an X keysym, or 0 (NoSymbol) if unmappable."""
    if key in SPECIAL_KEYS:
        return SPECIAL_KEYS[key]
    if isinstance(key, str) and len(key) == 1:
        ks = XK.string_to_keysym(key)
        if ks == 0 and key.isupper():
            # e.g. "Z" may not resolve directly; fall back to the lowercase keysym.
            ks = XK.string_to_keysym(key.lower())
        return ks
    return 0


def _handle_key(msg):
    key = msg.get("key")
    keysym = _keysym_for(key)
    if not keysym:
        return  # NoSymbol — nothing we can inject
    kc = d.keysym_to_keycode(keysym)
    if not kc:
        return  # no keycode bound to this keysym
    evt = X.KeyPress if msg.get("down") else X.KeyRelease
    xtest.fake_input(d, evt, kc)
    d.sync()


def _handle_move(msg):
    xtest.fake_input(d, X.MotionNotify, x=int(msg.get("x", 0)), y=int(msg.get("y", 0)))
    d.sync()


def _handle_button(msg):
    button = int(msg.get("button", 0)) + 1  # X buttons are 1-based (1=left,2=mid,3=right)
    evt = X.ButtonPress if msg.get("down") else X.ButtonRelease
    xtest.fake_input(d, evt, button)
    d.sync()


async def handler(ws):
    async for raw in ws:
        try:
            msg = json.loads(raw)
            t = msg.get("t")
            if t == "k":
                _handle_key(msg)
            elif t == "m":
                _handle_move(msg)
            elif t == "b":
                _handle_button(msg)
        except Exception:
            # One bad event must never tear down the connection.
            pass


async def main():
    async with websockets.serve(handler, "0.0.0.0", PORT):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
