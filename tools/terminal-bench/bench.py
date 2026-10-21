#!/usr/bin/env python3
"""Reproducible Linux/X11 multi-terminal benchmarks; artifacts stay outside Git."""

import argparse
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True


def main():
    from common import PROFILES, load_config
    import adapters
    import report
    import runner

    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    listing = actions.add_parser("list", help="probe configured executable versions")
    listing.add_argument("--config", type=Path, required=True)
    execution = actions.add_parser(
        "run", help="run on an owned Xvfb or a dedicated X11 display"
    )
    execution.add_argument("--config", type=Path, required=True)
    execution.add_argument(
        "--output",
        type=Path,
        required=True,
        help="new directory outside every checkout",
    )
    execution.add_argument("--profile", choices=PROFILES, default="standard")
    screen = execution.add_mutually_exclusive_group(required=True)
    screen.add_argument(
        "--xvfb",
        action="store_true",
        help="own a fresh virtual display (functional smoke, not hardware ranking)",
    )
    screen.add_argument(
        "--display",
        help="existing dedicated X11 server; the runner changes input focus",
    )
    execution.add_argument(
        "--environment",
        choices=["software", "hardware", "unknown"],
        default="unknown",
        help="declaration retained with renderer inventory; not hardware certification",
    )
    execution.add_argument(
        "--geometry-policy",
        choices=["cell", "grid"],
        default="cell",
        help="compare equal pixel cells (default) or equal character grids",
    )
    execution.add_argument("--seed", type=int, default=0x41555241)
    execution.add_argument("--timeout", type=float, default=30)
    execution.add_argument("--repetitions", type=int)
    execution.add_argument("--warmups", type=int)
    execution.add_argument(
        "--baseline", help="terminal ID for per-metric relative deltas"
    )
    rendering = actions.add_parser(
        "report", help="regenerate a report in a new private directory"
    )
    rendering.add_argument("source", type=Path, help="run.json outside Git")
    rendering.add_argument("--output", type=Path, required=True)
    rendering.add_argument("--baseline")
    args = parser.parse_args()
    try:
        if args.action == "report":
            report.render(args.source, args.output, args.baseline)
            print(f"Report: {args.output / 'index.html'}")
            return 0
        config = load_config(args.config)
        if args.action == "list":
            for entry in config["terminal"]:
                print(
                    entry["id"],
                    json.dumps(adapters.identify(entry), ensure_ascii=False),
                )
            return 0
        settings = PROFILES[args.profile] | dict(
            seed=args.seed, timeout=args.timeout, profile=args.profile
        )
        for field in ["repetitions", "warmups"]:
            if getattr(args, field) is not None:
                settings[field] = getattr(args, field)
        if (
            not 1 <= settings["repetitions"] <= 100
            or not 0 <= settings["warmups"] <= 10
            or not 1 <= args.timeout <= 120
        ):
            raise ValueError(
                "repetitions: 1..100, warmups: 0..10, timeout: 1..120 seconds"
            )
        if args.baseline and args.baseline not in {
            entry["id"] for entry in config["terminal"]
        }:
            raise ValueError("baseline is not a configured terminal ID")
        manifest = runner.run(
            config,
            settings,
            args.output,
            args.display,
            args.xvfb,
            args.environment,
            args.geometry_policy,
        )
        report.render(args.output / "run.json", args.output / "report", args.baseline)
        print(f"Artifacts: {args.output}; status={manifest['status']}")
        return 0 if manifest["status"] == "complete" else 1
    except (OSError, ValueError, RuntimeError, TimeoutError) as error:
        print(f"benchmark: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print(
            "benchmark interrupted; completed trials remain in run.json",
            file=sys.stderr,
        )
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
