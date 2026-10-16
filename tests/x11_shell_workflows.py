#!/usr/bin/env python3
"""Smoke-test real interactive programs on a dedicated X11 display."""

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

from x11_daily_use import capture, run, wait_for, window_for


def main():
    for tool in ["bash", "xdotool", "xdpyinfo", "xwd", "magick", "clear", "reset"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} is not installed")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no live X11 display")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-shell-") as directory:
        root = Path(directory)
        helper = root / "shell"
        helper.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc -i\n")
        helper.chmod(0o755)
        environment = dict(os.environ, SHELL=str(helper), PS1="\\[\\e]0;AURA_BASH_READY\\a\\]test> ", XDG_CONFIG_HOME=str(root))
        with open(root / "aura.log", "wb") as log:
            app = subprocess.Popen([binary], env=environment, stdout=log, stderr=log)
            try:
                window = window_for(app)

                def title(value):
                    wait_for(lambda: run("xdotool", "getwindowname", window).strip() == value.encode(), f"title did not become {value}")

                def type_text(text):
                    run("xdotool", "type", "--clearmodifiers", "--delay", "35", text)

                def enter(command):
                    type_text(command)
                    run("xdotool", "key", "Return")

                title("AURA_BASH_READY")
                edited = root / "edited"
                type_text("printf '%s' ac")
                run("xdotool", "key", "Left")
                type_text("b")
                run("xdotool", "key", "ctrl+e")
                type_text(" > " + shlex.quote(str(edited)))
                run("xdotool", "key", "Return")
                wait_for(lambda: edited.exists() and edited.read_text() == "abc", "bash line editing changed the command")
                print("PASS: real interactive bash line editing with Left and Ctrl+E")

                password = root / "password-result"
                enter("printf '\\e[?25l\\e]0;AURA_PASSWORD\\a'; read -rs secret; printf '%s' \"$secret\" > " + shlex.quote(str(password)) + "; unset secret")
                title("AURA_PASSWORD")
                time.sleep(0.1)
                before = capture(window, root / "password-before.xwd")
                # Synthetic test text, never an account credential.
                type_text("Test-42")
                time.sleep(0.1)
                assert capture(window, root / "password-after.xwd") == before, "no-echo input painted password characters"
                run("xdotool", "key", "Return")
                wait_for(lambda: password.exists() and password.read_bytes() == b"Test-42", "no-echo read lost input")
                title("AURA_BASH_READY")
                print("PASS: actual bash read -s receives the password without echo")

                for command in ["clear", "reset"]:
                    marker = "AURA_" + command.upper()
                    enter(command + "; printf '\\e[?25l\\e]0;" + marker + "\\a'; read -rsn1")
                    title(marker)
                    time.sleep(0.15)
                    image = capture(window, root / (command + ".xwd"))
                    pixels = image[2]
                    assert all(pixels[index:index + 3] == pixels[:3] for index in range(0, len(pixels), 3)), command + " left visible text or residue"
                    run("xdotool", "key", "x")
                    title("AURA_BASH_READY")
                    print(f"PASS: real {command} produces a blank framebuffer and returns to bash")

                if shutil.which("vi"):
                    document = root / "document"
                    document.write_text("first line\n")
                    enter("printf '\\e]0;AURA_VI\\a'; vi -u NONE -i NONE " + shlex.quote(str(document)))
                    title("AURA_VI")
                    time.sleep(0.2)
                    run("xdotool", "windowsize", window, "827", "491")
                    run("xdotool", "key", "G", "o")
                    type_text("edited in vi")
                    run("xdotool", "key", "Escape")
                    enter(":wq")
                    wait_for(lambda: "edited in vi" in document.read_text(), "vi did not save inserted text")
                    title("AURA_BASH_READY")
                    print("PASS: real vi insert/save/quit and live resize")
                else:
                    print("SKIP: vi is not installed")

                if shutil.which("less"):
                    document = root / "pager"
                    document.write_text("".join(f"line {index}\n" for index in range(200)))
                    enter("printf '\\e]0;AURA_LESS\\a'; less " + shlex.quote(str(document)))
                    title("AURA_LESS")
                    time.sleep(0.2)
                    run("xdotool", "key", "Next", "slash")
                    enter("line 150")
                    run("xdotool", "windowsize", window, "907", "547")
                    run("xdotool", "key", "q")
                    title("AURA_BASH_READY")
                    assert app.poll() is None
                    print("PASS: real less paging/search/resize/quit")
                else:
                    print("SKIP: less is not installed")
                enter("exit")
                assert app.wait(timeout=8) == 0
            except Exception:
                log.flush()
                print((root / "aura.log").read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=8)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
