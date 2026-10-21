#!/usr/bin/env python3
"""Native protocol regressions on a dedicated X11 display; raw PTY, no echo."""
import ctypes
import ctypes.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from x11_daily_use import capture, run, wait_for, window_for


class Fixture:
    def __init__(self, root, binary, output, config=""):
        self.root = root
        (root / "aura").mkdir(parents=True)
        (root / "aura/config.toml").write_text('[window]\ncolumns=20\nlines=8\npadding_x=4\npadding_y=6\n[cursor]\nblink=false\n' + config)
        self.received = root / "input"
        fifo = root / "control"
        os.mkfifo(fifo)
        self.control = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
        helper = root / "shell"
        helper.write_text(f'''#!{sys.executable}
import os,select,tty
tty.setraw(0)
fd=os.open({str(fifo)!r},os.O_RDONLY | os.O_NONBLOCK)
os.write(1,{output!r}+b"\\x1b]0;READY\\a")
pending=b""
while True:
    ready,_,_=select.select([0,fd],[],[])
    for source in ready:
        piece=os.read(source,16384)
        if not piece: raise SystemExit(0)
        if source==0:
            with open({str(self.received)!r},"ab") as log:log.write(piece)
        else:
            pending+=piece
            while b"\\n" in pending:
                line,pending=pending.split(b"\\n",1)
                os.write(1,bytes.fromhex(line.decode()))
''')
        helper.chmod(0o755)
        self.log = (root / "stderr").open("wb")
        self.process = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(root)), stdout=self.log, stderr=self.log)
        self.window = window_for(self.process)
        wait_for(lambda: run("xdotool", "getwindowname", self.window).strip() == b"READY", "raw PTY not ready")

    def emit(self, data, title):
        os.write(self.control, (data + b"\x1b]0;" + title.encode() + b"\a").hex().encode() + b"\n")
        wait_for(lambda: run("xdotool", "getwindowname", self.window).strip() == title.encode(), "output not parsed")

    def data(self):
        return self.received.read_bytes() if self.received.exists() else b""

    def close(self):
        self.process.terminate()
        try:
            assert self.process.wait(timeout=8) == 0
        finally:
            self.log.close()
            os.close(self.control)


def cursor_is_hidden():
    class Image(ctypes.Structure):
        _fields_ = [("x", ctypes.c_short), ("y", ctypes.c_short), ("width", ctypes.c_ushort), ("height", ctypes.c_ushort), ("xhot", ctypes.c_ushort), ("yhot", ctypes.c_ushort), ("serial", ctypes.c_ulong), ("pixels", ctypes.POINTER(ctypes.c_ulong))]
    x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
    fixes = ctypes.CDLL(ctypes.util.find_library("Xfixes"))
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XFree.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.restype = ctypes.POINTER(Image)
    display = x11.XOpenDisplay(None)
    assert display
    image = fixes.XFixesGetCursorImage(display)
    try:
        assert image
        return not any(image.contents.pixels[i] >> 24 for i in range(image.contents.width * image.contents.height))
    finally:
        x11.XFree(image)
        x11.XCloseDisplay(display)


