#!/usr/bin/env python3
"""Exercise input and selection ownership on a dedicated X11 test display."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

from x11_daily_use import capture, run, wait_for, window_for


def main():
    for tool in ["xdotool", "setxkbmap", "xdpyinfo", "xwd", "magick"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} is not installed")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no live X11 display")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    selected = sys.argv[2] if len(sys.argv) > 2 else "all"
    with tempfile.TemporaryDirectory(prefix="aura-input-") as directory:
        root = Path(directory)
        (root / "aura").mkdir()
        (root / "aura" / "config.toml").write_text("[window]\ncolumns = 40\nlines = 12\npadding_x = 3\npadding_y = 5\n")
        control = root / "control"
        os.mkfifo(control)
        keys = root / "keys"
        compose = root / "Compose"
        compose.write_text('<dead_acute> <e> : "é"\n<dead_grave> <a> : "à"\n')
        helper = root / "shell"
        helper.write_text(f'''#!{sys.executable}
import os, select, tty
tty.setraw(0)
control = os.open({str(control)!r}, os.O_RDONLY | os.O_NONBLOCK)
pending = b""
with open({str(keys)!r}, "wb", buffering=0) as output:
    os.write(1, b"\\x1b[?25l\\x1b]0;AURA_INPUT_READY\\a")
    while True:
        ready, _, _ = select.select([0, control], [], [])
        for fd in ready:
            data = os.read(fd, 4096)
            if fd == 0:
                if not data: raise SystemExit(0)
                output.write(data)
            else:
                pending += data
                while b"\\n" in pending:
                    command, pending = pending.split(b"\\n", 1)
                    os.write(1, bytes.fromhex(command.decode()))
''')
        helper.chmod(0o755)
        # The parent keeps the FIFO open across commands; no live input is
        # needed to drive screen transitions while a mouse button is held.
        descriptor = os.open(control, os.O_RDWR | os.O_NONBLOCK)
        environment = dict(os.environ, SHELL=str(helper), XCOMPOSEFILE=str(compose), XDG_CONFIG_HOME=str(root))
        with open(root / "aura.log", "wb") as log:
            app = subprocess.Popen([binary], env=environment, stdout=log, stderr=log)
            try:
                run("setxkbmap", "-layout", "us", "-variant", "", "-option", "")
                window = window_for(app)
                wait_for(lambda: keys.exists() and run("xdotool", "getwindowname", window).strip() == b"AURA_INPUT_READY", "raw shell not ready")
                serial = 0

                def emit(data):
                    nonlocal serial
                    serial += 1
                    title = f"AURA_INPUT_{serial}".encode()
                    os.write(descriptor, (data + b"\x1b]0;" + title + b"\a").hex().encode() + b"\n")
                    wait_for(lambda: run("xdotool", "getwindowname", window).strip() == title, "output not processed")

                if selected in ("all", "keyboard"):
                    run("setxkbmap", "-layout", "us", "-variant", "intl", "-option", "")
                    time.sleep(0.15)
                    run("xdotool", "key", "--delay", "60", "dead_acute", "c", "dead_acute", "dead_grave", "a", "dead_acute", "ctrl+c", "e")
                    expected = "cà".encode() + b"\x03e"
                    wait_for(lambda: keys.read_bytes() == expected, "cancelled Compose lost or duplicated input")
                    run("setxkbmap", "-layout", "us", "-variant", "", "-option", "")
                    time.sleep(0.15)
                    run("xdotool", "key", "--repeat", "32", "--delay", "10", "shift+a")
                    expected += b"A" * 32
                    wait_for(lambda: keys.read_bytes() == expected, "repeated key presses lost bytes or modifiers")
                    # Test server autorepeat as well as repeated synthetic presses.
                    start = len(expected)
                    run("xdotool", "keydown", "a")
                    try:
                        wait_for(lambda: len(keys.read_bytes()) >= start + 3, "held key did not repeat", timeout=4)
                    finally:
                        run("xdotool", "keyup", "a")
                    time.sleep(0.1)
                    actual = keys.read_bytes()
                    assert actual[:start] == expected and actual[start:] and set(actual[start:]) == {ord("a")}, actual
                    print("PASS: cancelled/restarted Compose, Ctrl cancellation, modifiers and held/repeated keys")

                if selected in ("all", "drag"):
                    emit(b"\x1bc\x1b[?25l" + b"history\r\n" * 40)
                    alternate = b"\x1b[?1049h\x1b[?1000h\x1b[2J\x1b[Halternate screen"
                    emit(alternate)
                    time.sleep(0.15)
                    reference = capture(window, root / "reference.xwd")
                    emit(b"\x1b[?1000l\x1b[?1049l")
                    run("xdotool", "mousemove", "--window", window, "100", "80", "mousedown", "1")
                    run("xdotool", "mousemove", "--window", window, "100", "0")
                    time.sleep(0.1)
                    emit(alternate)
                    time.sleep(0.3)
                    tracked = capture(window, root / "tracked.xwd")
                    assert tracked == reference, "a stale selection drag modified the application's alternate screen"
                    run("xdotool", "mouseup", "1")
                    emit(b"\x1b[?1000l")
                    before = len(keys.read_bytes())
                    run("xdotool", "key", "--delay", "60", "shift+Prior", "shift+Next", "ctrl+shift+Home", "ctrl+shift+End")
                    expected = b"\x1b[5;2~\x1b[6;2~\x1b[1;6H\x1b[1;6F"
                    wait_for(lambda: keys.read_bytes()[before:] == expected, "alternate-screen navigation was swallowed")
                    emit(b"\x1b[?1049l")
                    print("PASS: screen/mouse tracking transitions cancel local dragging; alternate shortcuts reach the PTY")
            except Exception:
                log.flush()
                print((root / "aura.log").read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                run("xdotool", "keyup", "a", "mouseup", "1")
                if app.poll() is None:
                    app.terminate()
                app.wait(timeout=8)
                os.close(descriptor)
                run("setxkbmap", "-layout", "us", "-variant", "", "-option", "")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
