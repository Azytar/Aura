"""Isolated X11 launch configurations; no user configuration is read or edited."""

import json
import os
from pathlib import Path
import shutil
import subprocess
from common import sha256


def identify(entry):
    binary = shutil.which(entry["binary"])
    if not binary:
        return dict(status="unavailable", error="executable not found")
    binary = str(Path(binary).resolve())
    flag = "-version" if entry["kind"] == "xterm" else "--version"
    try:
        result = subprocess.run([binary, flag], capture_output=True, timeout=10,
                                env=dict(os.environ, NVIM_LOG_FILE=os.devnull))
        version = (
            (result.stdout + result.stderr).decode(errors="replace").strip()[:4096]
        )
        if result.returncode:
            return dict(
                status="unavailable",
                binary=binary,
                error=f"version probe exited {result.returncode}",
                version=version,
            )
    except (OSError, subprocess.TimeoutExpired) as error:
        return dict(status="unavailable", binary=binary, error=str(error))
    return dict(
        status="available", binary=binary, sha256=sha256(binary), version=version
    )


def identify_all(entries):
    identities = {entry["id"]: identify(entry) for entry in entries}
    hashes = {}
    for entry in entries:
        identity = identities[entry["id"]]
        if identity["status"] == "available":
            key = (entry["kind"], identity["sha256"])
            if key in hashes:
                raise ValueError(
                    f"{entry['id']} and {hashes[key]} use the same binary hash; build baseline/candidate separately"
                )
            hashes[key] = entry["id"]
    return identities


def launch(kind, binary, directory, config, child, title, spacing=(0, 0)):
    directory = Path(directory)
    family = json.dumps(config["font_family"], ensure_ascii=False)
    font_file = json.dumps(config["font_file"], ensure_ascii=False)
    points = config["font_pixels"] * 72 / config["dpi"]
    cols, rows, history = config["columns"], config["rows"], config["history"]
    # HOME and XDG paths are supplied by the runner; absolute command arguments
    # keep the helper independent of login-shell syntax and inherited dotfiles.
    if kind == "aura":
        path = directory / "config.toml"
        path.write_text(f"""[general]
live_config_reload=false
[font]
family={font_file}
size={config["font_pixels"]}
letter_spacing={spacing[0]}
line_spacing={spacing[1]}
[window]
columns={cols}
lines={rows}
padding_x=0
padding_y=0
[cursor]
blink=false
[scrollback]
lines={history}
[shell]
program={json.dumps(child[0], ensure_ascii=False)}
args={json.dumps(child[1:], ensure_ascii=False)}
""")
        return [binary, "--config-file", str(path)]
    if kind == "alacritty":
        path = directory / "alacritty.toml"
        path.write_text(f"""[general]
live_config_reload=false
[window]
dimensions={{columns={cols},lines={rows}}}
padding={{x=0,y=0}}
dynamic_padding=false
[font]
normal={{family={family},style="Regular"}}
size={points}
offset={{x={spacing[0]},y={spacing[1]}}}
[cursor]
style={{shape="Block",blinking="Never"}}
[colors.primary]
foreground="#d8d8d8"
background="#181818"
[scrolling]
history={history}
[terminal]
shell={{program={json.dumps(child[0], ensure_ascii=False)},args={json.dumps(child[1:], ensure_ascii=False)}}}
""")
        return [binary, "--config-file", str(path), "--title", title]
    if kind == "kitty":
        adjustments = "\n".join(
            f"modify_font cell_{axis} {value:+}px"
            for axis, value in zip(["width", "height"], spacing)
            if value
        )
        path = directory / "kitty.conf"
        path.write_text(f"""font_family {config["font_family"]}
{adjustments}
font_size {points}
initial_window_width {cols}c
initial_window_height {rows}c
remember_window_size no
window_padding_width 0
window_margin_width 0
placement_strategy top-left
window_border_width 0
cursor_blink_interval 0
scrollback_lines {history}
sync_to_monitor yes
linux_display_server x11
confirm_os_window_close 0
foreground #d8d8d8
background #181818
""")
        return [binary, "--config", str(path), "--title", title, *child]
    if kind == "wezterm":
        path = directory / "wezterm.lua"
        # Lua quoted strings use the common JSON escaping subset for these paths.
        # Font family validation excludes line breaks; Unicode remains literal.
        path.write_text(f"""local w = require 'wezterm'
return {{ font=w.font({family}, {{weight='Regular'}}), font_size={points}, dpi={config["dpi"]},
initial_cols={cols}, initial_rows={rows}, scrollback_lines={history},
window_padding={{left=0,right=0,top=0,bottom=0}}, enable_tab_bar=false,
window_decorations='NONE', cursor_blink_rate=0, enable_wayland=false,
check_for_updates=false, automatically_reload_config=false,
colors={{foreground='#d8d8d8',background='#181818'}} }}
""")
        return [
            binary,
            "--config-file",
            str(path),
            "start",
            "--always-new-process",
            "--",
            *child,
        ]
    if kind == "xterm":
        return [
            binary,
            "-u8",
            "-fg",
            "#d8d8d8",
            "-bg",
            "#181818",
            "-geometry",
            f"{cols}x{rows}",
            "-fa",
            config["font_family"],
            "-fs",
            str(points),
            "-sl",
            str(history),
            "-b",
            "0",
            "+sb",
            "-title",
            title,
            "-xrm",
            "XTerm*cursorBlink:false",
            "-xrm",
            f"Xft.dpi:{config['dpi']}",
            "-e",
            *child,
        ]
    raise ValueError(f"unsupported adapter: {kind}")
