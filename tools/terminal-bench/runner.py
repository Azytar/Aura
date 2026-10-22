"""Sequential paired campaigns. No network, user dotfiles, or shell interpolation."""

from contextlib import contextmanager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import random
import select
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from xml.sax.saxutils import escape

import adapters
from common import outside_checkout, require_hash, sha256
from resources import Resources
from x11 import X11

HERE = Path(__file__).resolve().parent


def save(path, value):
    # A single writer owns a newly created run directory. Replace snapshots so
    # an interrupted write cannot leave the last completed trial unreadable.
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    temporary.replace(path)


def stop(process):
    if process is None or process.poll() is not None:
        return
    # Every launch owns a fresh process group. Never signal an unrelated PID
    # discovered by process enumeration or a terminal's pre-existing server.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=3)


@contextmanager
def display_session(requested, xvfb, output, config):
    if not xvfb:
        if not requested:
            raise ValueError("pass --display for a dedicated server, or use --xvfb")
        yield requested
        return
    read_fd, write_fd = os.pipe()
    process = None
    width = max(1280, int(config["columns"] * config["font_pixels"] + 64))
    height = max(800, int(config["rows"] * config["font_pixels"] * 2 + 64))
    if width > 8192 or height > 8192:
        os.close(read_fd)
        os.close(write_fd)
        raise ValueError("requested Xvfb screen exceeds 8192 pixels; reduce grid/font")
    try:
        with (output / "xvfb.log").open("wb") as log:
            process = subprocess.Popen(
                [
                    "Xvfb",
                    "-displayfd",
                    str(write_fd),
                    "-screen",
                    "0",
                    f"{width}x{height}x24",
                    "-dpi",
                    str(int(config["dpi"])),
                    "-nolisten",
                    "tcp",
                ],
                pass_fds=(write_fd,),
                stdout=log,
                stderr=log,
                start_new_session=True,
            )
        os.close(write_fd)
        write_fd = -1
        if not select.select([read_fd], [], [], 10)[0]:
            raise TimeoutError("Xvfb startup timed out")
        number = os.read(read_fd, 64).decode().strip()
        if not number.isdecimal():
            raise RuntimeError("Xvfb did not return a display number; inspect xvfb.log")
        yield ":" + number
    finally:
        os.close(read_fd)
        if write_fd >= 0:
            os.close(write_fd)
        stop(process)


class Channel:
    def __init__(self, connection, resources, timeout):
        self.connection = connection
        self.resources = resources
        self.timeout = timeout
        self.buffer = b""
        connection.setblocking(False)

    def receive(self):
        deadline = time.monotonic() + self.timeout
        while b"\n" not in self.buffer:
            self.resources.sample()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("control reply timed out")
            if select.select([self.connection], [], [], min(0.01, remaining))[0]:
                data = self.connection.recv(65536)
                if not data:
                    raise EOFError("PTY helper disconnected")
                self.buffer += data
                if len(self.buffer) > 65536:
                    raise ValueError("control reply exceeded 64 KiB")
        line, self.buffer = self.buffer.split(b"\n", 1)
        result = json.loads(line)
        if result.get("op") == "error":
            raise RuntimeError(result["error"])
        return result

    def send(self, request):
        data = json.dumps(request).encode() + b"\n"
        # Requests are bounded below the minimum Unix stream send buffer.
        self.connection.sendall(data)

    def request(self, op, **values):
        self.send(dict(op=op, **values))
        reply = self.receive()
        if reply.get("op") != op:
            raise ValueError(f"expected {op} reply, got {reply.get('op')}")
        return reply


