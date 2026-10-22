#!/usr/bin/env python3
"""Check themed pointers and history selection on a dedicated X11 display."""

import ctypes
import ctypes.util
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

from x11_daily_use import run, wait_for
from x11_protocols import Fixture


class CursorImage(ctypes.Structure):
    _fields_ = [
        ("x", ctypes.c_short),
        ("y", ctypes.c_short),
        ("width", ctypes.c_ushort),
        ("height", ctypes.c_ushort),
        ("xhot", ctypes.c_ushort),
        ("yhot", ctypes.c_ushort),
        ("serial", ctypes.c_ulong),
        ("pixels", ctypes.POINTER(ctypes.c_ulong)),
    ]


def cursor_image():
    x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
    fixes = ctypes.CDLL(ctypes.util.find_library("Xfixes"))
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XFree.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.restype = ctypes.POINTER(CursorImage)
    display = x11.XOpenDisplay(None)
    assert display
    image = fixes.XFixesGetCursorImage(display)
    try:
        assert image
        value = image.contents
        return (
            value.width,
            value.height,
            value.xhot,
            value.yhot,
            tuple(
                value.pixels[index] & 0xFFFFFFFF
                for index in range(value.width * value.height)
            ),
        )
    finally:
        x11.XFree(image)
        x11.XCloseDisplay(display)


def write_cursor(path, color):
    # Two real Xcursor image chunks let the server verify size selection as
    # well as theme discovery; no installed desktop theme is assumed.
    chunks = []
    toc = []
    position = 40
    for size in [24, 48]:
        chunk = struct.pack("<9I", 36, 0xFFFD0002, size, 1, size, size, 1, 2, 0)
        chunk += struct.pack("<I", color) * (size * size)
        toc.append(struct.pack("<3I", 0xFFFD0002, size, position))
        chunks.append(chunk)
        position += len(chunk)
    path.write_bytes(
        struct.pack("<4I", 0x72756358, 16, 0x10000, 2) + b"".join(toc + chunks)
    )


def themed_pointer(root, binary):
    themes = root / "themes"
    cursors = themes / "aura-test" / "cursors"
    cursors.mkdir(parents=True)
    write_cursor(cursors / "xterm", 0xFF123456)
    write_cursor(cursors / "left_ptr", 0xFF654321)
    saved = run("xrdb", "-query")
    try:
        run("xrdb", "-merge", input=b"Xcursor.theme: aura-test\nXcursor.size: 24\n")
        for label, variables, size in [
            ("resources", {}, 24),
            ("environment", {"XCURSOR_THEME": "aura-test", "XCURSOR_SIZE": "48"}, 48),
        ]:
            environment = {
                key: value
                for key, value in os.environ.items()
                if key not in ["XCURSOR_THEME", "XCURSOR_SIZE"]
            }
            environment.update(XCURSOR_PATH=str(themes), **variables)
            with patch.dict(os.environ, environment, clear=True):
                fixture = Fixture(
                    root / label,
                    binary,
                    b"",
                    config="[window]\ncolumns=20\nlines=8\n[cursor]\nblink=false\n",
                    complete_config=True,
                )
            try:
                run("xdotool", "mousemove", "--window", fixture.window, "40", "40")
                expected = (size, size, 1, 2, (0xFF123456,) * (size * size))
                wait_for(
                    lambda: cursor_image() == expected,
                    "text pointer ignored theme, size or hotspot",
                )
                fixture.emit(b"\x1b[?1003h", "MOUSE")
                expected = (size, size, 1, 2, (0xFF654321,) * (size * size))
                wait_for(
                    lambda: cursor_image() == expected,
                    "application pointer ignored theme",
                )
                fixture.emit(b"\x1b[?1003l", "TEXT")
                expected = (size, size, 1, 2, (0xFF123456,) * (size * size))
                wait_for(
                    lambda: cursor_image() == expected, "text pointer was not restored"
                )
            finally:
                fixture.close()
        print(
            "PASS: Xresources theme/size, environment size override, pointer modes and exact ARGB/hotspots"
        )
        broken = themes / "broken" / "cursors"
        broken.mkdir(parents=True)
        (broken / "xterm").write_bytes(b"invalid cursor")
        for label in ["missing", "broken"]:
            with patch.dict(os.environ, XCURSOR_THEME=label, XCURSOR_PATH=str(themes)):
                fixture = Fixture(root / label, binary, b"")
            try:
                run("xdotool", "mousemove", "--window", fixture.window, "40", "40")

                def fallback_visible():
                    image = cursor_image()
                    opaque = {pixel for pixel in image[4] if pixel >> 24}
                    return (
                        image[0] > 1
                        and image[1] > 1
                        and 0xFF000000 in opaque
                        and opaque <= {0xFF000000, 0xFFFFFFFF}
                    )

                wait_for(
                    fallback_visible,
                    "missing/malformed theme did not fall back to a visible core cursor",
                )
            finally:
                fixture.close()
        print("PASS: missing and malformed cursor themes retain a visible fallback")
    finally:
        run("xrdb", "-load", input=saved)


