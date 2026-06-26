#!/usr/bin/env python3.11
"""Input WebSocket on :6904.

Receives JSON keyboard/mouse events and injects them into X display :1 via XTEST.
"""
import asyncio
import json

import websockets
from Xlib import X, display, XK
from Xlib.ext import xtest

PORT = 6904

d = display.Display(':1')

# Non-printable JS key names.
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
    """Map a JS key name to an X keysym."""
    if key in SPECIAL_KEYS:
        return SPECIAL_KEYS[key]
    if isinstance(key, str) and len(key) == 1:
        ks = XK.string_to_keysym(key)
        if ks == 0 and key.isupper():
            ks = XK.string_to_keysym(key.lower())
        return ks
    return 0


def _handle_key(msg):
    key = msg.get("key")
    keysym = _keysym_for(key)
    if not keysym:
        return
    kc = d.keysym_to_keycode(keysym)
    if not kc:
        return
    evt = X.KeyPress if msg.get("down") else X.KeyRelease
    xtest.fake_input(d, evt, kc)
    d.sync()


def _handle_move(msg):
    xtest.fake_input(d, X.MotionNotify, x=int(msg.get("x", 0)), y=int(msg.get("y", 0)))
    d.sync()


def _handle_button(msg):
    button = int(msg.get("button", 0)) + 1
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
            pass


async def main():
    async with websockets.serve(handler, "127.0.0.1", PORT):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
