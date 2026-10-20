#!/usr/bin/env python3
"""Compare live dirty-row rendering with a fresh process and fresh GPU caches."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from x11_daily_use import capture, run, wait_for, window_for


def main():
    required = os.environ.get("AURA_REQUIRE_GPU") == "1"
    font = Path(os.environ.get("AURA_TEST_SYMBOL_FONT", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"))
    missing = [tool for tool in ["xdotool", "xdpyinfo", "xwd", "magick", "vulkaninfo"] if not shutil.which(tool)]
    if missing or not font.exists():
        if required:
            raise RuntimeError(f"required GPU prerequisites missing: {missing}, font={font.exists()}")
        print(f"SKIP GPU prerequisites: {missing}, font={font.exists()}")
        return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode or subprocess.run(["vulkaninfo", "--summary"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode:
        if required:
            raise RuntimeError("required X11/Vulkan unavailable")
        print("SKIP X11/Vulkan unavailable")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    profile = os.environ.get("AURA_PROFILE", "quick")
    sizes = {"quick": [17, 18], "extended": [12, 17, 18, 23], "exhaustive": [9, 12, 17, 18, 23, 36]}[profile]
    initial = b"\x1b[?25l\x1b[31mABCD\r\n\x1b[44m12345678"
    updates = "\x1b[1;2H界\x1b[2;3H\x1b[3X\x1b[0m\x1b[3;1H⣇█╬i⏎↵😀↩\ufe0f\x1b[1;7H\x1b[4;7mX\x1b[0m".encode()
    output = Path(os.environ.get("AURA_ARTIFACTS", "target/campaign"))
    with tempfile.TemporaryDirectory(prefix="aura-dirty-") as directory:
        root = Path(directory)
        processes = []
        descriptors = []
        logs = []
        try:
            def launch(name, size, data):
                home = root / name
                (home / "aura").mkdir(parents=True)
                (home / "aura/config.toml").write_text(f'''[font]
family = "{font}"
size = {size}
letter_spacing = 1
line_spacing = 3
offset_x = 1
offset_y = -1
[window]
columns = 12
lines = 8
padding_x = 4
padding_y = 5
[cursor]
blink = false
''')
                fifo = home / "control"
                os.mkfifo(fifo)
                descriptor = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
                descriptors.append(descriptor)
                helper = home / "shell"
                helper.write_text(f'''#!{sys.executable}
import os, select, tty
tty.setraw(0)
fd = os.open({str(fifo)!r}, os.O_RDONLY | os.O_NONBLOCK)
os.write(1, {data!r} + b"\\x1b]0;READY\\a")
pending = b""
while True:
    ready, _, _ = select.select([0, fd], [], [])
    for source in ready:
        piece = os.read(source, 4096)
        if source == 0:
            if not piece: raise SystemExit(0)
        else:
            pending += piece
            while b"\\n" in pending:
                line, pending = pending.split(b"\\n", 1)
                os.write(1, bytes.fromhex(line.decode()))
''')
                helper.chmod(0o755)
                log = open(home / "stderr.log", "wb")
                logs.append(log)
                app = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(home)), stdout=log, stderr=log)
                processes.append(app)
                window = window_for(app)
                wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"READY", "initial frame not ready")
                return app, window, descriptor

            def emit(window, descriptor, data):
                os.write(descriptor, (data + b"\x1b]0;UPDATED\a").hex().encode() + b"\n")
                wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"UPDATED", "updated frame not ready")
                time.sleep(0.1)

            for size in sizes:
                cached, window, control = launch(f"cached-{size}", size, initial)
                time.sleep(0.1)
                capture(window, root / "initial.xwd")
                emit(window, control, updates)
                actual = capture(window, root / "actual.xwd")
                fresh, reference_window, _ = launch(f"fresh-{size}", size, initial + updates)
                run("xdotool", "windowraise", reference_window)
                time.sleep(0.15)
                expected = capture(reference_window, root / "expected.xwd")
                # A fresh renderer can share a defect with the cached path.
                # Independently require the erased cell's ANSI-blue background
                # before using its text pixels as a differential reference.
                cw, ch = (expected[0] - 8) // 12, (expected[1] - 10) // 8
                at = ((5 + 2 * ch - 1) * expected[0] + 4 + 3 * cw) * 3
                if expected[2][at:at+3] != bytes([0, 0, 238]):
                    output.mkdir(parents=True, exist_ok=True)
                    oracle = bytearray(expected[2])
                    oracle[at:at+3] = bytes([0, 0, 238])
                    for label, pixels in [("expected", oracle), ("actual", expected[2]), ("diff", bytes(abs(a-b) for a,b in zip(oracle, expected[2])))]:
                        (output / f"native-content-{size}-{label}.ppm").write_bytes(f"P6\n{expected[0]} {expected[1]}\n255\n".encode() + pixels)
                    raise AssertionError("native reference missing required blue background")

                def compare(actual, stage):
                    if actual == expected:
                        return
                    output.mkdir(parents=True, exist_ok=True)
                    for label, image in [("expected", expected), ("actual", actual)]:
                        (output / f"native-{stage}-{size}-{label}.ppm").write_bytes(f"P6\n{image[0]} {image[1]}\n255\n".encode() + image[2])
                    if actual[:2] == expected[:2]:
                        diff = bytes(abs(a-b) for a,b in zip(actual[2], expected[2]))
                        (output / f"native-{stage}-{size}-diff.ppm").write_bytes(f"P6\n{actual[0]} {actual[1]}\n255\n".encode() + diff)
                    raise AssertionError(f"native {stage} differs from fresh process at size={size}")

                compare(actual, "dirty")
                # This fixture leaves spare rows/columns, so zoom cannot
                # discard content below the cursor. General resize round trips
                # are not content-conservation properties for bounded grids.
                # Compare dirty-row updates BEFORE any full invalidation; a
                # font rebuild must not mask a missing incremental update.
                # Xvfb does not preserve pixels of an occluded X window.
                # Raise it above the reference before capturing its drawable.
                run("xdotool", "windowraise", window)
                run("xdotool", "windowfocus", "--sync", window)
                run("xdotool", "key", "ctrl+plus")
                time.sleep(0.1)
                run("xdotool", "key", "ctrl+0")
                time.sleep(0.15)
                compare(capture(window, root / "zoom.xwd"), "zoom")
                cached.terminate()
                fresh.terminate()
                assert cached.wait(timeout=8) == 0 and fresh.wait(timeout=8) == 0
                print(f"PASS native incremental/fresh font={font.name} size={size} pixels={actual[0]}x{actual[1]}")
        except Exception:
            for log in logs:
                log.flush()
            for path in root.glob("*/stderr.log"):
                print(path.read_text(errors="replace"), file=sys.stderr)
            raise
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            for descriptor in descriptors:
                os.close(descriptor)
            for log in logs:
                log.close()
    print(f"COMPLETE native GPU profile={profile} cases={len(sizes)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
