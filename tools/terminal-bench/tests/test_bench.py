"""Harness invariants: failures, provenance, privacy and timing semantics."""

import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import adapters
import child
import common
import report
import resources
import runner


class Temporary(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="aura-bench-unit-")
        self.root = Path(self.temporary.name)
        self.addCleanup(self.temporary.cleanup)

    def config(self, suffix=""):
        (self.root / "font.ttf").write_bytes(b"synthetic font fixture")
        path = self.root / "benchmark.toml"
        path.write_text(
            'font_file="font.ttf"\n[[terminal]]\nid="aura"\nkind="aura"\nbinary="./build/aura"\n'
            + suffix
        )
        return path


class Statistics(unittest.TestCase):
    def test_empty_single_even_and_nearest_rank(self):
        self.assertIsNone(common.summarize([])["p50"])
        self.assertEqual(common.summarize([7])["p99"], 7)
        self.assertEqual(common.summarize([4, 1, 3, 2])["p50"], 2)
        result = common.summarize(list(range(1, 301)))
        self.assertEqual([result["p50"], result["p95"], result["p99"]], [150, 285, 297])
        self.assertEqual(result["n"], 300)

    def test_invalid_samples_are_not_silently_charted(self):
        for value in [-1, float("inf"), float("nan")]:
            with self.assertRaises(ValueError):
                common.summarize([value])
        self.assertEqual(
            common.summarize([0])["n"], 1, "legitimate measured zeros remain samples"
        )


class Configuration(Temporary):
    def test_paths_are_relative_to_config_and_strict(self):
        config = common.load_config(self.config())
        self.assertEqual(config["font_file"], str(self.root / "font.ttf"))
        self.assertEqual(config["terminal"][0]["binary"], str(self.root / "build/aura"))
        self.assertEqual(config["history"], 10000)
        path = self.config("misspelled=true\n")
        with self.assertRaises(ValueError):
            common.load_config(path)

    def test_duplicate_ids_invalid_bounds_and_unsupported_spacing(self):
        for suffix in [
            '[[terminal]]\nid="aura"\nkind="xterm"\n',
            "cell_spacing=[-1,0]\n",
            "cell_spacing=[true,0]\n",
        ]:
            with self.assertRaises(ValueError):
                common.load_config(self.config(suffix))
        path = self.config()
        path.write_text(
            path.read_text().replace('kind="aura"', 'kind="xterm"')
            + "cell_spacing=[1,0]\n"
        )
        with self.assertRaises(ValueError):
            common.load_config(path)
        for text in [
            "columns=true",
            "font_pixels=nan",
            "rows=3",
            "dpi=0",
            "history=-1",
            "typo=1",
        ]:
            path = self.config()
            path.write_text(text + "\n" + path.read_text())
            with self.assertRaises(ValueError):
                common.load_config(path)

    def test_output_cannot_escape_through_symlinks_into_any_checkout(self):
        repository = self.root / "repository"
        (repository / ".git").mkdir(parents=True)
        (repository / ".git/HEAD").write_text("ref: refs/heads/main\n")
        link = self.root / "link"
        link.symlink_to(repository, target_is_directory=True)
        for path in [
            repository / "results",
            link / "results",
            common.ROOT / "ignored-target/results",
        ]:
            with self.assertRaises(ValueError):
                common.outside_checkout(path)
        self.assertEqual(
            common.outside_checkout(self.root / "private"), self.root / "private"
        )

    def test_worktree_marker_is_also_protected(self):
        (self.root / ".git").write_text("gitdir: /elsewhere/worktrees/test\n")
        with self.assertRaises(ValueError):
            common.outside_checkout(self.root / "results")

    def test_inert_empty_platform_marker_is_not_a_checkout(self):
        (self.root / ".git").mkdir()
        self.assertEqual(
            common.outside_checkout(self.root / "private"), self.root / "private"
        )


