"""Shared schema, bounded statistics, and artifact privacy rules."""

import hashlib
import math
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PROFILES = {
    "quick": dict(
        repetitions=3,
        warmups=1,
        payload_bytes=256 * 1024,
        input_samples=16,
        resize_pairs=4,
        idle_seconds=0.3,
    ),
    "standard": dict(
        repetitions=7,
        warmups=2,
        payload_bytes=8 * 1024 * 1024,
        input_samples=100,
        resize_pairs=12,
        idle_seconds=2.0,
    ),
}
KINDS = ("aura", "alacritty", "kitty", "wezterm", "xterm")


def outside_checkout(path):
    path = Path(path).expanduser().resolve()
    if (
        path == ROOT
        or ROOT in path.parents
        or any(
            (parent / ".git").is_file() or (parent / ".git" / "HEAD").is_file()
            for parent in (path, *path.parents)
        )
    ):
        raise ValueError("artifacts must be outside every Git checkout")
    return path


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require_hash(path, expected):
    if sha256(path) != expected:
        raise RuntimeError(f"provenance changed during campaign: {path}")


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


def summarize(values):
    if any(not math.isfinite(v) or v < 0 for v in values):
        raise ValueError("samples must be finite and nonnegative")
    if not values:
        return dict(n=0, p50=None, p95=None, p99=None, min=None, max=None)
    return dict(
        n=len(values),
        p50=percentile(values, 0.5),
        p95=percentile(values, 0.95),
        p99=percentile(values, 0.99),
        min=min(values),
        max=max(values),
    )


def load_config(path):
    with Path(path).open("rb") as source:
        config = tomllib.load(source)
    allowed = {
        "font_family",
        "font_file",
        "font_pixels",
        "dpi",
        "columns",
        "rows",
        "history",
        "terminal",
    }
    if set(config) - allowed:
        raise ValueError(f"unknown config keys: {sorted(set(config) - allowed)}")
    defaults = dict(
        font_family="DejaVu Sans Mono",
        font_pixels=14.0,
        dpi=96.0,
        columns=80,
        rows=24,
        history=10000,
    )
    config = defaults | config
    for key, low, high in [
        ("columns", 8, 512),
        ("rows", 4, 256),
        ("history", 0, 1000000),
    ]:
        if type(config[key]) is not int or not low <= config[key] <= high:
            raise ValueError(f"{key} must be an integer in {low}..{high}")
    for key, low, high in [("font_pixels", 6, 72), ("dpi", 50, 400)]:
        if (
            type(config[key]) not in (int, float)
            or not math.isfinite(config[key])
            or not low <= config[key] <= high
        ):
            raise ValueError(f"{key} must be in {low}..{high}")
    family = config["font_family"]
    if (
        not isinstance(family, str)
        or not family
        or any(ord(ch) < 32 or ord(ch) == 127 for ch in family)
    ):
        raise ValueError("font_family must be a nonempty single-line string")
    if not isinstance(config.get("font_file"), str):
        raise ValueError("font_file is required: identify and hash the intended face")
    font = Path(config["font_file"]).expanduser()
    if not font.is_absolute():
        font = Path(path).resolve().parent / font
    config["font_file"] = str(font.resolve(strict=True))
    entries = config.get("terminal")
    if not isinstance(entries, list) or not entries:
        raise ValueError("at least one [[terminal]] is required")
    names = set()
    for entry in entries:
        if set(entry) - {
            "id",
            "kind",
            "binary",
            "required",
            "cell_spacing",
            "source_ref",
        }:
            raise ValueError("unknown terminal key")
        name = entry.get("id")
        if (
            not isinstance(name, str)
            or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}", name)
            or name in names
        ):
            raise ValueError(
                "terminal IDs must be unique safe filenames (1..64 characters)"
            )
        names.add(name)
        if entry.get("kind") not in KINDS:
            raise ValueError(f"unsupported terminal kind: {entry.get('kind')}")
        entry.setdefault("binary", entry["kind"])
        entry.setdefault("required", True)
        if "source_ref" in entry and (
            not isinstance(entry["source_ref"], str) or len(entry["source_ref"]) > 256
        ):
            raise ValueError("source_ref must be a string of at most 256 characters")
        spacing = entry.setdefault("cell_spacing", [0, 0])
        if (
            not isinstance(spacing, list)
            or len(spacing) != 2
            or any(type(v) is not int or not -16 <= v <= 16 for v in spacing)
        ):
            raise ValueError("cell_spacing must contain two integers in -16..16")
        if entry["kind"] == "aura" and any(v < 0 for v in spacing):
            raise ValueError("Aura cell spacing must be nonnegative")
        if any(spacing) and entry["kind"] not in ("aura", "alacritty", "kitty"):
            raise ValueError(
                "nonzero cell_spacing is supported only for Aura, Alacritty and Kitty"
            )
        if (
            not isinstance(entry["binary"], str)
            or not entry["binary"]
            or "\x00" in entry["binary"]
        ):
            raise ValueError("binary must be a nonempty executable path or name")
        if "/" in entry["binary"] or entry["binary"].startswith("~"):
            binary = Path(entry["binary"]).expanduser()
            entry["binary"] = str(
                (
                    binary
                    if binary.is_absolute()
                    else Path(path).resolve().parent / binary
                ).resolve()
            )
        if type(entry["required"]) is not bool:
            raise ValueError("required must be a boolean")
    return config
