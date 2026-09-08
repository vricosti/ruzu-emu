"""Exercise the shell and PowerShell release guards without building an emulator."""
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]


class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ruzu-release-check-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.email", "release-test@example.invalid")
        self.git("config", "user.name", "Release Test")
        (self.repo / "Cargo.toml").write_text(
            '[workspace]\nmembers=["app"]\nresolver="2"\n'
            '[workspace.package]\nversion="0.0.2"\n'
        )
        (self.repo / "app/src").mkdir(parents=True)
        (self.repo / "app/Cargo.toml").write_text(
            '[package]\nname="ruzu"\nversion.workspace=true\nedition="2021"\n'
        )
        (self.repo / "app/src/main.rs").write_text("fn main() {}\n")
        subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=self.repo,
                       check=True, capture_output=True)
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()

    def runners(self):
        yield ["sh", str(SCRIPTS / "check-release.sh"), str(self.repo)]
        if shutil.which("pwsh"):
            yield ["pwsh", "-NoProfile", "-File", str(SCRIPTS / "check-release.ps1"),
                   "-Repository", str(self.repo)]

    def assert_checks(self, success, message=""):
        for command in self.runners():
            with self.subTest(runtime=command[0]):
                result = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, success, result.stderr)
                if success:
                    self.assertEqual(result.stdout.strip(), "0.0.2")
                else:
                    self.assertIn(message, result.stderr)

    def test_clean_matching_detached_tag(self):
        self.git("tag", "-a", "v0.0.2", "-m", "release")
        self.git("switch", "--detach", "v0.0.2")
        self.assert_checks(True)

    def test_missing_tag(self):
        self.assert_checks(False, "exact tag")

    def test_mismatched_tag(self):
        self.git("tag", "v0.0.3")
        self.assert_checks(False, "does not match Cargo version")

    def test_modified_tracked_file(self):
        self.git("tag", "v0.0.2")
        (self.repo / "app/src/main.rs").write_text("fn main() { println!(\"changed\"); }\n")
        self.assert_checks(False, "clean checkout")
        self.git("add", ".")
        self.assert_checks(False, "clean checkout")

    def test_untracked_file(self):
        self.git("tag", "v0.0.2")
        (self.repo / "untracked.txt").write_text("not released")
        self.assert_checks(False, "clean checkout")

    def test_uninitialized_submodule(self):
        # A gitlink can reference this fixture's existing commit without network I/O.
        (self.repo / ".gitmodules").write_text(
            '[submodule "dep"]\npath = dep\nurl = ./unused\n'
        )
        self.git("add", ".gitmodules")
        self.git("update-index", "--add", "--cacheinfo",
                 "160000," + self.git("rev-parse", "HEAD") + ",dep")
        (self.repo / "dep").mkdir()
        self.git("commit", "-qm", "add submodule")
        self.git("tag", "v0.0.2")
        self.assert_checks(False, "submodules must be initialized")

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell is not installed")
    def test_force_does_not_override_cargo_version(self):
        command = list(self.runners())[-1] + ["-ForcePackage", "-Version", "9.9.9"]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match Cargo version", result.stderr)


if __name__ == "__main__":
    unittest.main()
