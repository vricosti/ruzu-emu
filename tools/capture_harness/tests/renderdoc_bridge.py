#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the real C preload/IPC bridge without RenderDoc, a GPU, or input injection.

Usage: RENDERDOC_INCLUDE=/path/to/sdk/include python3 tools/capture_harness/tests/renderdoc_bridge.py
"""
import os
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest


class BridgeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        include = os.environ.get("RENDERDOC_INCLUDE")
        if not include:
            raise unittest.SkipTest("set RENDERDOC_INCLUDE to the SDK header directory")
        cls.temp = tempfile.TemporaryDirectory(prefix="capture-bridge-test-")
        cls.addClassCleanup(cls.temp.cleanup)
        cls.directory = Path(cls.temp.name)
        root = Path(__file__).resolve().parents[1]
        cls.helper = cls.directory / "helper.so"
        cls.fake = cls.directory / "fake.so"
        cls.target = cls.directory / "synthetic-target"
        for source, output in [(root / "rdc_trigger.c", cls.helper),
                               (root / "tests/fake_renderdoc.c", cls.fake)]:
            subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC",
                            "-I", include, str(source), "-o", str(output), "-ldl", "-pthread"], check=True)
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-DSYNTHETIC_TARGET",
                        "-I", include, str(root / "tests/fake_renderdoc.c"), "-o", str(cls.target)], check=True)

    def run_bridge(self, command, mode="", load_api=True):
        parent, child = socket.socketpair()
        with parent, child:
            parent.settimeout(3)
            env = dict(os.environ, RUZU_CAPTURE_CONTROL_FD=str(child.fileno()),
                       RUZU_CAPTURE_PREFIX=str(self.directory / "frame"), FAKE_CAPTURE_MODE=mode,
                       LD_PRELOAD=f"{self.fake}:{self.helper}" if load_api else str(self.helper))
            process = subprocess.Popen(["/bin/sleep", "10"], env=env, pass_fds=(child.fileno(),))
            child.close()
            try:
                with parent.makefile("rb") as stream:
                    ready = stream.readline().decode().strip()
                    if ready != "READY":
                        return ready
                    parent.sendall(command.encode() + b"\n")
                    return stream.readline().decode().strip()
            finally:
                process.terminate()
                process.wait(timeout=3)

    def test_completed_capture_returns_filename(self):
        reply = self.run_bridge("CAPTURE 2 100")
        self.assertTrue(reply.startswith("DONE "), reply)
        self.assertTrue((self.directory / "frame_synthetic.rdc").is_file())
        self.assertEqual(reply.count("frame_synthetic.rdc"), 2)

    def test_timeout_is_not_reported_as_capture(self):
        self.assertIn("ERROR capture timeout", self.run_bridge("CAPTURE 1 20", "timeout"))

    def test_refuses_overlapping_and_invalid_captures(self):
        self.assertIn("ERROR another capture", self.run_bridge("CAPTURE 1 20", "busy"))
        self.assertIn("ERROR invalid command", self.run_bridge("CAPTURE 0 20"))

    def test_missing_api_is_reported(self):
        self.assertIn("ERROR RenderDoc API", self.run_bridge("CAPTURE 1 20", load_api=False))

    def test_harness_launch_timeline_and_completed_manifest(self):
        binary = os.environ.get("CAPTURE_HARNESS_BINARY")
        if not binary:
            self.skipTest("set CAPTURE_HARNESS_BINARY for the full launch test")
        output = self.directory / "harness-run"
        config = self.directory / "run.toml"
        config.write_text(f'''[process]
executable = {json.dumps(str(self.target))}
rom_path = "/dev/null"
log_file = {json.dumps(str(output / 'process.log'))}
[capture]
output_directory = {json.dumps(str(output))}
times = []
[renderdoc]
library = {json.dumps(str(self.fake))}
helper = {json.dumps(str(self.helper))}
capture_prefix = {json.dumps(str(output / 'frame'))}
times = ["0.02"]
timeout = "0.1"
''')
        env = {key: value for key, value in os.environ.items() if key not in
               {"DISPLAY", "LD_PRELOAD", "RUZU_CAPTURE_CONTROL_FD", "FAKE_CAPTURE_MODE"}}
        dry_run = subprocess.run([binary, str(config), "--dry-run"], env=env,
                                 capture_output=True, text=True, timeout=5)
        self.assertEqual(dry_run.returncode, 0, dry_run.stderr)
        self.assertFalse(output.exists(), "dry-run created output")
        run = subprocess.run([binary, str(config)], env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(run.returncode, 0, run.stderr)
        report = json.loads((output / "renderdoc-manifest.json").read_text())
        self.assertIsNone(report["error"])
        self.assertTrue(report["captures"][0]["success"])
        self.assertEqual(report["captures"][0]["scheduled_us"], 20_000)
        self.assertGreaterEqual(report["captures"][0]["actual_us"], 20_000)
        self.assertTrue((output / "frame_synthetic.rdc").is_file())
        self.assertTrue((output / "capture-manifest.json").is_file())


if __name__ == "__main__":
    unittest.main()
