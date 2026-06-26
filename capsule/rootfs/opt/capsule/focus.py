#!/usr/bin/env python3
# Assert X keyboard input focus on the game window.
#
# Xvnc runs with NO window manager, so nothing assigns X input focus. SDL2 usually
# takes focus on a fullscreen window, but on a WM-less server it's not guaranteed --
# and XTEST keystrokes injected by input_ws are delivered to whatever holds input
# focus. If that's None/the root, the game never sees the keys. So every few seconds
# we find the game window (WM_NAME contains "doom") and XSetInputFocus to it. Cheap,
# idempotent, and re-asserts after the game restarts.
import time
from Xlib import display, X


def walk(win):
    yield win
    try:
        for child in win.query_tree().children:
            yield from walk(child)
    except Exception:
        pass


def main():
    d = display.Display(":1")
    root = d.screen().root
    while True:
        target = None
        for w in walk(root):
            try:
                name = w.get_wm_name()
            except Exception:
                name = None
            if name and "doom" in str(name).lower():
                target = w
        if target is not None:
            try:
                target.set_input_focus(X.RevertToParent, X.CurrentTime)
                d.sync()
            except Exception:
                pass
        time.sleep(4)


if __name__ == "__main__":
    main()
