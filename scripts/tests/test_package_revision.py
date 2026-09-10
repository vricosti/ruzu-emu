"""Compare Git package naming on Windows and POSIX without compiling Ruzu."""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]


class PackageRevision(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ruzu-package-name-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "Package Test")
        (self.repo / "tracked.txt").write_text("original\n")
        (self.repo / ".gitignore").write_text("output/\n")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.hash = self.git("rev-parse", "HEAD")[:12]

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()

    def runners(self):
        shell = shutil.which("sh")
        if not shell and os.name == "nt":
            git_root = Path(shutil.which("git")).resolve().parents[1]
            candidate = git_root / "bin/sh.exe"
            if candidate.exists():
                shell = str(candidate)
        if shell:
            yield [shell, str(SCRIPTS / "package-revision.sh"), str(self.repo)]
        powershell = shutil.which("pwsh") or shutil.which("powershell")
        if powershell:
            yield [powershell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                   str(SCRIPTS / "package-revision.ps1"), "-Repository", str(self.repo)]

    def check(self, expected):
        commands = list(self.runners())
        self.assertTrue(commands, "No supported script runtime found")
        for command in commands:
            with self.subTest(runtime=command[0]):
                result = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)

    def test_clean_branch_without_cargo(self):
        self.check(f"main-{self.hash}")

    def test_branch_filename_sanitization(self):
        self.git("switch", "-c", "feat/ui-font+scale")
        self.check(f"feat-ui-font-scale-{self.hash}")

    def test_detached(self):
        self.git("switch", "--detach")
        self.check(f"detached-{self.hash}")

    def test_annotated_version_tag(self):
        self.git("tag", "-a", "v0.0.5", "-m", "release")
        self.check("v0.0.5")
        self.git("switch", "--detach", "v0.0.5")
        self.check("v0.0.5")

    def test_lightweight_prerelease_and_other_tags(self):
        self.git("tag", "nightly")
        self.check(f"main-{self.hash}")
        self.git("tag", "v0.0.5-rc.1")
        self.check("v0.0.5-rc.1")
        self.git("tag", "v0.0.5")
        self.check("v0.0.5")

    def test_tag_must_point_at_head(self):
        self.git("tag", "v0.0.5")
        self.git("commit", "--allow-empty", "-qm", "next")
        self.hash = self.git("rev-parse", "HEAD")[:12]
        self.check(f"main-{self.hash}")

    def test_dirty_overrides_tag_staged_and_unstaged(self):
        self.git("tag", "v0.0.5")
        (self.repo / "tracked.txt").write_text("modified\n")
        self.check(f"main-{self.hash}-dirty")
        self.git("add", "tracked.txt")
        self.check(f"main-{self.hash}-dirty")

    def test_untracked_and_ignored(self):
        (self.repo / "output").mkdir()
        (self.repo / "output/artifact.zip").write_text("ignored")
        self.check(f"main-{self.hash}")
        (self.repo / "new.txt").write_text("untracked")
        self.check(f"main-{self.hash}-dirty")

    def test_uninitialized_submodule_is_dirty(self):
        (self.repo / ".gitmodules").write_text(
            '[submodule "dep"]\npath = dep\nurl = ./unused\n')
        self.git("add", ".gitmodules")
        self.git("update-index", "--add", "--cacheinfo",
                 "160000," + self.git("rev-parse", "HEAD") + ",dep")
        (self.repo / "dep").mkdir()
        self.git("commit", "-qm", "submodule")
        self.hash = self.git("rev-parse", "HEAD")[:12]
        self.git("tag", "v0.0.5")
        self.check(f"main-{self.hash}-dirty")


if __name__ == "__main__":
    unittest.main()
