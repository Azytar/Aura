#!/usr/bin/env python3
"""Exercise real XKB input, unscaled glyphs, htop and process cleanup on X11."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time


def run(*args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs).stdout


def wait_for(predicate, description, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.04)
    raise AssertionError(description)


def window_for(process):
    def find():
        if process.poll() is not None:
            raise AssertionError(f"aura exited during startup: {process.returncode}")
        result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)], capture_output=True)
        return result.stdout.splitlines()[0].decode() if result.returncode == 0 else None
    window = wait_for(find, "aura did not map a window")
    run("xdotool", "windowfocus", "--sync", window)
    return window


def capture(window, path):
    path.write_bytes(run("xwd", "-id", window, "-silent"))
    width, height = map(int, run("magick", "identify", "-format", "%w %h", str(path)).split())
    pixels = run("magick", str(path), "-depth", "8", "rgb:-")
    assert len(pixels) == width * height * 3
    return width, height, pixels


def crop(image, x, y, width, height):
    stride, _, pixels = image
    return b"".join(pixels[((y + row) * stride + x) * 3:((y + row) * stride + x + width) * 3] for row in range(height))


def pixel(image, x, y):
    width, _, data = image
    offset = (y * width + x) * 3
    return tuple(data[offset:offset + 3])


def main():
    for tool in ["xdotool", "setxkbmap", "xset", "xprop", "xwd", "magick", "htop", "xdpyinfo", "pgrep", "yes"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} is not installed")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no live X11 display")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    processes = []
    with tempfile.TemporaryDirectory(prefix="aura-x11-") as directory:
        root = Path(directory)
        config = root / "aura"
        config.mkdir()
        (config / "config.toml").write_text('''[font]
family = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
size = 13
letter_spacing = 2
line_spacing = 3
[font.bold]
style = "Bold"
[font.italic]
style = "Italic"
[font.bold_italic]
style = "Bold Italic"
[window]
columns = 40
lines = 12
padding_x = 3
padding_y = 5
''')
        keys = root / "keys.bin"
        helper = root / "shell"
        helper.write_text(f'''#!{sys.executable}
import os, tty
tty.setraw(0)
os.write(1, b"\\x1b[2J\\x1b[H\\x1b[?25l\\x1b[41mi\\x1b[42mi\\x1b[44mi\\x1b[0m\\x1b]0;AURA_READY\\a")
received = b""
stage = 0
with open(os.environ["AURA_TEST_KEYS"], "wb", buffering=0) as output:
    while True:
        data = os.read(0, 4096)
        if not data or b"\\x04" in data: break
        output.write(data)
        received += data
        if stage == 0 and received.endswith(b"sudo pacman -Syu\\r"):
            os.write(1, b"\\r\\n[sudo] password: ")
            stage = 1
        elif stage == 1 and received.endswith(b"test-secret\\r"):
            os.write(1, b"\\r\\n:: Synchronizing package databases...\\r\\n\\x1b[44mcore 12%\\r\\x1b[2Kcore 100%\\x1b[0m\\x1b]0;AURA_PROGRESS_\\xc3\\x9c\\a")
            stage = 2
        elif stage == 2 and received.endswith(b"graphics\\r"):
            patterns = "\\r\\n".join("".join(chr(0x2800 + row * 32 + column) for column in range(32)) for row in range(8))
            image = patterns + "\\r\\n" + "─" * 32 + "\\r\\n" + "█" * 32
            os.write(1, b"\\x1b[0m\\x1b[2J\\x1b[H" + image.encode() + b"\\x1b]0;AURA_GRAPHICS\\a")
            stage = 3
''')
        helper.chmod(0o755)
        environment = dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(root), AURA_TEST_KEYS=str(keys))
        log = open(root / "aura.log", "wb")
        try:
            run("setxkbmap", "-layout", "us", "-option", "")
            if re.search(rb"Caps Lock:\s+on", run("xset", "q")):
                run("xdotool", "key", "Caps_Lock")
            app = subprocess.Popen([binary], env=environment, stdout=log, stderr=log)
            processes.append(app)
            window = window_for(app)
            wait_for(lambda: keys.exists() and run("xdotool", "getwindowname", window).strip() == b"AURA_READY", "test shell not ready")
            run("xdotool", "key", "--delay", "60", "a", "shift+a", "Caps_Lock", "a", "shift+a", "Caps_Lock", "ctrl+Left", "shift+Tab", "F6")
            expected = b"aAAa\x1b[1;5D\x1b[Z\x1b[17~"
            wait_for(lambda: keys.read_bytes() == expected, f"US input mismatch (got {keys.read_bytes()!r})")
            print("PASS: Caps Lock, Shift, Ctrl+Left, backtab, F6 through the real PTY")
            run("setxkbmap", "-layout", "de", "-option", "")
            time.sleep(0.15)
            run("xdotool", "key", "--delay", "60", "z", "ISO_Level3_Shift+q")
            expected += b"z@"
            wait_for(lambda: keys.read_bytes() == expected, f"German/AltGr input mismatch (got {keys.read_bytes()!r})")
            print("PASS: live layout change and AltGr")
            run("setxkbmap", "-layout", "us", "-option", "")
            run("setxkbmap", "-layout", "us", "-variant", "intl", "-option", "")
            time.sleep(0.15)
            run("xdotool", "key", "--delay", "60", "dead_acute", "e")
            expected += "é".encode()
            wait_for(lambda: keys.read_bytes() == expected, "dead key composition changed")
            run("setxkbmap", "-layout", "us", "-variant", "", "-option", "")
            print("PASS: real XKB dead key composition")
            hints = run("xprop", "-id", window, "WM_NORMAL_HINTS")
            match = re.search(rb"resize increment: (\d+) by (\d+)", hints)
            assert match, hints
            cell_w, cell_h = map(int, match.groups())
            assert cell_w > 2 and cell_h > 3
            time.sleep(0.15)
            before = capture(window, root / "before.xwd")
            assert pixel(before, 0, 0) == (24, 24, 24), "padding uses the wrong background"
            for column, color in enumerate([(205, 0, 0), (0, 205, 0), (0, 0, 238)]):
                assert pixel(before, 3 + column * cell_w, 5 + cell_h - 1) == color
            original = crop(before, 3, 5, 3 * cell_w, cell_h)
            run("xdotool", "windowsize", window, str(before[0] + 7), str(before[1] + 11))
            time.sleep(0.25)
            after = capture(window, root / "after.xwd")
            assert crop(after, 3, 5, 3 * cell_w, cell_h) == original, "i glyphs stretched or backgrounds moved after a fractional-cell resize"
            assert pixel(after, after[0] - 1, 0) == (24, 24, 24)
            print("PASS: individual i glyphs, colors, spacing, padding and fractional-cell resize")
            # A deterministic protocol fixture, without invoking a package manager.
            # It exercises the exact command spelling, no-echo password input,
            # carriage-return progress, background erasure and a UTF-8 OSC title.
            run("xdotool", "type", "--delay", "35", "sudo pacman -Syu")
            run("xdotool", "key", "Return")
            expected += b"sudo pacman -Syu\r"
            wait_for(lambda: keys.read_bytes() == expected, "pacman command spelling changed")
            run("xdotool", "type", "--delay", "35", "test-secret")
            run("xdotool", "key", "Return")
            expected += b"test-secret\r"
            wait_for(lambda: keys.read_bytes() == expected, "no-echo password input changed")
            wait_for(lambda: run("xdotool", "getwindowname", window).strip() == "AURA_PROGRESS_Ü".encode(), "UTF-8 progress title was truncated")
            assert app.poll() is None
            print("PASS: sudo/pacman protocol fixture, uppercase S, no-echo input, CR progress and UTF-8 OSC")
            run("xdotool", "type", "--delay", "35", "graphics")
            run("xdotool", "key", "Return")
            expected += b"graphics\r"
            wait_for(lambda: keys.read_bytes() == expected and run("xdotool", "getwindowname", window).strip() == b"AURA_GRAPHICS", "graphics fixture did not render")
            time.sleep(0.2)
            graphics = capture(window, root / "graphics.xwd")
            dots = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)]
            for pattern in range(256):
                for bit, (x, y) in enumerate(dots):
                    px = 3 + (pattern % 32) * cell_w + (2 * x + 1) * cell_w // 4
                    py = 5 + (pattern // 32) * cell_h + (2 * y + 1) * cell_h // 8
                    assert (pixel(graphics, px, py)[0] > 44) == bool(pattern & (1 << bit)), f"Braille pattern {pattern:#x}, bit {bit} changed"
            for y in range(cell_h):
                for x in range(32 * cell_w):
                    assert pixel(graphics, 3 + x, 5 + 8 * cell_h + y) == pixel(graphics, 3, 5 + 8 * cell_h + y), "box drawing has a seam"
                    assert pixel(graphics, 3 + x, 5 + 9 * cell_h + y) == (216, 216, 216), "full blocks leave spacing gaps"
            run("xdotool", "windowsize", window, str(graphics[0] + 7), str(graphics[1] + 11))
            time.sleep(0.25)
            resized = capture(window, root / "graphics-resized.xwd")
            assert crop(graphics, 3, 5, 32 * cell_w, 10 * cell_h) == crop(resized, 3, 5, 32 * cell_w, 10 * cell_h)
            print("PASS: all 256 Braille patterns, continuous borders/full blocks and fractional resize through the native swapchain")
            child_ids = run("pgrep", "-P", str(app.pid)).split()
            app.terminate()
            assert app.wait(timeout=8) == 0
            assert all(not Path(f"/proc/{pid.decode()}").exists() for pid in child_ids), "shell was not reaped"
            print("PASS: SIGTERM closes the PTY and reaps its shell")
            environment["SHELL"] = shutil.which("htop")
            monitor = subprocess.Popen([binary], env=environment, stdout=log, stderr=log)
            processes.append(monitor)
            window = window_for(monitor)
            time.sleep(0.5)
            run("xdotool", "windowsize", window, "827", "491")
            run("xdotool", "key", "F2")
            time.sleep(0.25)
            shot = root / "htop.xwd"
            capture(window, shot)
            assert int(run("magick", "identify", "-format", "%k", str(shot))) > 32
            run("xdotool", "key", "Escape", "ctrl+plus", "ctrl+minus")
            run("xdotool", "windowsize", window, "6000", "491")
            time.sleep(0.2)
            assert monitor.poll() is None, "window exceeded the GPU grid budget"
            run("xdotool", "windowsize", window, "827", "491")
            time.sleep(0.2)
            run("xdotool", "key", "q")
            assert monitor.wait(timeout=8) == 0
            print("PASS: real htop, setup screen, resize, font zoom and clean exit")
            helper.write_text("#!/bin/sh\nexec yes\n")
            environment["SHELL"] = str(helper)
            flood = subprocess.Popen([binary], env=environment, stdout=log, stderr=log)
            processes.append(flood)
            window = window_for(flood)
            time.sleep(0.2)
            run("xdotool", "key", "ctrl+c")
            assert flood.wait(timeout=8) == 0
            print("PASS: Ctrl+C remains responsive during continuous yes output")
        except Exception:
            log.flush()
            print((root / "aura.log").read_text(errors="replace"), file=sys.stderr)
            raise
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            run("setxkbmap", "-layout", "us", "-option", "")
            log.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
