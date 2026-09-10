"""Check wrapper dispatch using disposable stubs, without builds or publication."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PackageModes(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ruzu-package-mode-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "dist").mkdir()

    @unittest.skipUnless(os.name == "nt", "PowerShell packaging confirmation")
    def test_windows_refusal_displays_names_and_stops_before_build(self):
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
             str(ROOT / "dist/package-windows.ps1")],
            cwd=ROOT, input="n\n", capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Package directory: Ruzu-Windows-", result.stdout)
        self.assertIn("-installer.exe", result.stdout)
        self.assertIn(".zip", result.stdout)
        self.assertIn("Packaging cancelled; no package files were changed.", result.stdout)

    @unittest.skipUnless(os.name == "nt", "Windows batch dispatch")
    def test_windows_default_local_and_explicit_official(self):
        shutil.copy(ROOT / "build.bat", self.root)
        (self.root / "scripts/build.ps1").write_text('''
param([string]$EnvironmentFile, [string]$Action, [switch]$Official, [Alias("ForcePackage")][switch]$Development)
$mode = if ($Official) { '1' } else { '0' }
@('set "RUZU_BUILD_ACTION=package"', 'set "RUZU_BUILD_PROFILE=release"',
  "set `"RUZU_OFFICIAL_PACKAGE=$mode`"") | Set-Content $EnvironmentFile
''')
        (self.root / "scripts/release-package.py").write_text('print("OFFICIAL_PATH")\n')
        (self.root / "dist/package-windows.ps1").write_text('param($Profile)\nWrite-Output "LOCAL_PATH"\n')
        for args, expected, excluded in [([], "LOCAL_PATH", "OFFICIAL_PATH"),
                                         (["-Official"], "OFFICIAL_PATH", "LOCAL_PATH"),
                                         (["-Development"], "LOCAL_PATH", "OFFICIAL_PATH"),
                                         (["-ForcePackage"], "LOCAL_PATH", "OFFICIAL_PATH")]:
            with self.subTest(args=args):
                result = subprocess.run(["cmd.exe", "/c", "build.bat", "package", *args],
                                        cwd=self.root, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(expected, result.stdout)
                self.assertNotIn(excluded, result.stdout)

    def test_shell_default_local_and_explicit_official(self):
        shell = shutil.which("sh")
        if not shell and os.name == "nt":
            shell = str(Path(shutil.which("git")).resolve().parents[1] / "bin/sh.exe")
        if not shell or not Path(shell).exists():
            self.skipTest("POSIX shell not available")
        shutil.copy(ROOT / "build.sh", self.root)
        # Emulate macOS and Python without executing a release transaction.
        (self.root / "bin").mkdir()
        python = self.root / "bin/python3"
        python.write_text('#!/bin/sh\necho OFFICIAL_PATH\n')
        python.chmod(0o755)
        (self.root / "scripts/package-revision.sh").write_text("exit 0\n")
        platform = self.root / "scripts/build-macos.sh"
        platform.write_text('#!/bin/sh\necho LOCAL_PATH\n')
        platform.chmod(0o755)
        for args, expected in [("", "LOCAL_PATH"), ("--official", "OFFICIAL_PATH"),
                               ("--development", "LOCAL_PATH")]:
            command = ('uname() { echo Darwin; }; PATH="$PWD/bin:$PATH"; export PATH; '
                       f'set -- package {args}; . ./build.sh')
            result = subprocess.run([shell, "-c", command, "./build.sh"], cwd=self.root,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(expected, result.stdout)
            self.assertNotIn("LOCAL_PATH" if args == "--official" else "OFFICIAL_PATH", result.stdout)


if __name__ == "__main__":
    unittest.main()
