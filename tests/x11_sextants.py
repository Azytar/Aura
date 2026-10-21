#!/usr/bin/env python3
"""Verify native sextant pixels and cached color updates against fresh rendering."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from x11_daily_use import capture, run, wait_for, window_for


def main():
    missing = [name for name in ["xdotool", "xdpyinfo", "xwd", "magick", "vulkaninfo"] if not shutil.which(name)]
    if missing:
        if os.environ.get("AURA_REQUIRE_GPU") == "1":
            raise RuntimeError(f"required graphics tools missing: {missing}")
        print(f"SKIP: {missing}")
        return 77
    for command in [["xdpyinfo"], ["vulkaninfo", "--summary"]]:
        if subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            if os.environ.get("AURA_REQUIRE_GPU") == "1":
                raise RuntimeError(f"required graphics unavailable: {command}")
            print(f"SKIP: {command}")
            return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    names = [line.split() for line in (Path(__file__).parent / "campaign/sextant-cells.txt").read_text().splitlines() if not line.startswith("#")]
    assert len(names) == 60
    sizes = [9, 12, 17, 18, 23, 36] if os.environ.get("AURA_PROFILE") == "exhaustive" else [14, 18]
    first, final, background = (244, 247, 73), (80, 210, 240), (24, 24, 24)

    def frame(color):
        data = f"\x1b[?25l\x1b[38;2;{color[0]};{color[1]};{color[2]}m".encode()
        for row in range(6):
            text = "".join(chr(int(cp, 16)) for cp, _ in names[row * 10:(row + 1) * 10])
            data += f"\x1b[{row + 1};1H".encode() + text.encode()
        return data + "\x1b[8;10H█".encode()

    with tempfile.TemporaryDirectory(prefix="aura-sextants-") as directory:
        root = Path(directory)
        processes, descriptors, logs = [], [], []
        try:
            def launch(label, size, data):
                home = root / label
                (home / "aura").mkdir(parents=True)
                (home / "aura/config.toml").write_text(f'''[font]
family = "DejaVu Sans Mono"
size = {size}
letter_spacing = 1
line_spacing = 3
offset_x = 9
offset_y = -7
[window]
columns = 10
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
    readable, _, _ = select.select([0, fd], [], [])
    for source in readable:
        piece = os.read(source, 8192)
        if not piece: raise SystemExit(0)
        if source == fd:
            pending += piece
            while b"\\n" in pending:
                line, pending = pending.split(b"\\n", 1)
                os.write(1, bytes.fromhex(line.decode()))
''')
                helper.chmod(0o755)
                log = (home / "stderr").open("wb")
                logs.append(log)
                process = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(helper), XDG_CONFIG_HOME=str(home)), stdout=log, stderr=log)
                processes.append(process)
                window = window_for(process)
                wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"READY", "raw fixture not ready")
                return process, window, descriptor

            def presented(window, path, color):
                def witness():
                    image = capture(window, path)
                    cw, ch = (image[0] - 8) // 10, (image[1] - 10) // 8
                    at = ((5 + 7 * ch + ch // 2) * image[0] + 4 + 9 * cw + cw // 2) * 3
                    return image if image[2][at:at + 3] == bytes(color) else None
                return wait_for(witness, "full-block witness was not presented")

            def check(image, color):
                cw, ch = (image[0] - 8) // 10, (image[1] - 10) // 8
                for index, (cp, digits) in enumerate(names):
                    for y in range(ch):
                        for x in range(cw):
                            coverage = 0.0
                            for digit in digits:
                                sixth = int(digit) - 1
                                left, top = (sixth % 2) * cw / 2, (sixth // 2) * ch / 3
                                coverage += max(0, min(x + 1, left + cw / 2) - max(x, left)) * max(0, min(y + 1, top + ch / 3) - max(y, top))
                            expected = [round(bg + (fg - bg) * min(1, coverage)) for fg, bg in zip(color, background)]
                            at = ((5 + index // 10 * ch + y) * image[0] + 4 + index % 10 * cw + x) * 3
                            actual = image[2][at:at + 3]
                            assert all(abs(a - b) <= 1 for a, b in zip(actual, expected)), f"U+{cp} sixths={digits} pixel={x},{y}: {list(actual)} != {expected}"

            for size in sizes:
                cached, window, control = launch(f"cached-{size}", size, frame(first))
                check(presented(window, root / "first.xwd", first), first)
                os.write(control, (frame(final) + b"\x1b]0;UPDATED\a").hex().encode() + b"\n")
                wait_for(lambda: run("xdotool", "getwindowname", window).strip() == b"UPDATED", "color update not parsed")
                actual = presented(window, root / "updated.xwd", final)
                check(actual, final)
                fresh, reference, _ = launch(f"fresh-{size}", size, frame(final))
                expected = presented(reference, root / "fresh.xwd", final)
                check(expected, final)
                assert actual == expected, f"cached sextant frame differs from fresh renderer at size={size}"
                for process in [cached, fresh]:
                    process.terminate()
                    assert process.wait(timeout=8) == 0
                print(f"PASS native 60 sextants, initial/color update/fresh exact comparison, size={size}")
        except Exception:
            for log in logs:
                log.flush()
            for path in root.glob("*/stderr"):
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
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