class Protocol(Temporary):
    def test_payloads_are_deterministic_complete_utf8_and_cover_budget(self):
        for kind in ["ascii", "unicode", "sgr", "tui"]:
            data = child.payload(kind, 4096, 80, 24)
            data.decode("utf-8")
            self.assertGreaterEqual(len(data), 4096)
            self.assertEqual(data, child.payload(kind, 4096, 80, 24))
        self.assertIn("界e\u0301", child.payload("unicode", 4096, 80, 24).decode())

    def channel(self):
        left, right = socket.socketpair()
        self.addCleanup(left.close)
        self.addCleanup(right.close)
        return runner.Channel(left, Mock(), 0.03), right

    def test_fragmented_multiple_packets_and_child_errors(self):
        channel, peer = self.channel()
        channel.buffer = b'{"op":'
        peer.sendall(b'"hello"}\n{"op":"error","error":"bad DSR"}\n')
        self.assertEqual(channel.receive(), {"op": "hello"})
        with self.assertRaisesRegex(RuntimeError, "bad DSR"):
            channel.receive()

    def test_timeout_eof_and_oversized_replies_fail(self):
        channel, peer = self.channel()
        with self.assertRaises(TimeoutError):
            channel.receive()
        peer.close()
        with self.assertRaises(EOFError):
            channel.receive()
        channel, peer = self.channel()
        peer.sendall(b"x" * 65537)
        with self.assertRaises(ValueError):
            channel.receive()

    def test_request_rejects_wrong_reply(self):
        channel, peer = self.channel()
        peer.sendall(b'{"op":"wrong"}\n')
        with self.assertRaises(ValueError):
            channel.request("size", expected=[80, 24])

    def test_stalled_emulator_is_stopped_and_retains_failure(self):
        binary = self.root / "stalled"
        binary.write_text(f"#!{sys.executable}\nimport time\ntime.sleep(60)\n")
        binary.chmod(0o755)
        config = common.load_config(self.config())
        original = runner.subprocess.Popen
        processes = []

        def launch(*args, **kwargs):
            process = original(*args, **kwargs)
            processes.append(process)
            return process

        with patch.object(runner.subprocess, "Popen", side_effect=launch):
            result = runner.trial(
                dict(id="stalled", kind="xterm"),
                dict(binary=str(binary)),
                self.root / "trial",
                config,
                dict(timeout=0.15),
                ":65534",
                Mock(),
            )
        self.assertEqual(result["status"], "failed")
        self.assertIn("startup timed out", result["error"])
        self.assertEqual(result["metrics"], {})
        self.assertEqual(len(processes), 1)
        self.assertIsNotNone(processes[0].poll())
        self.assertTrue((self.root / "trial/terminal.log").exists())

    def test_cursor_geometry_query_does_not_accept_wrong_columns(self):
        with (
            patch.object(child, "write"),
            patch.object(child, "read_input", return_value=b"\x1b[1;7R"),
        ):
            with self.assertRaisesRegex(RuntimeError, "cursor reply"):
                child.query_position(1, 6)

    def test_duplicate_baseline_hashes_are_rejected(self):
        entries = [dict(id="before", kind="aura"), dict(id="after", kind="aura")]
        with patch.object(
            adapters, "identify", return_value=dict(status="available", sha256="same")
        ):
            with self.assertRaisesRegex(ValueError, "same binary hash"):
                adapters.identify_all(entries)

    def test_binary_provenance_drift_is_rejected(self):
        binary = self.root / "binary"
        binary.write_bytes(b"baseline")
        expected = common.sha256(binary)
        common.require_hash(binary, expected)
        binary.write_bytes(b"different binary")
        with self.assertRaisesRegex(RuntimeError, "provenance changed"):
            common.require_hash(binary, expected)

    def test_cleanup_does_not_signal_a_reaped_process_group(self):
        process = Mock(pid=123, poll=Mock(return_value=0))
        with patch.object(runner.os, "killpg") as signal_group:
            runner.stop(process)
        signal_group.assert_not_called()

    def test_existing_run_is_not_overwritten(self):
        output = self.root / "existing"
        output.mkdir()
        evidence = output / "run.json"
        evidence.write_text("previous evidence")
        with self.assertRaises(FileExistsError):
            runner.run({}, {}, output)
        self.assertEqual(evidence.read_text(), "previous evidence")


class Sampling(unittest.TestCase):
    def test_pid_reuse_and_helper_exclusion(self):
        sampler = resources.Resources(1)
        sampler.exclude = {2}
        states = {
            1: dict(pid=1, start=10, ticks=20, rss=1024),
            2: dict(pid=2, start=11, ticks=100, rss=4096),
            3: dict(pid=3, start=12, ticks=30, rss=2048),
        }
        with (
            patch.object(
                resources,
                "children",
                side_effect=lambda pid: {2, 3} if pid == 1 else set(),
            ),
            patch.object(resources, "process_stat", side_effect=states.get),
        ):
            sampler.sample(True)
            self.assertEqual(sampler.peak, 3072)
            self.assertNotIn((2, 11), sampler.ticks)
            states[3] = dict(pid=3, start=99, ticks=2, rss=512)
            sampler.sample(True)
            self.assertEqual(sampler.ticks[(3, 12)], 30)
            self.assertEqual(sampler.ticks[(3, 99)], 2)

    def test_linux_stat_parser_includes_all_threads_not_children_cpu(self):
        stat = resources.process_stat(os.getpid())
        self.assertEqual(stat["pid"], os.getpid())
        self.assertGreater(stat["rss"], 0)
        self.assertGreater(stat["start"], 0)


