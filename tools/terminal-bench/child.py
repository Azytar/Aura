#!/usr/bin/env python3
"""Synthetic raw PTY peer. The Unix socket carries control, never workload bytes."""

import fcntl
import json
import os
import re
import select
import signal
import socket
import struct
import sys
import termios
import time
import tty


def size():
    rows, cols, width, height = struct.unpack(
        "HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8)
    )
    return dict(columns=cols, rows=rows, pixel_width=width, pixel_height=height)


def write(data):
    view = memoryview(data)
    while view:
        count = os.write(1, view)
        if count <= 0:
            raise RuntimeError("PTY write made no progress")
        view = view[count:]


def read_input(timeout=10):
    if not select.select([0], [], [], timeout)[0]:
        raise TimeoutError("PTY input timed out")
    data = os.read(0, 4096)
    if not data:
        raise EOFError("PTY closed")
    return data


def query_position(row=1, column=1):
    write(b"\x1b[6n")
    data = b""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        data += read_input(max(0.001, deadline - time.monotonic()))
        match = re.search(rb"\x1b\[(\d+);(\d+)R", data)
        if match:
            if [int(match[1]), int(match[2])] != [row, column]:
                raise RuntimeError(f"cursor reply {match[0]!r} != {row},{column}")
            return
        if len(data) > 4096:
            break
    raise RuntimeError("missing/incorrect ordered DSR reply")


def barrier():
    write(b"\x1b[1;1H")
    query_position()


def payload(kind, budget, cols, rows):
    if kind == "ascii":
        block = b"x" * (cols - 1) + b"\r\n"
    elif kind == "unicode":
        block = ("ab界e\u0301" * max(1, (cols - 1) // 5) + "\r\n").encode()
    elif kind == "sgr":
        block = b"\x1b[38;2;12;171;52mcolored\x1b[0m plain\r\n"
    elif kind == "tui":
        block = b"".join(
            f"\x1b[{row};1H\x1b[44m".encode() + b" " * (cols - 1) + b"\x1b[0m"
            for row in range(1, rows + 1)
        )
    else:
        raise ValueError("unknown workload")
    return block * max(1, (budget + len(block) - 1) // len(block))


def witness(rgb, cols, rows):
    r, g, b = rgb
    write(f"\x1b[{rows};{cols}H\x1b[48;2;{r};{g};{b}m \x1b[0m\x1b[1;1H".encode())


def serve(path, title):
    tty.setraw(0)
    write(b"\x1b[?25l\x1b[0m")
    write(f"\x1b]0;{title}\x07".encode())
    barrier()
    write("ab界e\u0301".encode())
    query_position(1, 6)
    write(b"\x1b[2J\x1b[H")
    connection = socket.socket(socket.AF_UNIX)
    connection.connect(path)
    stream = connection.makefile("rwb", buffering=0)

    def send(message):
        stream.write(json.dumps(message).encode() + b"\n")

    writer = None

    def stop_writer():
        nonlocal writer
        if writer is not None:
            try:
                os.kill(writer, signal.SIGTERM)
            except ProcessLookupError:
                pass
            os.waitpid(writer, 0)
            writer = None

    send(dict(op="hello", pid=os.getpid(), size=size(), unicode_geometry_checked=True))
    for line in stream:
        request = json.loads(line)
        op = request["op"]
        try:
            if op == "output":
                data = payload(
                    request["kind"], request["bytes"], size()["columns"], size()["rows"]
                )
                barrier()
                start = time.monotonic_ns()
                write(data)
                barrier()
                send(
                    dict(op=op, elapsed_ns=time.monotonic_ns() - start, bytes=len(data))
                )
            elif op == "prefill":
                geometry = size()
                write(b"\x1b[0m\x1b[2J\x1b[3J\x1b[H")
                block = b"h" * (geometry["columns"] - 1) + b"\r\n"
                for _ in range(request["lines"]):
                    write(block)
                barrier()
                send(dict(op=op, size=size()))
            elif op == "load_start":
                if writer is not None:
                    raise RuntimeError("output producer already running")
                geometry = size()
                writer = os.fork()
                if writer == 0:
                    try:
                        stream.close()
                        connection.close()
                        signal.alarm(max(5, int(request["timeout"]) * 3))
                        block = (b"f" * (geometry["columns"] - 1) + b"\r\n") * 32
                        while True:
                            write(block)
                    except BaseException:
                        os._exit(1)
                send(dict(op=op))
            elif op == "load_stop":
                stop_writer()
                barrier()
                send(dict(op=op))
            elif op == "input":
                if request.get("under_load"):
                    if writer is None:
                        raise RuntimeError("output producer is not running")
                    if os.waitpid(writer, os.WNOHANG)[0]:
                        writer = None
                        raise RuntimeError("output producer exited before the sample")
                else:
                    barrier()
                send(dict(op="armed"))
                data = read_input()
                received = time.monotonic_ns()
                if data != b"a":
                    raise RuntimeError(
                        f"expected raw a, received {data.hex()} (check keyboard layout/locks)"
                    )
                if not request.get("under_load"):
                    geometry = size()
                    witness(request["rgb"], geometry["columns"], geometry["rows"])
                send(dict(op=op, received_ns=received))
            elif op == "size":
                deadline = time.monotonic() + request.get("timeout", 10)
                while time.monotonic() < deadline:
                    geometry = size()
                    if [geometry["columns"], geometry["rows"]] == request["expected"]:
                        break
                    time.sleep(0.001)
                else:
                    raise TimeoutError(f"PTY size {geometry} != {request['expected']}")
                send(dict(op=op, observed_ns=time.monotonic_ns(), size=geometry))
            elif op == "quit":
                stop_writer()
                send(dict(op=op))
                return
            else:
                raise ValueError(f"unknown control command: {op}")
        except Exception as error:
            stop_writer()
            send(dict(op="error", error=str(error)))
            return
    stop_writer()


if __name__ == "__main__":
    serve(sys.argv[1], sys.argv[2])
