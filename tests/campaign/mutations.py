#!/usr/bin/env python3
"""Three bounded, temporary mutations; always restore the exact source bytes."""
import hashlib
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target/campaign/mutations").resolve()
OUTPUT.mkdir(parents=True, exist_ok=True)
ENV = dict(os.environ, AURA_PROFILE="quick", AURA_REQUIRE_GPU="1", PYTHONDONTWRITEBYTECODE="1")
RESULTS = []


def command(label, args):
    start = time.monotonic()
    with open(OUTPUT / (label + ".log"), "wb") as log:
        try:
            process = subprocess.Popen(args, cwd=ROOT, env=ENV, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                status = process.wait(timeout=240)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"INCOMPLETE: {label} exceeded safety deadline") from None
    return status, time.monotonic() - start


mutations = [
    ("braille", "shaders/cell_graphics.glsl", "uint[](0u, 3u, 1u", "uint[](3u, 0u, 1u", ["cargo", "test", "-p", "aura-render", "--test", "campaign_gpu", "unicode_named_braille", "--locked", "--", "--nocapture"], "pattern="),
    ("wide-repair", "crates/aura-core/src/grid.rs", "            cells[column] = Cell::EMPTY;\n            // Invalid clipped", "            let _retained_invalid_cell = cells[column];\n            // Invalid clipped", ["cargo", "test", "-p", "aura-core", "terminal::campaign::replay_cases", "--locked", "--", "--nocapture"], "orphan leading"),
    ("dirty-update", "crates/aura-render/src/lib.rs", "    } else {\n        dirty\n    }\n}", "    } else {\n        Vec::new()\n    }\n}", ["python3", "tests/x11_campaign.py", "target/release/aura"], "native reference missing required blue background"),
]
try:
    for name, relative, old, new, test, expected in mutations:
        path = ROOT / relative
        original = path.read_bytes()
        text = original.decode()
        assert text.count(old) == 1, f"mutation anchor not unique: {name}"
        ENV["AURA_ARTIFACTS"] = str(OUTPUT / name)
        if name == "wide-repair":
            ENV["AURA_REPLAY"] = str(ROOT / "tests/campaign/cases/wide-delete.case")
        else:
            ENV.pop("AURA_REPLAY", None)
        before = hashlib.sha256(original).hexdigest()
        try:
            path.write_text(text.replace(old, new, 1))
            if name == "dirty-update":
                assert command(name + "-mutant-build", ["cargo", "build", "--release", "--locked"])[0] == 0
            status, duration = command(name + "-mutant", test)
            output = (OUTPUT / (name + "-mutant.log")).read_text(errors="replace")
            assert status != 0 and expected in output and "error: could not compile" not in output, f"mutation was not detected by its assertion: {name}"
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
        if name == "dirty-update":
            assert command(name + "-restored-build", ["cargo", "build", "--release", "--locked"])[0] == 0
        restored, restored_time = command(name + "-restored", test)
        assert restored == 0, f"restored source does not pass: {name}"
        RESULTS.append(dict(name=name, detected=True, restored=True, mutant_exit=status, mutant_seconds=duration, restored_seconds=restored_time, source_sha256=before))
        print(f"PASS mutation={name} detected={status} restored=0", flush=True)
finally:
    (OUTPUT / "results.json").write_text(json.dumps(RESULTS, indent=2) + "\n")
