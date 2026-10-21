"""Sample observed Linux process trees, excluding the synthetic PTY helper."""

import os
from pathlib import Path
import time


def process_stat(pid):
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
        fields = text[text.rfind(")") + 2 :].split()
        return dict(
            pid=pid,
            start=int(fields[19]),
            ticks=int(fields[11]) + int(fields[12]),
            rss=max(0, int(fields[21])) * os.sysconf("SC_PAGE_SIZE"),
        )
    except (OSError, ValueError, IndexError):
        return None


def children(pid):
    # Children can belong to any thread in a GUI process, not just its leader.
    found = set()
    try:
        tasks = list(Path(f"/proc/{pid}/task").iterdir())
    except OSError:
        return found
    for task in tasks:
        try:
            found.update(int(v) for v in (task / "children").read_text().split())
        except OSError:
            continue
    return found


class Resources:
    def __init__(self, root):
        self.root = root
        self.exclude = set()
        self.ticks = {}
        self.phase_peak = 0
        self.peak = 0
        self.samples = 0
        self.current_rss = 0
        self.last_sample_ns = 0

    def sample(self, force=False):
        now = time.monotonic_ns()
        if not force and now - self.last_sample_ns < 10_000_000:
            return
        self.last_sample_ns = now
        pending, seen, rss = [self.root], set(), 0
        while pending:
            pid = pending.pop()
            if pid in seen or pid in self.exclude:
                continue
            seen.add(pid)
            stat = process_stat(pid)
            if stat:
                self.ticks[(pid, stat["start"])] = max(
                    self.ticks.get((pid, stat["start"]), 0), stat["ticks"]
                )
                rss += stat["rss"]
                pending.extend(children(pid) - seen)
        self.current_rss = rss
        self.samples += 1
        self.phase_peak = max(self.phase_peak, rss)
        self.peak = max(self.peak, rss)

    def cpu(self):
        return sum(self.ticks.values()) / os.sysconf("SC_CLK_TCK")

    def begin(self):
        self.sample(True)
        self.phase_peak = self.current_rss
        return time.monotonic_ns(), self.cpu()

    def end(self, baseline):
        self.sample(True)
        start, cpu = baseline
        elapsed = (time.monotonic_ns() - start) / 1e9
        return dict(
            wall_seconds=elapsed,
            cpu_seconds=max(0, self.cpu() - cpu),
            sampled_peak_rss_mib=self.phase_peak / 1048576,
        )
