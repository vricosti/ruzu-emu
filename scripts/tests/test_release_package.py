"""Exercise release sequencing against disposable real Git remotes, no builds."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

SPEC = importlib.util.spec_from_file_location(
    "release_package", Path(__file__).resolve().parents[1] / "release-package.py")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class GitIndexLockRetries(unittest.TestCase):
    def result(self, code, stderr=""):
        return subprocess.CompletedProcess(["git", "add"], code, stdout="", stderr=stderr)

    def busy(self):
        return self.result(128, "fatal: Unable to create 'D:/repo/.git/index.lock': File exists\n")

    def test_transient_lock_retries_add_and_commit(self):
        for command in ("add", "commit"):
            with self.subTest(command=command), patch.object(release.subprocess, "run",
                    side_effect=[self.busy(), self.result(0)]) as process, \
                    patch.object(release.time, "sleep") as sleep:
                release.run(Path("."), "git", command)
                self.assertEqual(process.call_count, 2)
                sleep.assert_called_once_with(1)

    def test_persistent_lock_is_bounded(self):
        with patch.object(release.subprocess, "run", return_value=self.busy()) as process, \
                patch.object(release.time, "sleep") as sleep:
            with self.assertRaises(subprocess.CalledProcessError):
                release.run(Path("."), "git", "add")
            self.assertEqual(process.call_count, release.INDEX_LOCK_ATTEMPTS)
            self.assertEqual(sleep.call_count, release.INDEX_LOCK_ATTEMPTS - 1)

    def test_unrelated_failure_is_not_retried(self):
        with patch.object(release.subprocess, "run", return_value=self.result(128, "fatal: bad path\n")) as process, \
                patch.object(release.time, "sleep") as sleep:
            with self.assertRaises(subprocess.CalledProcessError):
                release.run(Path("."), "git", "commit")
            process.assert_called_once()
            sleep.assert_not_called()


class ReleasePackage(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ruzu-release-flow-")
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        self.remote = root / "origin.git"
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        for args in [("init", "-q", "-b", "main"),
                     ("config", "user.name", "Test"),
                     ("config", "user.email", "test@example.invalid")]:
            release.git(self.repo, *args)
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.0.4"\n')
        (self.repo / "Cargo.lock").write_text("fixture\n")
        release.git(self.repo, "add", ".")
        release.git(self.repo, "commit", "-qm", "fixture")
        release.git(self.repo, "remote", "add", "origin", str(self.remote))
        release.git(self.repo, "push", "-u", "origin", "main")
        release.git(self.repo, "tag", "-a", "v0.0.4", "-m", "v0.0.4")
        release.git(self.repo, "push", "origin", "v0.0.4")
        self.original = release.git(self.repo, "rev-parse", "HEAD")
        actual_run = release.run

        def run(repo, *args, **kwargs):
            if args[0] == "cargo":
                (repo / "Cargo.lock").write_text("updated fixture\n")
                return None
            return actual_run(repo, *args, **kwargs)

        self.run_patch = patch.object(release, "run", side_effect=run)
        self.run_patch.start()
        self.addCleanup(self.run_patch.stop)

    def invoke(self, build, answers=("", "y")):
        with patch("builtins.input", side_effect=answers), patch.object(release, "build", side_effect=build):
            release.release(self.repo, "windows")

    def test_success_build_before_tag_and_atomic_publication(self):
        phases = []

        def build(repo, platform, package, skip_deps):
            phases.append(package)
            self.assertEqual("v0.0.5" in release.git(repo, "tag").splitlines(), package)
            self.assertNotIn("v0.0.5", release.git(repo, "ls-remote", "--tags", "origin"))

        self.invoke(build)
        self.assertEqual(phases, [False, True])
        commit = release.git(self.repo, "rev-parse", "HEAD")
        self.assertNotEqual(commit, self.original)
        self.assertEqual(release.git(self.repo, "cat-file", "-t", "v0.0.5"), "tag")
        self.assertEqual(release.git(self.repo, "tag", "-l", "v0.0.5", "--format=%(contents)"), "v0.0.5")
        self.assertIn(commit, release.git(self.repo, "ls-remote", "origin", "refs/heads/main"))
        self.assertIn("v0.0.5", release.git(self.repo, "ls-remote", "--tags", "origin"))
        self.assertIn('version = "0.0.5"', (self.repo / "Cargo.toml").read_text())

    def test_failed_build_has_no_new_tag_or_push(self):
        with self.assertRaises(RuntimeError):
            self.invoke(lambda *args, **kwargs: (_ for _ in ()).throw(RuntimeError("build failed")))
        self.assertNotIn("v0.0.5", release.git(self.repo, "tag").splitlines())
        self.assertIn(self.original, release.git(self.repo, "ls-remote", "origin", "refs/heads/main"))

    def test_failed_packaging_keeps_local_tag_but_does_not_publish(self):
        def build(repo, platform, package, skip_deps):
            if package:
                raise RuntimeError("NSIS failed")
        with self.assertRaises(RuntimeError):
            self.invoke(build)
        self.assertIn("v0.0.5", release.git(self.repo, "tag").splitlines())
        self.assertNotIn("v0.0.5", release.git(self.repo, "ls-remote", "--tags", "origin"))
        self.assertIn(self.original, release.git(self.repo, "ls-remote", "origin", "refs/heads/main"))

    def test_dirty_checkout_refused(self):
        (self.repo / "notes.txt").write_text("user notes")
        with self.assertRaisesRegex(RuntimeError, "clean checkout"):
            self.invoke(None)
        self.assertEqual(release.git(self.repo, "rev-parse", "HEAD"), self.original)

    def test_existing_tag_refused(self):
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            self.invoke(None, answers=("v0.0.4",))
        self.assertEqual(release.git(self.repo, "rev-parse", "HEAD"), self.original)

    def test_detached_checkout_refused(self):
        release.git(self.repo, "switch", "--detach")
        with self.assertRaises(subprocess.CalledProcessError):
            self.invoke(None)

    def test_atomic_push_rejects_remote_divergence_without_publishing_tag(self):
        def build(repo, platform, package, skip_deps):
            if package:
                tree = release.git(repo, "rev-parse", "HEAD^{tree}")
                competitor = release.git(repo, "commit-tree", tree, "-p", self.original,
                                         "-m", "concurrent remote change")
                release.git(repo, "push", "origin", f"{competitor}:refs/heads/main")
        with self.assertRaises(subprocess.CalledProcessError):
            self.invoke(build)
        self.assertNotIn("v0.0.5", release.git(self.repo, "ls-remote", "--tags", "origin"))

    def test_cancel(self):
        self.invoke(None, answers=("", "n"))
        self.assertEqual(release.git(self.repo, "rev-parse", "HEAD"), self.original)
        release.clean(self.repo)

    def test_version_suggestion_and_scoped_manifest_update(self):
        self.assertEqual(release.suggested_version(["v0.0.9", "v0.0.10", "nightly"]), "0.0.11")
        self.assertEqual(release.suggested_version([]), "0.0.1")
        text = '[workspace.package]\r\nversion = "0.0.4"\r\n[dependencies]\r\nversion = "other"\r\n'
        self.assertEqual(release.update_manifest(text, "0.0.5"), text.replace('"0.0.4"', '"0.0.5"'))


if __name__ == "__main__":
    unittest.main()