class Reports(Temporary):
    def manifest(self):
        measured = dict(
            terminal="aura",
            status="ok",
            round=1,
            warmup=False,
            cell_pixels=[9, 17],
            metrics={"startup_ready_ms": [2], "key_to_child_ms": [1, 2, 3]},
        )
        reference = dict(
            measured,
            terminal="reference",
            metrics={"startup_ready_ms": [4], "key_to_child_ms": [2, 4, 6]},
        )
        warmup = dict(
            measured, round=0, warmup=True, metrics={"startup_ready_ms": [999]}
        )
        return dict(
            schema=1,
            status="complete",
            created_utc="test",
            geometry_policy="cell",
            settings=dict(repetitions=1),
            config=dict(
                terminal=[
                    dict(id="aura", kind="aura"),
                    dict(id="reference", kind="alacritty"),
                    dict(id="missing", kind="kitty"),
                ]
            ),
            terminal_identities={
                "aura": dict(status="available", version="<script>alert(1)</script>"),
                "reference": dict(status="available"),
                "missing": dict(status="unavailable", error="not installed"),
            },
            environment=dict(xvfb=True),
            trials=[warmup, measured, reference],
        )

    def test_warmups_missing_failures_and_relative_baselines(self):
        manifest = self.manifest()
        rows = report.aggregate(manifest, "reference")
        row = next(
            row
            for row in rows
            if row["terminal"] == "aura" and row["metric"] == "startup_ready_ms"
        )
        self.assertEqual(row["n"], 1)
        self.assertEqual(row["p50"], 2)
        self.assertEqual(row["delta_percent"], -50)
        missing = next(row for row in rows if row["terminal"] == "missing")
        self.assertIsNone(missing["p50"])
        self.assertEqual(missing["n"], 0)
        manifest["trials"].append(
            dict(
                manifest["trials"][1],
                status="failed",
                metrics={"startup_ready_ms": [0]},
            )
        )
        row = next(
            row
            for row in report.aggregate(manifest, "reference")
            if row["terminal"] == "aura" and row["metric"] == "startup_ready_ms"
        )
        self.assertEqual(row["p50"], 2)
        self.assertEqual(row["status"], "partial")
        self.assertIsNone(row["delta_percent"])

    def test_different_pixel_geometry_is_not_ranked_together(self):
        manifest = self.manifest()
        manifest["trials"][2]["cell_pixels"] = [8, 17]
        rows = report.aggregate(manifest, "reference")
        row = next(
            row
            for row in rows
            if row["terminal"] == "aura"
            and row["metric"] == "startup_ready_ms"
            and row["n"]
        )
        self.assertIsNone(row["delta_percent"])
        manifest["geometry_policy"] = "grid"
        row = next(
            row
            for row in report.aggregate(manifest, "reference")
            if row["terminal"] == "aura" and row["metric"] == "startup_ready_ms"
        )
        self.assertEqual(row["delta_percent"], -50)

    def test_report_is_self_contained_escaped_and_never_overwrites_evidence(self):
        source = self.root / "run.json"
        source.write_text(json.dumps(self.manifest()))
        output = self.root / "report"
        report.render(source, output, "reference")
        document = (output / "index.html").read_text()
        self.assertIn("&lt;script&gt;alert(1)&lt;/script&gt;", document)
        self.assertNotIn("<script>alert(1)</script>", document)
        self.assertNotIn("https://", document)
        self.assertIn("input-to-photon", document)
        self.assertIn("Xvfb / software", document)
        self.assertTrue((output / "summary.csv").is_file())
        self.assertGreater(len(list(output.glob("*.svg"))), 0)
        with self.assertRaises(FileExistsError):
            report.render(source, output)

    def test_actual_adapter_configs_keep_arguments_without_shell_interpolation(self):
        config = common.load_config(self.config())
        command = [
            sys.executable,
            "/tmp/helper with spaces.py",
            "literal $(no-shell)",
            "/tmp/terminal-🧪/helper.py",
        ]
        for kind in common.KINDS:
            directory = self.root / kind
            directory.mkdir()
            args = adapters.launch(
                kind, "/fake/terminal", directory, config, command, "synthetic-title"
            )
            self.assertEqual(args[0], "/fake/terminal")
            if kind in ("aura", "alacritty"):
                path = directory / (
                    "config.toml" if kind == "aura" else "alacritty.toml"
                )
                import tomllib

                value = tomllib.loads(path.read_text())
                shell = value["shell"] if kind == "aura" else value["terminal"]["shell"]
                self.assertEqual(shell["args"], command[1:])
            else:
                self.assertEqual(args[-len(command) :], command)


if __name__ == "__main__":
    unittest.main()