def window_for(title, display, resources, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        resources.sample()
        result = subprocess.run(
            ["xdotool", "search", "--onlyvisible", "--name", "^" + title + "$"],
            env=dict(os.environ, DISPLAY=display),
            capture_output=True,
            timeout=3,
        )
        if result.returncode == 0:
            windows = result.stdout.split()
            if len(windows) != 1:
                raise RuntimeError("unique trial title matched multiple windows")
            return int(windows[0])
        time.sleep(0.01)
    raise TimeoutError("benchmark window not mapped")


def wait_pixel(x11, window, point, rgb, resources, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if x11.pixel(window, *point) == rgb:
            return time.monotonic_ns()
        resources.sample()
        time.sleep(0.001)
    raise TimeoutError(f"presented witness {rgb} not observed")


def input_sample(channel, x11, window, point, rgb, resources, timeout):
    channel.send(dict(op="input", rgb=rgb))
    if channel.receive().get("op") != "armed":
        raise ValueError("helper did not arm the input sample")
    before = x11.key()
    reply = channel.receive()
    if reply.get("op") != "input":
        raise ValueError("incorrect input acknowledgement")
    pixel_time = wait_pixel(x11, window, point, rgb, resources, timeout)
    return dict(
        child_ms=(reply["received_ns"] - before) / 1e6,
        pixel_ms=(pixel_time - before) / 1e6,
        injected_ns=before,
        observed_ns=pixel_time,
    )


def input_color(index):
    return [
        29 + index % 101,
        67 + (index * 37) % 151,
        31 + (index * 53) % 191,
    ]


def trial(
    entry, identity, directory, config, settings, display, x11, expected_font_hash=None
):
    directory.mkdir()
    for name in ["home", "config", "data", "cache", "state"]:
        (directory / name).mkdir()
    result = dict(terminal=entry["id"], status="failed", metrics={}, phases={})
    process = None
    with tempfile.TemporaryDirectory(prefix="aura-bench-control-") as control:
        server = socket.socket(socket.AF_UNIX)
        server.bind(str(Path(control) / "peer"))
        server.listen(1)
        title = "AURA_BENCH_" + uuid.uuid4().hex
        child = [
            sys.executable,
            str(directory.parent / "harness-source" / "child.py"),
            str(Path(control) / "peer"),
            title,
        ]
        command = adapters.launch(
            entry["kind"],
            identity["binary"],
            directory,
            config,
            child,
            title,
            entry.get("cell_spacing", [0, 0]),
        )
        result["command"] = command
        environment = dict(
            os.environ,
            DISPLAY=display,
            HOME=str(directory / "home"),
            XDG_CONFIG_HOME=str(directory / "config"),
            XDG_DATA_HOME=str(directory / "data"),
            XDG_CACHE_HOME=str(directory / "cache"),
            XDG_STATE_HOME=str(directory / "state"),
            WINIT_UNIX_BACKEND="x11",
            WINIT_X11_SCALE_FACTOR=str(config["dpi"] / 96),
            PYTHONDONTWRITEBYTECODE="1",
            LANG="C.UTF-8",
            LC_ALL="C.UTF-8",
            FONTCONFIG_FILE=str(directory.parent / "fonts.conf"),
        )
        for name in [
            "WAYLAND_DISPLAY",
            "AURA_PERF",
            "AURA_DEBUG_KEYBOARD",
            "WEZTERM_UNIX_SOCKET",
            "KITTY_LISTEN_ON",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "WINDOWID",
        ]:
            environment.pop(name, None)
        connection = None
        try:
            if identity.get("sha256"):
                require_hash(identity["binary"], identity["sha256"])
            if expected_font_hash:
                require_hash(config["font_file"], expected_font_hash)
            start = time.monotonic_ns()
            with (directory / "terminal.log").open("wb") as log:
                process = subprocess.Popen(
                    command,
                    env=environment,
                    cwd=directory,
                    stdout=log,
                    stderr=log,
                    start_new_session=True,
                )
            resources = Resources(process.pid)
            deadline = time.monotonic() + settings["timeout"]
            while not select.select([server], [], [], 0.01)[0]:
                resources.sample()
                if process.poll() is not None:
                    raise RuntimeError(
                        f"terminal exited {process.returncode} before readiness"
                    )
                if time.monotonic() >= deadline:
                    raise TimeoutError("terminal startup timed out")
            connection, _ = server.accept()
            channel = Channel(connection, resources, settings["timeout"])
            hello = channel.receive()
            ready = time.monotonic_ns()
            if hello.get("op") != "hello":
                raise ValueError("missing helper handshake")
            if not hello.get("unicode_geometry_checked"):
                raise ValueError("helper did not verify wide/combining cursor geometry")
            result["checks"] = {"unicode_geometry": True}
            resources.exclude.add(hello["pid"])
            resources.ticks = {
                key: value
                for key, value in resources.ticks.items()
                if key[0] != hello["pid"]
            }
            resources.peak = 0
            initial = hello["size"]
            if [initial["columns"], initial["rows"]] != [
                config["columns"],
                config["rows"],
            ]:
                raise ValueError(
                    f"initial PTY geometry {initial} does not match requested grid"
                )
            result["metrics"]["startup_ready_ms"] = [(ready - start) / 1e6]
            result["initial_pty"] = initial
            window = window_for(title, display, resources, settings["timeout"])
            width, height = x11.geometry(window)
            result["initial_window_pixels"] = [width, height]
            cols, rows = config["columns"], config["rows"]
            # Kitty can initially leave fractional-cell remainder pixels. Use the
            # PTY's text extent, pin content top-left, then normalize the client
            # rectangle before measured work. No font size is changed.
            text_width, text_height = initial["pixel_width"], initial["pixel_height"]
            if (
                text_width
                and text_height
                and text_width % cols == 0
                and text_height % rows == 0
            ):
                cw, ch = text_width // cols, text_height // rows
            elif width % cols == 0 and height % rows == 0:
                cw, ch = width // cols, height // rows
            else:
                raise ValueError("no exact cell geometry; check padding/decorations")
            width, height = cw * cols, ch * rows
            if result["initial_window_pixels"] != [width, height]:
                x11.resize(window, width, height)
                x11.sync()
                channel.request(
                    "size", expected=[cols, rows], timeout=settings["timeout"]
                )
            if x11.geometry(window) != [width, height]:
                raise ValueError(
                    "window manager did not accept normalized cell geometry"
                )
            result["cell_pixels"] = [cw, ch]
            result["window_pixels"] = [width, height]
            x11.focus(window)
            # PTY readiness can precede the first rendered content. Measure
            # an explicit witness too, and let startup drawing finish before
            # the idle CPU interval begins.
            initial_rgb = [41, 83, 167]
            channel.request("paint", rgb=initial_rgb)
            observed = wait_pixel(
                x11,
                window,
                [width - max(1, cw // 2), height - max(1, ch // 2)],
                initial_rgb,
                resources,
                settings["timeout"],
            )
            result["metrics"]["startup_observed_pixel_ms"] = [(observed - start) / 1e6]
            result["checks"]["startup_witness"] = True
            # Every trial starts with the same controlled idle period and no cursor blink.
            phase = resources.begin()
            until = time.monotonic() + settings["idle_seconds"]
            while time.monotonic() < until:
                resources.sample()
                time.sleep(min(0.01, max(0, until - time.monotonic())))
            idle = resources.end(phase)
            result["phases"]["idle"] = idle
            result["metrics"]["idle_cpu_percent"] = [
                100 * idle["cpu_seconds"] / idle["wall_seconds"]
            ]
            result["metrics"]["idle_rss_mib"] = [idle["sampled_peak_rss_mib"]]
            for workload in ["ascii", "unicode", "sgr", "tui"]:
                phase = resources.begin()
                reply = channel.request(
                    "output", kind=workload, bytes=settings["payload_bytes"]
                )
                result["phases"][workload] = resources.end(phase) | reply
                if (
                    reply["elapsed_ns"] <= 0
                    or reply["bytes"] < settings["payload_bytes"]
                ):
                    raise ValueError("invalid output sample")
                result["metrics"][workload + "_mib_s"] = [
                    reply["bytes"] * 1e9 / reply["elapsed_ns"] / 1048576
                ]
            phase = resources.begin()
            channel.request("prefill", lines=config["history"] + rows)
            result["phases"]["prefill"] = resources.end(phase)
            resize = []
            phase = resources.begin()
            for _ in range(settings["resize_pairs"]):
                for target in [(cols - 7, rows - 2), (cols, rows)]:
                    channel.send(
                        dict(op="size", expected=target, timeout=settings["timeout"])
                    )
                    before = x11.resize(window, cw * target[0], ch * target[1])
                    reply = channel.receive()
                    if reply.get("op") != "size" or [
                        reply["size"]["columns"],
                        reply["size"]["rows"],
                    ] != list(target):
                        raise ValueError("incorrect resize acknowledgement")
                    resize.append((reply["observed_ns"] - before) / 1e6)
            result["phases"]["resize"] = resources.end(phase)
            result["metrics"]["resize_ack_ms"] = resize
            child_samples, observed = [], []
            phase = resources.begin()
            point = [width - max(1, cw // 2), height - max(1, ch // 2)]
            for index in range(settings["input_samples"]):
                sample = input_sample(
                    channel, x11, window, point, input_color(index), resources,
                    settings["timeout"],
                )
                child_samples.append(sample["child_ms"])
                observed.append(sample["pixel_ms"])
            result["phases"]["input"] = resources.end(phase)
            result["metrics"]["key_to_child_ms"] = child_samples
            result["metrics"]["key_to_observed_pixel_ms"] = observed
            phase = resources.begin()
            paused, gaps = [], []
            previous_witness = sample["observed_ns"]
            for index in range(settings["paused_input_samples"]):
                # The pause follows the previous observed witness and stays
                # outside the injection-to-pixel clock. It is not proof of idle.
                time.sleep(settings["input_pause_seconds"])
                sample = input_sample(
                    channel, x11, window, point,
                    input_color(settings["input_samples"] + index), resources,
                    settings["timeout"],
                )
                paused.append(sample["pixel_ms"])
                gaps.append((sample["injected_ns"] - previous_witness) / 1e6)
                previous_witness = sample["observed_ns"]
            result["phases"]["input_after_pause"] = resources.end(phase)
            result["metrics"]["key_to_observed_pixel_after_pause_ms"] = paused
            result["input_after_pause_gaps_ms"] = gaps
            phase = resources.begin()
            channel.request("load_start", timeout=settings["timeout"])
            loaded_samples = []
            for _ in range(settings["input_samples"]):
                channel.send(dict(op="input", under_load=True))
                if channel.receive().get("op") != "armed":
                    raise ValueError("helper did not arm input under load")
                before = x11.key()
                reply = channel.receive()
                if reply.get("op") != "input":
                    raise ValueError("incorrect input-under-load acknowledgement")
                loaded_samples.append((reply["received_ns"] - before) / 1e6)
            channel.request("load_stop")
            result["phases"]["input_under_output"] = resources.end(phase)
            result["metrics"]["key_to_child_under_output_ms"] = loaded_samples
            result["metrics"]["load_cpu_seconds"] = [
                sum(
                    phase["cpu_seconds"]
                    for name, phase in result["phases"].items()
                    if name not in ("idle", "input_after_pause")
                )
            ]
            result["metrics"]["sampled_peak_rss_mib"] = [resources.peak / 1048576]
            result["resource_samples"] = resources.samples
            channel.request("quit")
            try:
                exit_code = process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                raise RuntimeError("terminal did not exit after its child") from None
            if exit_code:
                raise RuntimeError(f"terminal exited with status {exit_code}")
            if identity.get("sha256"):
                require_hash(identity["binary"], identity["sha256"])
            if expected_font_hash:
                require_hash(config["font_file"], expected_font_hash)
            result["checks"].update(
                raw_input=True, observed_witness=True, pty_resize=True
            )
            result["status"] = "ok"
        except (
            OSError,
            ValueError,
            RuntimeError,
            TimeoutError,
            EOFError,
            subprocess.SubprocessError,
        ) as error:
            result["error"] = str(error)
        finally:
            if connection:
                connection.close()
            server.close()
            stop(process)
    return result


def inventory(display, output):
    values = dict(
        system=list(os.uname()),
        python=sys.version.split()[0],
        cpu_count=os.cpu_count(),
        display=display,
        resource_scope="observed terminal process tree; helper and X server excluded after readiness",
        resource_sampling_ms=10,
        clock_ticks_per_second=os.sysconf("SC_CLK_TCK"),
        load_average=list(os.getloadavg()),
        presentation="unmeasured; X11 readback is a host-observation upper bound",
    )
    try:
        values["cpu_model"] = next(
            line.split(":", 1)[1].strip()
            for line in Path("/proc/cpuinfo").read_text().splitlines()
            if line.startswith("model name")
        )
    except (OSError, StopIteration):
        values["cpu_model"] = None
    for name, command in [
        ("gl", ["glxinfo", "-B"]),
        ("vulkan", ["vulkaninfo", "--summary"]),
    ]:
        try:
            reply = subprocess.run(
                command,
                env=dict(os.environ, DISPLAY=display),
                capture_output=True,
                timeout=15,
            )
            text = (reply.stdout + reply.stderr).decode(errors="replace")
            (output / f"{name}-inventory.txt").write_text(text)
            values[name] = dict(
                exit_code=reply.returncode,
                software_mentions=any(
                    v in text.lower()
                    for v in ["llvmpipe", "lavapipe", "software rasterizer"]
                ),
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            values[name] = dict(error=str(error))
    return values


def run(
    config,
    settings,
    output,
    display=None,
    xvfb=False,
    environment="unknown",
    geometry_policy="cell",
):
    output = outside_checkout(output)
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    font_home = output / "font-home"
    font_home.mkdir()
    font_cache = output / "font-cache"
    font_cache.mkdir()
    font_config = output / "fonts.conf"
    font_config.write_text(
        '<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">'
        "<fontconfig><include>/etc/fonts/fonts.conf</include><dir>"
        + escape(str(Path(config["font_file"]).parent))
        + "</dir><cachedir>"
        + escape(str(font_cache))
        + "</cachedir></fontconfig>"
    )
    font_environment = dict(
        os.environ,
        FONTCONFIG_FILE=str(font_config),
        HOME=str(font_home),
        XDG_CONFIG_HOME=str(font_home),
        XDG_CACHE_HOME=str(font_cache),
    )
    match = subprocess.run(
        ["fc-match", "-f", "%{file}", config["font_family"]],
        env=font_environment,
        capture_output=True,
        timeout=15,
    )
    matched_font = match.stdout.decode(errors="replace").strip()
    if (
        match.returncode
        or not Path(matched_font).is_file()
        or sha256(matched_font) != sha256(config["font_file"])
    ):
        raise ValueError(
            "Fontconfig does not resolve font_family to font_file; select the actual regular face"
        )
    identities = adapters.identify_all(config["terminal"])
    available = [
        entry
        for entry in config["terminal"]
        if identities[entry["id"]]["status"] == "available"
    ]
    manifest = dict(
        schema=1,
        created_utc=datetime.now(timezone.utc).isoformat(),
        config=config,
        settings=settings,
        geometry_policy=geometry_policy,
        terminal_identities=identities,
        trials=[],
        execution_order=[],
        font_sha256=sha256(config["font_file"]),
        fontconfig_match=matched_font,
        fontconfig_sha256=sha256(font_config),
        helper_sha256=sha256(HERE / "child.py"),
        harness_sha256={path.name: sha256(path) for path in HERE.glob("*.py")},
        status="running",
    )
    save(output / "run.json", manifest)
    try:
        snapshot = output / "harness-source"
        snapshot.mkdir()
        for name, expected in manifest["harness_sha256"].items():
            destination = snapshot / name
            shutil.copyfile(HERE / name, destination)
            require_hash(destination, expected)
        with display_session(display, xvfb, output, config) as selected:
            manifest["environment"] = inventory(selected, output) | dict(
                declared_rendering=environment, xvfb=xvfb
            )
            generator = random.Random(settings["seed"])
            x11 = X11(selected)
            try:
                for index in range(settings["warmups"] + settings["repetitions"]):
                    order = available.copy()
                    generator.shuffle(order)
                    manifest["execution_order"].append([entry["id"] for entry in order])
                    for entry in order:
                        directory = output / f"trial-{index:03}-{entry['id']}"
                        manifest["active_trial"] = dict(
                            terminal=entry["id"],
                            round=index,
                            warmup=index < settings["warmups"],
                            directory=directory.name,
                        )
                        save(output / "run.json", manifest)
                        result = trial(
                            entry,
                            identities[entry["id"]],
                            directory,
                            config,
                            settings,
                            selected,
                            x11,
                            manifest["font_sha256"],
                        )
                        result.update(
                            round=index,
                            warmup=index < settings["warmups"],
                            directory=directory.name,
                        )
                        manifest["trials"].append(result)
                        manifest.pop("active_trial", None)
                        save(output / "run.json", manifest)
                        print(
                            f"{entry['id']} round={index} warmup={result['warmup']} {result['status']}",
                            flush=True,
                        )
            finally:
                x11.close()
        failed = any(row["status"] != "ok" for row in manifest["trials"])
        missing_required = any(
            entry["required"] and identities[entry["id"]]["status"] != "available"
            for entry in config["terminal"]
        )
        manifest["status"] = (
            "failed" if failed or missing_required or len(available) < 2 else "complete"
        )
    except BaseException as error:
        manifest.update(
            status="interrupted" if isinstance(error, KeyboardInterrupt) else "failed",
            error=str(error),
        )
        raise
    finally:
        save(output / "run.json", manifest)
    return manifest