def history_selection(root, binary):
    output = b"\r\n".join(f"line{line:03}".encode() for line in range(60))
    fixture = Fixture(
        root / "selection",
        binary,
        output,
        config="[window]\ncolumns=40\nlines=12\npadding_x=0\npadding_y=0\n[cursor]\nblink=false\n",
        complete_config=True,
    )
    try:
        run("xdotool", "windowmove", fixture.window, "0", "0")
        geometry = dict(
            line.split("=", 1)
            for line in run("xdotool", "getwindowgeometry", "--shell", fixture.window)
            .decode()
            .splitlines()
        )
        height = int(geometry["HEIGHT"])
        cell = height // 12
        run(
            "xdotool",
            "mousemove",
            "--window",
            fixture.window,
            "0",
            str(cell * 10 + cell // 2),
            "mousedown",
            "1",
        )
        run("xdotool", "mousemove", "--window", fixture.window, "0", "0")

        def copied_text():
            run("xdotool", "key", "ctrl+shift+c")
            return run("xclip", "-o", "-selection", "clipboard", timeout=2)

        def partly_scrolled():
            text = copied_text()
            first = text.split(b"\n", 1)[0]
            return (
                text
                if first.startswith(b"line") and 20 <= int(first[4:]) <= 40
                else None
            )

        wait_for(partly_scrolled, "edge drag did not advance into retained history")
        run("xdotool", "mouseup", "1")
        released = run("xclip", "-o", "-selection", "primary", timeout=2)
        assert not released.startswith(b"line000"), (
            "release check reached the history limit"
        )
        time.sleep(0.2)  # An inactive drag must leave its copied endpoint stable.
        assert copied_text() == released, (
            "selection kept advancing after button release"
        )
        # Restore the live viewport before testing the complete history range.
        run("xdotool", "key", "ctrl+shift+End")
        run(
            "xdotool",
            "mousemove",
            "--window",
            fixture.window,
            "0",
            str(cell * 10 + cell // 2),
            "mousedown",
            "1",
        )
        # A wheel event while button 1 remains held must not end the drag.
        run("xdotool", "click", "4")
        run("xdotool", "mousemove", "--window", fixture.window, "0", "0")

        def copied_to_start():
            run("xdotool", "key", "ctrl+shift+c")
            value = subprocess.run(
                ["xclip", "-o", "-selection", "clipboard"],
                capture_output=True,
                timeout=2,
            )
            return (
                value.returncode == 0
                and value.stdout.startswith(b"line000\nline001\n")
                and b"line057" in value.stdout
            )

        wait_for(
            copied_to_start, "stationary top-edge drag never selected retained history"
        )
        run("xdotool", "mouseup", "1")
        primary = run("xclip", "-o", "-selection", "primary", timeout=2)
        assert primary.startswith(b"line000\nline001\n") and b"line057" in primary, (
            primary
        )
        run(
            "xdotool",
            "mousemove",
            "--window",
            fixture.window,
            "0",
            str(cell + cell // 2),
            "mousedown",
            "1",
        )
        run(
            "xdotool",
            "mousemove",
            "--window",
            fixture.window,
            str(int(geometry["WIDTH"]) - 1),
            str(height - 1),
        )

        def copied_to_end():
            run("xdotool", "key", "ctrl+shift+c")
            value = run("xclip", "-o", "-selection", "clipboard", timeout=2)
            return value.startswith(b"line001\n") and b"line059" in value

        wait_for(
            copied_to_end, "stationary bottom-edge drag did not return to live output"
        )
        run("xdotool", "mouseup", "1")
        print(
            "PASS: zero-padding screen-edge history selection, stationary timer, wheel during drag, PRIMARY and release"
        )
    finally:
        run("xdotool", "mouseup", "1")
        fixture.close()


def shifted_history_selection(root, binary):
    output = b"\r\n".join(f"line{line:03}".encode() for line in range(60))
    fixture = Fixture(
        root / "shift-selection",
        binary,
        output + b"\x1b[?1003;1006h",
        config="[window]\ncolumns=40\nlines=12\npadding_x=0\npadding_y=0\n[cursor]\nblink=false\n",
        complete_config=True,
    )
    try:
        run("xdotool", "windowmove", fixture.window, "0", "0")
        geometry = dict(
            line.split("=", 1)
            for line in run("xdotool", "getwindowgeometry", "--shell", fixture.window)
            .decode()
            .splitlines()
        )
        cell = int(geometry["HEIGHT"]) // 12
        run("xdotool", "keydown", "Shift_L")
        run(
            "xdotool", "mousemove", "--window", fixture.window, "0",
            str(cell * 10 + cell // 2), "mousedown", "1",
        )
        # Finish the gesture with Shift released. The application never got its
        # button press, so it must not get the rest of this local selection.
        start = len(fixture.data())
        run("xdotool", "mousemove", "--window", fixture.window, "0", "0")
        run("xdotool", "keyup", "Shift_L")

        # The anchor is column zero of line 58; selection includes that one
        # character, not the rest of its row.
        expected = b"\n".join(f"line{line:03}".encode() for line in range(58)) + b"\nl"

        def copied_to_start():
            run("xdotool", "key", "ctrl+shift+c")
            text = run("xclip", "-o", "-selection", "clipboard", timeout=2)
            return text == expected

        wait_for(
            copied_to_start, "Shift drag did not autoscroll over application mouse mode"
        )
        run("xdotool", "mouseup", "1")
        primary = run("xclip", "-o", "-selection", "primary", timeout=2)
        assert primary == expected, primary
        # An ordered input sentinel waits for queued reports to reach the raw
        # helper before checking that no part of this gesture leaked to it.
        run("xdotool", "key", "a")
        wait_for(lambda: fixture.data().endswith(b"a"), "release sentinel not delivered")
        assert fixture.data()[start:] == b"a", fixture.data()[start:]
        # Normal motion still belongs to the application after release.
        run("xdotool", "key", "ctrl+shift+End")
        run("xdotool", "mousemove", "--window", fixture.window, "20", "30")
        wait_for(
            lambda: b"\x1b[<35;" in fixture.data()[start:], "mouse reporting did not resume"
        )
        print("PASS: Shift history drag, modifier release, PRIMARY and application mouse ownership")
    finally:
        run("xdotool", "mouseup", "1", "keyup", "Shift_L")
        fixture.close()


def main():
    for tool in ["xdotool", "xdpyinfo", "xrdb", "xclip"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool}")
            return 77
    if subprocess.run(
        ["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ).returncode:
        print("SKIP: dedicated X11 unavailable")
        return 77
    binary = str(
        Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve()
    )
    with tempfile.TemporaryDirectory(prefix="aura-pointer-selection-") as directory:
        root = Path(directory)
        themed_pointer(root, binary)
        history_selection(root, binary)
        shifted_history_selection(root, binary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