def main():
    for tool in ["xdotool", "xdpyinfo", "xmessage"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool}")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: dedicated X11 unavailable")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-protocols-") as directory:
        fixture = Fixture(Path(directory) / "mouse", binary, b"\x1b[?1003;1004;1006h")
        other = None
        try:
            run("xdotool", "mousemove", "--window", fixture.window, "30", "38")
            wait_for(lambda: b"\x1b[<35;" in fixture.data(), "hover without buttons not delivered")
            run("xdotool", "mousedown", "1")
            run("xdotool", "mouseup", "1")
            wait_for(lambda: b"m" in fixture.data(), "button release missing")
            fixture.emit(b"\x1b[?1016h", "PIXELS")
            start = len(fixture.data())
            run("xdotool", "mousemove", "--window", fixture.window, "42", "50")
            wait_for(lambda: b"\x1b[<35;39;45M" in fixture.data()[start:], "SGR pixel coordinates differ from padded text origin")
            start = len(fixture.data())
            run("xdotool", "keydown", "Shift_L")
            run("xdotool", "mousemove", "--window", fixture.window, "60", "60")
            run("xdotool", "mousedown", "1")
            run("xdotool", "mouseup", "1")
            run("xdotool", "keyup", "Shift_L")
            fixture.emit(b"", "SHIFT_DONE")
            assert b"\x1b[<" not in fixture.data()[start:], fixture.data()[start:]
            run("xdotool", "key", "a")
            wait_for(lambda: fixture.data().endswith(b"a"), "typing did not reach PTY")
            wait_for(cursor_is_hidden, "pointer remained visible while typing")
            run("xdotool", "mousemove", "--window", fixture.window, "61", "60")
            wait_for(lambda: not cursor_is_hidden(), "pointer did not return on motion")
            other = subprocess.Popen(["xmessage", "-title", "aura-focus-fixture", "focus"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            def find_other():
                result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "^aura-focus-fixture$"], capture_output=True)
                return result.stdout.splitlines()[0].decode() if result.returncode == 0 else None
            other_window = wait_for(find_other, "focus fixture did not map")
            run("xdotool", "windowfocus", "--sync", other_window)
            wait_for(lambda: b"\x1b[O" in fixture.data(), "focus-out not delivered")
            start = len(fixture.data())
            run("xdotool", "windowfocus", "--sync", fixture.window)
            wait_for(lambda: b"\x1b[I" in fixture.data()[start:], "focus-in not delivered")
            print("PASS button-free hover, pixel mouse, release, Shift selection, typing pointer and focus")
        finally:
            if other is not None:
                other.terminate()
                other.wait(timeout=8)
            fixture.close()
        fixture = Fixture(Path(directory) / "queries", binary, b"\x1b[?1003h\x1b[?1003$p\x1b[?9999$p\x1b]10;?\x07\x1b[14t\x1b[16t\x1b[18t\x1bP+q4d73\x1b\\")
        try:
            expected = [b"\x1b[?1003;1$y", b"\x1b[?9999;0$y", b"\x1b]10;rgb:", b"\x1b[4;", b"\x1b[6;", b"\x1b[8;8;20t", b"\x1bP0+r4d73\x1b\\"]
            wait_for(lambda: all(part in fixture.data() for part in expected), "missing host capability replies")
            fixture.emit(b"\x1b[?25l\x1b[?2026h\x1b[48;2;17;34;51m\x1b[2J", "HELD")
            # The client deliberately never closes the batch. The host's bounded
            # recovery must eventually present the new background and clear mode.
            def recovered():
                image = capture(fixture.window, Path(directory) / "sync.xwd")
                at = ((image[1] // 2) * image[0] + image[0] // 2) * 3
                return image[2][at:at + 3] == bytes([17, 34, 51])
            wait_for(recovered, "stalled synchronized output never recovered")
            fixture.emit(b"\x1b[?2026$p", "SYNC_QUERY")
            wait_for(lambda: b"\x1b[?2026;2$y" in fixture.data(), "expired synchronized mode remained active")
            print("PASS host color/size/mode/termcap replies and stalled synchronized-output recovery")
            fixture.emit(b"\x1b[>31u\x1b[?u", "KEYBOARD")
            wait_for(lambda: b"\x1b[?31u" in fixture.data(), "keyboard negotiation failed")
            start = len(fixture.data())
            run("xdotool", "key", "shift+Return", "ctrl+Return", "a")
            wait_for(lambda: all(code in fixture.data()[start:] for code in [b"\x1b[13;2u", b"\x1b[13;5u", b"\x1b[97;1;97u", b"\x1b[97;1:3u"]), "modified Enter or key release encoding differs")
            fixture.emit(b"\x1b[<u", "LEGACY")
            start = len(fixture.data())
            run("xdotool", "key", "a", "Return")
            wait_for(lambda: fixture.data()[start:] == b"a\r", "legacy keyboard was not restored")
            print("PASS negotiated keyboard flags, modified Enter, associated text, releases and legacy restore")
        finally:
            fixture.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
