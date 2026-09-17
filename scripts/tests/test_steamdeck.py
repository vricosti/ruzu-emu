"""Offline tests for the dedicated container packaging path (no Docker daemon)."""
import hashlib
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HOST = load("steamdeck_host", "scripts/package-steamdeck.py")
PACKAGE = load("steamdeck_package", "scripts/steamdeck/package.py")


class SteamDeckTests(unittest.TestCase):
    def test_dry_run_dispatch_and_isolation(self):
        result = subprocess.run(["sh", "build.sh", "steamdeck", "--dry-run", "--jobs", "3"],
                                cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("docker build --platform linux/amd64", result.stdout)
        self.assertIn("dst=/source,readonly", result.stdout)
        self.assertNotIn("CARGO_TARGET_DIR=", result.stdout)
        self.assertIn("build.sh 3", result.stdout)
        self.assertNotIn("--privileged", result.stdout)
        self.assertNotIn("/dev/dri", result.stdout)
        self.assertNotIn("GDK_BACKEND", result.stdout)

    def test_invalid_job_count_and_options(self):
        for args in (["--jobs", "0"], ["--official"], ["--skip-deps"]):
            result = subprocess.run(["sh", "build.sh", "steamdeck", *args], cwd=ROOT,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)

    def test_missing_docker_stops_without_writes(self):
        with patch.object(HOST.shutil, "which", return_value=None):
            with self.assertRaisesRegex(RuntimeError, "Docker is required"):
                HOST.preflight(ROOT)

    def test_low_disk_stops_before_build(self):
        with patch.object(HOST.shutil, "which", return_value="docker"), \
             patch.object(HOST.subprocess, "run"), \
             patch.object(HOST.shutil, "disk_usage", return_value=HOST.shutil._ntuple_diskusage(100, 99, 1)):
            with self.assertRaisesRegex(RuntimeError, "35 GiB"):
                HOST.preflight(ROOT)

    def test_host_without_zen2_features_can_build(self):
        with patch.object(HOST.platform, "system", return_value="Linux"), \
             patch.object(HOST.platform, "machine", return_value="x86_64"), \
             patch.object(HOST.Path, "read_text", side_effect=AssertionError("no CPU gate")), \
             patch.object(HOST.shutil, "which", return_value="docker"), \
             patch.object(HOST.subprocess, "run"), \
             patch.object(HOST.subprocess, "check_output", return_value=""), \
             patch.object(HOST.shutil, "disk_usage", return_value=HOST.shutil._ntuple_diskusage(
                 2 * HOST.MIN_FREE_BYTES, 0, 2 * HOST.MIN_FREE_BYTES)):
            HOST.preflight(ROOT)

    def test_target_flags_and_downloads_are_pinned(self):
        dockerfile = (ROOT / "scripts/steamdeck/Dockerfile").read_text()
        entry = (ROOT / "scripts/steamdeck/build.sh").read_text()
        self.assertIn("@sha256:", dockerfile)
        self.assertIn("snapshot.debian.org/archive/debian/20260114T000000Z", dockerfile)
        self.assertIn("sha256sum -c -", dockerfile)
        self.assertNotIn("/main/", dockerfile)
        self.assertIn("target-cpu=znver2", entry)
        self.assertIn("-march=znver2 -mtune=znver2", entry)
        self.assertIn("cargo build --locked --release", entry)
        self.assertIn("--target x86_64-unknown-linux-gnu", entry)
        self.assertIn("unset CARGO_TARGET_DIR", entry)
        self.assertIn("--target-dir /output/build", entry)
        self.assertNotIn("panic=abort", entry)

    def test_packaging_contract_and_checksum(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)
            binary = output / "build/x86_64-unknown-linux-gnu/release/ruzu"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"synthetic binary")
            libraries = output / "system-libraries"
            libraries.mkdir()
            for name in ("libasound.so.2", "libjack.so.0", "libpulse.so.0",
                         "libpipewire-0.3.so.0", "libudev.so.1", "libdecor-0.so.0"):
                (libraries / name).touch()
            seen = []

            def run(command, **kwargs):
                seen.append(command)
                if command[0] == "strip":
                    return
                env = kwargs["env"]
                self.assertEqual(env["ADD_HOOKS"], "wayland-is-broken.hook")
                self.assertEqual(env["OPTIMIZE_LAUNCH"], "0")
                self.assertEqual(env["STRACE_MODE"], "0")
                self.assertEqual(env["GTK_DIR"], "gtk-4.0")
                for name in ("DEPLOY_SDL", "DEPLOY_PULSE", "DEPLOY_PIPEWIRE"):
                    self.assertEqual(env[name], "1")
                for name in ("DEPLOY_GTK", "DEPLOY_GLIBC", "DEPLOY_VULKAN", "DEPLOY_OPENGL"):
                    self.assertEqual(env[name], "1")
                self.assertTrue(env["XDG_CONFIG_HOME"].startswith(temp))
                appdir = Path(env["APPDIR"])
                if command[1] != "--make-appimage":
                    self.assertEqual(command[0], "/opt/quick-sharun")
                    self.assertIn(str(libraries / "libasound.so.2"), command)
                    self.assertIn(str(libraries / "libjack.so.0"), command)
                    (appdir / "lib").mkdir(parents=True)
                    (appdir / "lib/libc.so.6").touch()
                    (appdir / "bin").mkdir()
                    (appdir / "bin/05-wayland-is-broken.hook").write_text("export GDK_BACKEND=x11")
                else:
                    self.assertTrue((appdir / "share/doc/ruzu/build-info.txt").is_file())
                    (Path(env["OUTPATH"]) / env["OUTNAME"]).write_bytes(b"synthetic AppImage")

            with patch.dict(PACKAGE.os.environ, {"OPTIMIZE_LAUNCH": "1", "STRACE_MODE": "1"}), \
                 patch.object(PACKAGE.subprocess, "run", side_effect=run):
                PACKAGE.package("test-revision", ROOT, output, libraries)
            artifact = output / "artifacts/Ruzu-SteamDeck-test-revision-x86_64.AppImage"
            checksum = hashlib.sha256(artifact.read_bytes()).hexdigest()
            self.assertEqual(Path(str(artifact) + ".sha256").read_text(),
                             f"{checksum}  {artifact.name}\n")
            self.assertEqual(len(seen), 3)
            self.assertEqual(binary.read_bytes(), b"synthetic binary")

    def test_bad_revision_rejected_before_packaging(self):
        with self.assertRaisesRegex(ValueError, "Invalid artifact"):
            PACKAGE.package("../escape")

    def test_missing_dynamic_library_stops_before_packaging(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(PACKAGE.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "Missing runtime library"):
                PACKAGE.package("test", ROOT, Path(temp), Path(temp))
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
