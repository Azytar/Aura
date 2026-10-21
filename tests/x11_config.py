#!/usr/bin/env python3
"""Exercise configured child startup, imports, bindings and atomic live reload."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from x11_daily_use import capture, run, wait_for
from x11_protocols import Fixture, cursor_is_hidden


def main():
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-config-") as directory:
        root = Path(directory)
        (root / "aura").mkdir()
        (root / "work").mkdir()
        config_path = root / "aura/config.toml"
        theme = root / "aura/theme.toml"
        theme.write_text('[colors]\nbackground="#203040"\nforeground="#ffaa33"\n')
        config = f'''[general]
import = ["theme.toml"]
[window]
columns=20
lines=8
title="CustomTitle"
class="CustomClass"
dynamic_title=false
[cursor]
blink=false
[mouse]
hide_when_typing=false
[shell]
program={json.dumps(str(root / "shell"))}
args=["configured"]
working_directory={json.dumps(str(root / "work"))}
[env]
AURA_CONFIG_TEST="child-value"
[keyboard]
use_default_bindings=false
[[keyboard.bindings]]
key="F6"
chars="bound"
'''
        fixture = Fixture(root, binary, b"\x1b[?25l", config, argv=["--config-file", str(config_path)], complete_config=True, title="CustomTitle")
        try:
            metadata = json.loads((root / "metadata").read_text())
            assert metadata == {"cwd": str(root / "work"), "args": ["configured"], "probe": "child-value"}, metadata
            assert b"CustomClass" in run("xprop", "-id", fixture.window, "WM_CLASS")
            run("xdotool", "mousemove", "--window", fixture.window, "30", "30")
            run("xdotool", "key", "F6")
            wait_for(lambda: fixture.data() == b"bound", "configured binding not sent")
            assert not cursor_is_hidden()

            def background(color):
                image = capture(fixture.window, root / "frame.xwd")
                at = ((image[1] // 2) * image[0] + image[0] // 2) * 3
                return image[2][at:at + 3] == bytes.fromhex(color)

            wait_for(lambda: background("203040"), "imported theme not applied")
            theme.write_text('[colors]\nbackground="#102030"\nforeground="#ffaa33"\n')
            wait_for(lambda: background("102030"), "imported theme did not reload")
            theme.write_text('[colors]\nbackground="invalid"\n')
            wait_for(lambda: "reload rejected" in (root / "stderr").read_text(), "invalid reload was not rejected")
            assert background("102030"), "invalid reload changed the active theme"
            theme.write_text('[colors]\nbackground="#304050"\nforeground="#ffaa33"\n')
            wait_for(lambda: background("304050"), "corrected import did not reload")
            config_path.write_text(config.replace('class="CustomClass"', 'class="ReloadedClass"').replace('chars="bound"', 'chars="new"').replace('hide_when_typing=false', 'hide_when_typing=true'))
            wait_for(lambda: b"ReloadedClass" in run("xprop", "-id", fixture.window, "WM_CLASS"), "window/config reload not applied")
            start = len(fixture.data())
            run("xdotool", "key", "F6")
            wait_for(lambda: fixture.data()[start:] == b"new", "binding did not reload")
            wait_for(cursor_is_hidden, "mouse policy did not reload")
            result = subprocess.run([binary, "--config-file", str(config_path), "--check-config"], capture_output=True, env=dict(os.environ, DISPLAY=":65534"), timeout=10)
            assert result.returncode == 0 and b"configuration OK" in result.stdout, result.stderr
            print("PASS configured shell/cwd/env, file selection, imports, bindings, pointer policy and atomic live reload")
        finally:
            fixture.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
