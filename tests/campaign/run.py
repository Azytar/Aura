#!/usr/bin/env python3
"""Run Aura's bounded campaign profiles and retain commands, logs and outcomes."""
import argparse
import hashlib
import json
import os
import signal
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
PROFILES = {"quick": (64, 32, 12, 6, 32, 1024), "extended": (2048, 96, 24, 12, 128, 4096), "exhaustive": (4096, 128, 32, 16, 256, 8192)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", choices=PROFILES)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--seed", type=lambda x: int(x, 0), default=0x41555241)
    parser.add_argument("--rng", choices=["chacha", "xorshift"], default="chacha")
    for field in ["cases", "actions", "width", "height", "history", "shrink"]:
        parser.add_argument("--" + field, type=int)
    parser.add_argument("--gpu", choices=["off", "local", "required"], default="local")
    parser.add_argument("--triples", action="store_true")
    parser.add_argument("--safety-seconds", type=int, default=600)
    args = parser.parse_args()
    if not 0 <= args.seed < 2**64 or args.safety_seconds < 1:
        parser.error("invalid seed or safety deadline")
    effective = dict(zip(["cases", "actions", "width", "height", "history", "shrink"], PROFILES[args.profile]))
    for field in effective:
        if getattr(args, field) is not None:
            effective[field] = getattr(args, field)
    limits = {"cases": (1, 65536), "actions": (1, 512), "width": (1, 128), "height": (1, 64), "history": (0, 4096), "shrink": (0, 65536)}
    if any(not limits[key][0] <= value <= limits[key][1] for key, value in effective.items()):
        parser.error(f"resource bounds: {limits}")
    output = (args.output or ROOT / "target/campaign" / args.profile).resolve()
    output.mkdir(parents=True, exist_ok=True)
    child = dict(os.environ, AURA_PROFILE=args.profile, AURA_SEED=str(args.seed), AURA_RNG=args.rng, AURA_ARTIFACTS=str(output), AURA_REQUIRE_GPU="1" if args.gpu == "required" else "0", AURA_BRAILLE_TRIPLES="1" if args.triples else "0", PYTHONDONTWRITEBYTECODE="1")
    child.update({"AURA_" + key.upper(): str(value) for key, value in effective.items()})
    child.pop("AURA_REPLAY", None)
    child.pop("AURA_TEXT_REPLAY", None)
    report = dict(profile=args.profile, effective=effective, seed=args.seed, rng=args.rng, seed_encoding="4 little-endian u64 words: seed+i, wrapping; ChaCha32 bytes/XorShift16 bytes", gpu=args.gpu, triples=args.triples, stages=[])
    report["base_commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    report["rustc"] = subprocess.check_output(["rustc", "-Vv"], text=True)
    report["lock_sha256"] = hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()
    font = Path("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf")
    report["font"] = dict(path=str(font), sha256=hashlib.sha256(font.read_bytes()).hexdigest() if font.exists() else None)
    report["initial_status"] = subprocess.check_output(["git", "status", "--short"], cwd=ROOT, text=True)
    report["source_sha256"] = {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for base, pattern in [(ROOT / "crates", "*.rs"), (ROOT / "shaders", "*.glsl"), (ROOT / "tests", "*.py"), (ROOT / "tests/campaign", "*.case")] for p in base.rglob(pattern) if "target" not in p.parts}

    def save():
        (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")

    def stage(name, command, gpu=False):
        before = time.monotonic()
        path = output / (name + ".log")
        status = "FAILED"
        with open(path, "wb") as log:
            try:
                process = subprocess.Popen(command, cwd=ROOT, env=child, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    code = process.wait(timeout=args.safety_seconds)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                    raise
                text = path.read_text(errors="replace")
                if code == 77 or (code == 0 and "skip" in text.lower()) or (name == "vulkan" and code != 0):
                    status = "FAILED_PREREQUISITE" if args.gpu == "required" else "SKIPPED"
                elif code == 0:
                    status = "COMPLETE"
            except FileNotFoundError as error:
                log.write(str(error).encode())
                code = 77
                status = "FAILED_PREREQUISITE" if args.gpu == "required" or not gpu else "SKIPPED"
            except subprocess.TimeoutExpired:
                code = 124
                status = "INCOMPLETE"
        report["stages"].append(dict(name=name, command=command, cwd=str(ROOT), exit=code, status=status, seconds=time.monotonic()-before))
        print(f"{name}: {status} exit={code}", flush=True)
        save()
        return status in ["COMPLETE", "SKIPPED"]

    save()
    stages = [("core", ["cargo", "test", "-p", "aura-core", "terminal::campaign", "--locked", "--", "--nocapture", "--test-threads=1"], False), ("text", ["cargo", "test", "-p", "aura-text", "campaign::", "--locked", "--", "--nocapture", "--test-threads=1"], False)]
    if args.gpu != "off":
        stages += [("vulkan", ["vulkaninfo", "--summary"], True), ("gpu", ["cargo", "test", "-p", "aura-render", "--test", "campaign_gpu", "--test", "cell_graphics_gpu", "--test", "offscreen_gpu", "--locked", "--", "--nocapture", "--test-threads=1"], True), ("build", ["cargo", "build", "--release", "--locked"], False), ("native", ["python3", "tests/x11_campaign.py", "target/release/aura"], True)]
    for name, command, gpu in stages:
        if not stage(name, command, gpu):
            report["outcome"] = "INCOMPLETE" if report["stages"][-1]["status"] == "INCOMPLETE" else "FAILED"
            save()
            return 1
    report["outcome"] = "COMPLETE_WITH_OMISSIONS" if any(s["status"] == "SKIPPED" for s in report["stages"]) else "COMPLETE"
    save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
