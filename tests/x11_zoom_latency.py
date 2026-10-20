#!/usr/bin/env python3
"""Measure key-to-PTY-resize latency, excluding initial history generation."""
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

from x11_daily_use import run, wait_for, window_for


def main():
    for tool in ["xdotool", "xdpyinfo"]:
        if not shutil.which(tool):
            print(f"SKIP: missing {tool}")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5).returncode:
        print("SKIP: no X11 display")
        return 77
    font = Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf")
    if not font.exists():
        print("SKIP: controlled DejaVu font unavailable")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-zoom-latency-") as directory:
        root = Path(directory)
        for history in [0, 1_000, 10_000]:
            home = root / str(history)
            (home / "aura").mkdir(parents=True)
            (home / "aura/config.toml").write_text(f'''[font]
family = "{font}"
size = 13
[window]
columns = 80
lines = 25
[scrollback]
lines = {history}
[cursor]
blink = false
''')
            events = home / "events"
            helper = home / "shell"
            helper.write_text(f'''#!{sys.executable}
import fcntl, json, os, signal, struct, sys, termios, time, tty
tty.setraw(0)
def resized(*args):
    size = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    with open({str(events)!r}, "a") as output:
        output.write(json.dumps([time.monotonic_ns(), *size]) + "\\n")
signal.signal(signal.SIGWINCH, resized)
for line in range({history} + 25):
    sys.stdout.buffer.write(f"├── directory-{{line:05}}/file-é界\\r\\n".encode())
sys.stdout.buffer.write(b"\\x1b[?25l\\x1b]0;READY\\a")
sys.stdout.buffer.flush()
while os.read(0, 4096): pass
''')
            helper.chmod(0o755)
            with open(home / "app.log", "wb") as log:
                app = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(home)), stdout=log, stderr=log)
                try:
                    window = window_for(app)
                    wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"READY", "history not ready")
                    time.sleep(0.2)
                    samples = []
                    sizes = []
                    for key in ["ctrl+plus", "ctrl+minus"] * 4:
                        previous = len(events.read_text().splitlines()) if events.exists() else 0
                        start = time.monotonic_ns()
                        run("xdotool", "key", "--clearmodifiers", "--delay", "0", key, timeout=3)
                        def next_resize():
                            rows = events.read_text().splitlines() if events.exists() else []
                            return json.loads(rows[-1]) if len(rows) > previous else None
                        row = wait_for(next_resize, "zoom did not notify PTY")
                        assert row[0] >= start and row[1] > 0 and row[2] > 0
                        samples.append((row[0] - start) / 1_000_000)
                        sizes.append(row[1:])
                        # Keep separate key presses separate: this benchmark
                        # measures completed operations, not coalesced repeats.
                        time.sleep(0.15)
                    result = dict(history=history, samples_ms=samples, median_ms=statistics.median(samples), max_ms=max(samples), winsizes=sizes)
                    print(json.dumps(result), flush=True)
                except Exception:
                    log.flush()
                    print((home / "app.log").read_text(errors="replace"), file=sys.stderr)
                    raise
                finally:
                    if app.poll() is None:
                        app.terminate()
                    try:
                        app.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        app.kill()
                        app.wait()
    print("MEASURED key delivery + font metrics + terminal resize + SIGWINCH; excludes GPU presentation")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
