#!/usr/bin/env python3
"""Check opt-in logging and unchanged raw PTY input on a dedicated X11 display."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from x11_daily_use import run, wait_for, window_for


def main():
    if len(sys.argv) != 3 or sys.argv[2] not in ["debug", "release"]:
        raise SystemExit("usage: x11_diagnostics.py BINARY debug|release")
    binary = str(Path(sys.argv[1]).resolve())
    debug = sys.argv[2] == "debug"
    for tool in ["xdotool", "xdpyinfo", "setxkbmap"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} unavailable")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no dedicated X11 display")
        return 77
    base = dict(os.environ)
    for name in ["AURA_DEBUG_KEYBOARD", "AURA_PERF"]:
        base.pop(name, None)
    for name in ["AURA_DEBUG_KEYBOARD", "AURA_PERF"]:
        for value in ["", "false", "2"]:
            result = subprocess.run([binary], env=dict(base, **{name: value}, DISPLAY=":65534"), capture_output=True, timeout=10)
            assert result.returncode != 0 and f"{name} must be 0 or 1".encode() in result.stderr, result.stderr
    # Release must not even contain the compiled keyboard format strings.
    if not debug:
        image = Path(binary).read_bytes()
        for trace in [b"DEBUG keyboard device=", b"DEBUG reload:", b"DEBUG key event:"]:
            assert trace not in image, trace
    combinations = [(None, None), ("0", "0"), ("0", "1"), ("1", "0"), ("1", "1")]
    with tempfile.TemporaryDirectory(prefix="aura-diagnostics-") as directory:
        root = Path(directory)
        for case, (keyboard, perf) in enumerate(combinations):
            home = root / str(case)
            (home / "aura").mkdir(parents=True)
            (home / "aura/config.toml").write_text('[window]\ncolumns=20\nlines=8\n[cursor]\nblink=false\n')
            received = home / "input"
            helper = home / "shell"
            helper.write_text(f'''#!{sys.executable}
import os, select, tty, signal
tty.setraw(0)
def size_changed(*_):
    size = os.get_terminal_size(0)
    os.write(1, f"\\x1b]0;SIZE:{{size.columns}}x{{size.lines}}\\a".encode())
signal.signal(signal.SIGWINCH, size_changed)
data = b""
os.write(1, b"\\x1b]0;RAW_READY\\a")
while True:
    ready, _, _ = select.select([0], [], [], 0.1)
    if ready:
        piece = os.read(0, 4096)
        if not piece: break
        data += piece
        with open({str(received)!r}, "wb") as output: output.write(data)
    os.write(1, b".\\r")
''')
            helper.chmod(0o755)
            settings = dict(base, SHELL=str(helper), XDG_CONFIG_HOME=str(home))
            for name, value in [("AURA_DEBUG_KEYBOARD", keyboard), ("AURA_PERF", perf)]:
                if value is not None: settings[name] = value
            log_path = home / "stderr"
            with log_path.open("wb") as log:
                process = subprocess.Popen([binary], env=settings, stdout=log, stderr=log)
                try:
                    window = window_for(process)
                    wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"RAW_READY", "raw helper not ready")
                    run("setxkbmap", "us")
                    run("xdotool", "key", "a", "shift+b", "ctrl+a", "Return")
                    expected = b"aB\x01\r"
                    wait_for(lambda: received.exists() and received.read_bytes() == expected, "raw input bytes changed")
                    # Exercise window resize, font resize/reflow and surface rebuild.
                    run("xdotool", "windowsize", "--sync", window, "420", "240")
                    wait_for(lambda: run("xdotool", "getwindowname", window).strip().startswith(b"SIZE:"), "window resize did not reach PTY")
                    run("xdotool", "key", "ctrl+plus")
                    if perf == "1":
                        stages = ["parse", "font_resize", "window_resize", "zoom_reflow", "frame_total", "prepare", "fence_wait", "acquire", "upload", "record_submit", "present_call", "surface_rebuild"]
                        wait_for(lambda: all(f"aura perf: {stage} n=" in log_path.read_text() for stage in stages), "missing performance stages", timeout=12)
                    assert received.read_bytes() == expected, received.read_bytes()
                except Exception:
                    log.flush()
                    print(log_path.read_text(errors="replace"), file=sys.stderr)
                    raise
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                assert process.returncode == 0, log_path.read_text()
            text = log_path.read_text()
            traces = debug and keyboard == "1"
            for message in ["DEBUG keyboard device=", "DEBUG reload:", "DEBUG key event:"]:
                assert (message in text) == traces, (message, text)
            assert ("aura perf:" in text) == (perf == "1"), text
            assert "frames/s" not in text
            print(f"PASS {sys.argv[2]} keyboard={keyboard} perf={perf}: exact raw input and gated diagnostics")
    print("PASS invalid flags rejected before X11/PTY startup")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
