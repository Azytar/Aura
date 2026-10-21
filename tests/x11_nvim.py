#!/usr/bin/env python3
"""Drive an actual Neovim session: hover without buttons, insert and save."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from x11_daily_use import run, wait_for, window_for


def main():
    for tool in ["nvim", "xdotool", "xdpyinfo"]:
        if not shutil.which(tool):
            print(f"SKIP: {tool} unavailable")
            return 77
    if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        print("SKIP: no dedicated X11 display")
        return 77
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/aura").resolve())
    with tempfile.TemporaryDirectory(prefix="aura-nvim-") as directory:
        root = Path(directory)
        (root / "aura").mkdir()
        (root / "aura/config.toml").write_text('[window]\ncolumns=60\nlines=16\n[cursor]\nblink=false\n')
        document = root / "document.txt"
        document.write_text("first line\nsecond line\nthird line\n")
        hover = root / "hover"
        ready = root / "ready"
        init = root / "init.lua"
        init.write_text(f'''vim.o.mouse = "a"
vim.o.mousemoveevent = true
vim.o.guicursor = "n-v-c:block,i:ver25"
vim.o.swapfile = false
vim.o.shadafile = "NONE"
vim.keymap.set("n", "<MouseMove>", function()
  local p = vim.fn.getmousepos()
  vim.fn.writefile({{tostring(p.screenrow) .. ":" .. tostring(p.screencol)}}, {str(hover)!r})
end)
vim.api.nvim_create_autocmd("VimEnter", {{callback = function()
  vim.fn.writefile({{"ready"}}, {str(ready)!r})
end}})
''')
        shell = root / "shell"
        shell.write_text(f'#!/bin/sh\nexec "{shutil.which("nvim")}" --clean -u "{init}" "{document}"\n')
        shell.chmod(0o755)
        with (root / "stderr").open("wb") as log:
            process = subprocess.Popen([binary], env=dict(os.environ, SHELL=str(shell), XDG_CONFIG_HOME=str(root), XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"), NVIM_LOG_FILE=str(root / "nvim.log")), stdout=log, stderr=log)
            try:
                window = window_for(process)
                wait_for(ready.exists, "Neovim did not finish startup")
                run("xdotool", "mousemove", "--window", window, "40", "40")
                wait_for(hover.exists, "Neovim did not receive button-free MouseMove")
                row, column = map(int, hover.read_text().strip().split(":"))
                assert row > 0 and column > 0
                run("xdotool", "key", "i")
                run("xdotool", "type", "--delay", "20", "aura-native-")
                run("xdotool", "key", "Escape")
                run("xdotool", "type", "--delay", "20", ":w")
                run("xdotool", "key", "Return")
                wait_for(lambda: document.read_text().startswith("aura-native-first line"), "Neovim insert/save round trip changed bytes")
                run("xdotool", "type", "--delay", "20", ":q!")
                run("xdotool", "key", "Return")
                assert process.wait(timeout=8) == 0
                print("PASS actual Neovim MouseMove without buttons, insert/normal transition, save and clean exit")
            except Exception:
                log.flush()
                print((root / "stderr").read_text(errors="replace"), file=sys.stderr)
                print("Document:", repr(document.read_text()), file=sys.stderr)
                artifacts = os.environ.get("AURA_ARTIFACTS")
                if artifacts:
                    from x11_daily_use import capture
                    target = Path(artifacts)
                    target.mkdir(parents=True, exist_ok=True)
                    capture(window, target / "nvim-failure.xwd")
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=8)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
