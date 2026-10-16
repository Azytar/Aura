#!/usr/bin/env python3
"""Check fixed-window zoom, pixel geometry and resize bursts on dedicated X11."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

from x11_daily_use import capture, run, wait_for, window_for


def main():
    for tool in ["xdotool", "xdpyinfo", "xprop", "xwd", "magick"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} is not installed")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no X11 display")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-resize-") as directory:
        root = Path(directory)
        (root / "aura").mkdir()
        config = root / "aura/config.toml"
        for invalid in ["[font]\nsizee = 13", "[cursor]\nblink = 'false'", "[window]\npadding_x = 4294967296"]:
            config.write_text(invalid)
            result = subprocess.run([binary], env=dict(os.environ, XDG_CONFIG_HOME=str(root)), capture_output=True, timeout=3)
            assert result.returncode == 1 and b"aura:" in result.stderr, "invalid configuration was silently ignored"
        print("PASS: invalid configuration produces actionable startup errors")
        config.write_text('''[font]
family = "monospace"
size = 13.0
letter_spacing = 2
line_spacing = 3
[window]
columns = 60
lines = 20
padding_x = 5
padding_y = 7
[colors]
background = "#123456"
[cursor]
blink = false
''')
        sizes = root / "sizes"
        helper = root / "shell"
        helper.write_text(f'''#!{sys.executable}
import fcntl, json, os, signal, struct, termios, tty
tty.setraw(0)
def size(*args):
    values = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    with open({str(sizes)!r}, "a") as output:
        output.write(json.dumps(values) + "\\n")
signal.signal(signal.SIGWINCH, size)
size()
os.write(1, b"\\x1b[?25lResize and zoom: ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789\\r\\nsecond line\\x1b]0;AURA_RESIZE_READY\\a")
while os.read(0, 4096): pass
''')
        helper.chmod(0o755)
        with open(root / "app.log", "wb") as log:
            app = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(root)), stdout=log, stderr=log)
            try:
                window = window_for(app)
                wait_for(lambda: sizes.exists() and run("xdotool", "getwindowname", window).strip() == b"AURA_RESIZE_READY", "PTY not ready")

                def geometry():
                    text = run("xdotool", "getwindowgeometry", "--shell", window)
                    return tuple(int(re.search(rb"^" + key + rb"=(\d+)$", text, re.M)[1]) for key in [b"WIDTH", b"HEIGHT"])

                def latest():
                    return json.loads(sizes.read_text().splitlines()[-1])

                def metrics():
                    hints = run("xprop", "-id", window, "WM_NORMAL_HINTS")
                    match = re.search(rb"resize increment: (\d+) by (\d+)", hints)
                    assert match, hints
                    return tuple(map(int, match.groups()))

                def synchronized():
                    width, height = geometry()
                    cw, ch = metrics()
                    return latest() == [max(1, min(256, (height - 14) // ch)), max(1, min(512, (width - 10) // cw)), width, height]

                original = geometry()
                time.sleep(0.15)
                before = capture(window, root / "before.xwd")
                assert before[2][:3] == bytes.fromhex("123456")
                original_metrics = metrics()
                run("xdotool", "key", "ctrl+plus")
                wait_for(lambda: metrics() != original_metrics, "zoom did not change font metrics")
                assert geometry() == original, "font zoom unexpectedly changed the outer window size"
                wait_for(synchronized, "zoom left grid/PTY/window metrics inconsistent")
                assert b"base size: 10 by 14" in run("xprop", "-id", window, "WM_NORMAL_HINTS")
                run("xdotool", "key", "ctrl+minus", "ctrl+0")
                wait_for(lambda: metrics() == original_metrics and synchronized(), "zoom reset did not restore configured metrics")
                time.sleep(0.15)
                assert capture(window, root / "reset.xwd") == before, "zoom round trip left shifted text or stale pixels"
                print("PASS: fixed-window zoom, configured reset, padding hints and identical pixels after round trip")
                for width, height in [(1, 1), (640, 400), (1000, 600), (360, 240), (1200, 720), (413, 297), original]:
                    run("xdotool", "windowsize", window, str(width), str(height))
                wait_for(lambda: geometry() == original and synchronized(), "resize burst did not settle on the final geometry")
                time.sleep(0.15)
                assert capture(window, root / "restored.xwd") == before, "resize round trip changed the logical text or left residue"
                print("PASS: resize bursts settle on matching window/grid/PTY dimensions without text or pixel residue")
            except Exception:
                log.flush()
                print((root / "app.log").read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                app.wait(timeout=8)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
