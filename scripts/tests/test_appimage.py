"""Packaging regressions without a GPU, network, root access or Cargo build."""
import hashlib
import io
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from contextlib import redirect_stdout

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("package_appimage", ROOT / "scripts/package-appimage.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class AppImageTests(unittest.TestCase):
    def test_generic_package_name_and_guidance(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name in ("target/release/ruzu", "dist/linux/AppRun", "LICENSE"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"test")
            metadata = []

            def fake_run(*args, **kwargs):
                output = ""
                if args[0] == "readelf":
                    output = "Advanced Micro Devices X86-64"
                elif args[0] == "sh":
                    output = "v0.1.1-rc1\n"
                elif "--output" in args:
                    Path(kwargs["env"]["OUTPUT"]).write_bytes(b"packaged")
                    appdir = Path(args[args.index("--appdir") + 1])
                    metadata.append((appdir / "usr/share/doc/ruzu/build-info.txt").read_text())
                return subprocess.CompletedProcess(args, 0, stdout=output)

            output = io.StringIO()
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": "", "CARGO_BUILD_TARGET": "",
                                         "XDG_CACHE_HOME": str(root / "cache")}), \
                    patch.object(PACKAGE.platform, "system", return_value="Linux"), \
                    patch.object(PACKAGE.platform, "machine", return_value="x86_64"), \
                    patch.object(PACKAGE.shutil, "which", return_value="/mock/tool"), \
                    patch.object(PACKAGE, "download", side_effect=lambda cache, name, _: cache / name), \
                    patch.object(PACKAGE, "run", side_effect=fake_run), \
                    patch.object(PACKAGE, "remove_plugin_overrides"), \
                    patch.object(PACKAGE, "glibc_requirement", return_value="2.39"), \
                    redirect_stdout(output):
                PACKAGE.package(root)
            artifact = root / "target/release/Ruzu-Linux-v0.1.1-rc1-x86_64.AppImage"
            self.assertEqual(artifact.read_bytes(), b"packaged")
            self.assertIn(str(artifact), output.getvalue())
            self.assertIn("target Linux distributions", output.getvalue())
            self.assertNotIn("Steam Deck", output.getvalue() + "".join(metadata))

    def test_plugin_overrides_removed_before_final_packaging(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "usr/lib").mkdir(parents=True)
            (root / "apprun-hooks").mkdir()
            for name in ("libvulkan.so.1", "libGL.so.1", "libgtk-4.so.1"):
                (root / "usr/lib" / name).write_bytes(b"test")
            (root / "AppRun").write_text("old wrapper")
            (root / "apprun-hooks/linuxdeploy-plugin-gtk.sh").write_text("export GDK_BACKEND=x11")
            PACKAGE.remove_plugin_overrides(root)
            self.assertEqual([p.name for p in (root / "usr/lib").iterdir()], ["libgtk-4.so.1"])
            self.assertFalse((root / "AppRun").exists())
            self.assertEqual(list((root / "apprun-hooks").iterdir()), [])

    def test_unsupported_platform_stops_before_build(self):
        result = subprocess.run(
            ["sh", "-c", 'uname() { echo Darwin; }; set -- appimage; . ./build.sh', "./build.sh"],
            cwd=ROOT, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Linux x86_64", result.stderr)

    def test_custom_target_stops_before_build(self):
        result = subprocess.run(["sh", "build.sh", "appimage"], cwd=ROOT,
                                env=dict(os.environ, CARGO_TARGET_DIR="/tmp/custom-target"),
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unset CARGO_TARGET_DIR", result.stderr)

    def test_dispatch_release_gui_only_and_reject_official(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            shutil.copy2(ROOT / "build.sh", root)
            (root / "scripts").mkdir()
            script = root / "scripts/build-linux.sh"
            script.write_text('#!/bin/sh\nprintf "%s\\n" "$RUZU_LINUX_APPIMAGE" "$@"\n')
            script.chmod(0o755)
            for option in ([], ["--skip-deps"]):
                result = subprocess.run(["sh", "build.sh", "appimage", *option], cwd=root,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(),
                                 ["1", "--release", *option, "--", "--bin", "ruzu"])
            result = subprocess.run(["sh", "build.sh", "appimage", "--official"], cwd=root,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Unsupported", result.stderr)

    def test_launcher_preserves_settings_and_arguments(self):
        with tempfile.TemporaryDirectory(prefix="ruzu appdir ") as temp:
            root = Path(temp)
            shutil.copy2(ROOT / "dist/linux/AppRun", root)
            (root / "usr/bin").mkdir(parents=True)
            binary = root / "usr/bin/ruzu"
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "${GDK_BACKEND-unset}" '
                              '"${GTK_THEME-unset}" "$XDG_CONFIG_HOME" "$@"\n')
            binary.chmod(0o755)
            for backend in (None, "wayland"):
                env = dict(os.environ, GTK_THEME="Custom", XDG_CONFIG_HOME="/user/config")
                env.pop("GDK_BACKEND", None)
                if backend:
                    env["GDK_BACKEND"] = backend
                result = subprocess.run(["sh", str(root / "AppRun"), "-g", "homebrew path.nro"],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(),
                                 [backend or "unset", "Custom", "/user/config", "-g", "homebrew path.nro"])

    def test_download_verifies_cache_and_rejects_corruption(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            payload = root / "tool"
            payload.write_bytes(b"verified")
            checksum = hashlib.sha256(b"verified").hexdigest()
            with patch.object(PACKAGE.urllib.request, "urlopen") as fetch:
                self.assertEqual(PACKAGE.download(root, "tool", ("https://invalid", checksum)), payload)
                fetch.assert_not_called()
            payload.write_bytes(b"stale")
            source = root / "source"
            source.write_bytes(b"corrupt")
            with self.assertRaisesRegex(RuntimeError, "Checksum mismatch"):
                PACKAGE.download(root, "tool", (source.as_uri(), checksum))
            self.assertEqual(payload.read_bytes(), b"stale")
            source.write_bytes(b"verified")
            PACKAGE.download(root, "tool", (source.as_uri(), checksum))
            self.assertEqual(payload.read_bytes(), b"verified")
            self.assertTrue(os.access(payload, os.X_OK))


if __name__ == "__main__":
    unittest.main()
