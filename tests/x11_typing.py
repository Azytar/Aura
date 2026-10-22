#!/usr/bin/env python3
"""Check local typing redraws independently of a raw child that never echoes."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

from x11_daily_use import capture, pixel, run, wait_for
from x11_protocols import Fixture


CONFIG = '''[window]
columns=20
lines=8
padding_x=4
padding_y=6
[cursor]
blink=false
[colors]
background="#102030"
foreground="#ffffff"
cursor="#ffffff"
selection="#305070"
'''


def typing_redraws(root, binary):
    history = b'\r\n'.join(f'line{index:03}'.encode() for index in range(60))
    with patch.dict(os.environ, AURA_PERF='1'):
        fixture = Fixture(root / 'steady', binary, history + b'\x1b[2J\x1b[4;1Habc\x1b[H',
                          config=CONFIG, complete_config=True)
    try:
        def image():
            return capture(fixture.window, root / 'cursor.xwd')
        def rates():
            return re.findall(rb'render_calls/s=([0-9.]+)', (fixture.root / 'stderr').read_bytes())
        def type_a():
            before = len(fixture.data())
            run('xdotool', 'key', 'a')
            wait_for(lambda: fixture.data()[before:] == b'a', 'raw input changed or never reached child')
        wait_for(lambda: rates(), 'initial performance interval missing')
        interval = len(rates())
        deadline = time.monotonic() + 8
        while len(rates()) == interval and time.monotonic() < deadline:
            type_a()
            # Span internal blink phases even though this cursor is steady.
            time.sleep(.11)
        assert len(rates()) > interval, 'typing interval missing'
        assert float(rates()[interval]) == 0, 'steady no-echo typing repainted unchanged content'
        original = image()
        cw, ch = (original[0] - 8) // 20, (original[1] - 12) // 8
        # Select visible text, away from the cursor. Clearing it must repaint
        # even though the child produces no echo or output after the key.
        y = 6 + 3 * ch + ch // 2  # Outside selection autoscroll hot zones.
        run('xdotool', 'mousemove', '--window', fixture.window, str(4 + cw // 2), str(y),
            'mousedown', '1', 'mousemove', '--window', fixture.window, str(4 + 2 * cw), str(y),
            'mouseup', '1')
        point = (4 + cw + 1, 6 + 4 * ch - 2)
        wait_for(lambda: pixel(image(), *point) == (48, 80, 112), 'selection witness not painted')
        type_a()
        wait_for(lambda: pixel(image(), *point) == (16, 32, 48), 'typing did not repaint cleared selection')
        bottom = image()
        run('xdotool', 'key', 'ctrl+shift+Home')
        wait_for(lambda: image() != bottom, 'history viewport did not move')
        type_a()
        wait_for(lambda: image() == bottom, 'no-echo typing did not restore the live viewport')
        print('PASS: unchanged no-echo typing has no render calls; selection and viewport still repaint')
    finally:
        fixture.close()
    # Blink is an independent local visual change; a non-echoing child must not
    # prevent a hidden blink phase from becoming visible immediately on typing.
    fixture = Fixture(root / 'blinking', binary, b'',
                      config=CONFIG.replace('blink=false', 'blink=true\nblink_interval_ms=1000\nblink_timeout_ms=0'),
                      complete_config=True)
    try:
        def cursor_is(color):
            frame = capture(fixture.window, root / 'blink.xwd')
            cw, ch = (frame[0] - 8) // 20, (frame[1] - 12) // 8
            return pixel(frame, 4 + cw // 2, 6 + ch // 2) == color
        wait_for(lambda: cursor_is((255, 255, 255)), 'initial cursor missing')
        wait_for(lambda: cursor_is((16, 32, 48)), 'cursor did not enter hidden blink phase')
        run('xdotool', 'key', 'a')
        wait_for(lambda: fixture.data() == b'a', 'blink fixture input differs')
        wait_for(lambda: cursor_is((255, 255, 255)), 'typing did not restore the hidden cursor')
        print('PASS: no-echo typing restores the blinking cursor')
    finally:
        fixture.close()


def main():
    missing = [name for name in ['xdotool', 'xdpyinfo', 'xwd', 'magick'] if not shutil.which(name)]
    if missing or subprocess.run(['xdpyinfo'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        if os.environ.get('AURA_REQUIRE_GPU') == '1':
            raise RuntimeError(f'required dedicated X11 prerequisites unavailable: {missing}')
        print(f'SKIP: dedicated X11 prerequisites unavailable: {missing}')
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/aura').resolve())
    with tempfile.TemporaryDirectory(prefix='aura-typing-') as folder:
        typing_redraws(Path(folder), binary)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
