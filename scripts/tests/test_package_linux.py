"""Exercise Debian version selection without building or publishing packages."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(shutil.which("dpkg-deb") and shutil.which("sh"),
                     "Debian packaging tools required")
class DebianPackageVersion(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="ruzu-deb-version-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / "scripts").mkdir()
        for name in ("package-linux.sh", "package-revision.sh"):
            shutil.copy2(ROOT / "scripts" / name, self.root / "scripts" / name)
        (self.root / "target/release").mkdir(parents=True)
        (self.root / "target/release/ruzu").touch(mode=0o755)
        (self.root / ".gitignore").write_text("target/\nbin/\n")
        (self.root / "bin").mkdir()
        cargo = self.root / "bin/cargo"
        cargo.write_text('#!/bin/sh\necho "path+file:///fixture#ruzu@0.1.0"\n')
        cargo.chmod(0o755)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.hash = self.git("rev-parse", "HEAD")[:12]

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def check_version(self, expected):
        result = subprocess.run(
            ["sh", "scripts/package-linux.sh"], cwd=self.root,
            env=dict(os.environ, PATH=f"{self.root / 'bin'}:{os.environ['PATH']}"),
            input="n\n", capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"Debian version: {expected}\n", result.stdout)
        self.assertIn("Packaging cancelled", result.stdout)
        subprocess.run(["dpkg", "--validate-version", expected], check=True)

    def test_tag_overrides_older_cargo_version(self):
        for tag in ("v0.1.1-rc7", "v0.1.1", "v0.2.0-rc.1"):
            with self.subTest(tag=tag):
                self.git("tag", "-a", tag, "-m", "release")
                self.check_version(tag[1:])
                self.git("tag", "-d", tag)

    def test_dirty_tag_retains_release_version(self):
        self.git("tag", "v0.1.1-rc7")
        (self.root / "modified.txt").write_text("dirty")
        self.check_version(f"0.1.1-rc7+git{self.hash}.dirty")

    def test_untagged_and_version_like_branch_use_cargo(self):
        self.check_version(f"0.1.0+git{self.hash}")
        self.git("switch", "-qc", "v0.1.1")
        self.check_version(f"0.1.0+git{self.hash}")
        (self.root / "modified.txt").write_text("dirty")
        self.check_version(f"0.1.0+git{self.hash}.dirty")


if __name__ == "__main__":
    unittest.main()
