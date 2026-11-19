#!/usr/bin/env python3
"""Interactive release transaction shared by Windows and macOS (Python 3.9+)."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time


# A release is X.Y.Z; a release candidate is X.Y.Z-rcN with N >= 1. The tag is
# the version prefixed with "v" (v0.1.2, v0.1.1-rc9). Nothing else is accepted:
# common/build.rs only shows a tag in the window title when it equals "v" plus
# the Cargo version, so the two must be written from the same string.
VERSION = re.compile(
    r"(?:v)?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-rc([1-9][0-9]*))?\Z")
INDEX_LOCK_ATTEMPTS = 10


def run(repo, *args, capture=False, env=None):
    if args[:2] in (("git", "add"), ("git", "commit")):
        # Status refreshes (e.g. IDE integration) may briefly own index.lock.
        # Retry only Git's explicit failure to acquire that lock. Never unlink
        # it, retry a push, or repeat unrelated commit/hook failures.
        environment = dict(os.environ if env is None else env)
        environment.update(LC_ALL="C", LANGUAGE="C")
        for attempt in range(INDEX_LOCK_ATTEMPTS):
            result = subprocess.run(args, cwd=repo, text=True, env=environment,
                                    stdout=subprocess.PIPE if capture else None,
                                    stderr=subprocess.PIPE)
            busy = (result.returncode == 128 and
                    re.search(r"fatal: Unable to create '[^\r\n]*[/\\]index\.lock': File exists", result.stderr))
            if busy and attempt + 1 < INDEX_LOCK_ATTEMPTS:
                print(f"Git index is busy; retrying in 1 second ({attempt + 1}/{INDEX_LOCK_ATTEMPTS - 1}).",
                      file=sys.stderr, flush=True)
                time.sleep(1)
                continue
            if result.stderr:
                print(result.stderr, end="", file=sys.stderr)
            if busy:
                print("Git index is still locked. Stop the competing Git operation and retry; "
                      "the lock has NOT been removed.", file=sys.stderr)
            result.check_returncode()
            return result.stdout.strip() if capture else None
    result = subprocess.run(args, cwd=repo, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None, env=env)
    return result.stdout.strip() if capture else None


def git(repo, *args):
    # Our read-only inspections need not refresh/write the index on disk.
    environment = dict(os.environ, GIT_OPTIONAL_LOCKS="0")
    return run(repo, "git", *args, capture=True, env=environment)


def clean(repo):
    if git(repo, "status", "--porcelain", "--untracked-files=all", "--ignore-submodules=none"):
        raise RuntimeError("Release requires a clean checkout. Commit or stash your changes first.")
    if any(line.startswith(("-", "+", "U")) for line in
           git(repo, "submodule", "status", "--recursive").splitlines()):
        raise RuntimeError("Initialize submodules at their recorded commits before releasing.")


def parse_version(text):
    """Returns (major, minor, patch, rc) with rc None for a final release, or None."""
    match = VERSION.fullmatch(text)
    if not match:
        return None
    major, minor, patch, candidate = match.groups()
    return int(major), int(minor), int(patch), int(candidate) if candidate else None


def format_version(version):
    major, minor, patch, candidate = version
    base = f"{major}.{minor}.{patch}"
    return base if candidate is None else f"{base}-rc{candidate}"


def version_order(version):
    # A release candidate precedes the final release of the same X.Y.Z.
    major, minor, patch, candidate = version
    return major, minor, patch, candidate is None, candidate or 0


def suggested_versions(tags):
    """Returns the default proposal and the alternative, from the latest version tag.

    After a release candidate (v0.1.1-rc8): the next candidate (0.1.1-rc9), or
    the final release (0.1.1). After a final release (v0.1.1): the next patch
    release (0.1.2), or its first candidate (0.1.2-rc1).
    """
    versions = [version for tag in tags
                if tag.startswith("v") and (version := parse_version(tag))]
    if not versions:
        return "0.0.1", "0.0.1-rc1"
    major, minor, patch, candidate = max(versions, key=version_order)
    if candidate is not None:
        return (format_version((major, minor, patch, candidate + 1)),
                format_version((major, minor, patch, None)))
    return (format_version((major, minor, patch + 1, None)),
            format_version((major, minor, patch + 1, 1)))


def suggested_version(tags):
    return suggested_versions(tags)[0]


def annotated_tag_commit(repo, tag):
    """Commit behind `tag`, which must be an annotated tag."""
    if git(repo, "cat-file", "-t", f"refs/tags/{tag}") != "tag":
        raise RuntimeError(f"Tag {tag} is not an annotated tag")
    return git(repo, "rev-parse", f"refs/tags/{tag}^{{commit}}")


def update_manifest(text, version):
    section = re.search(r"(?ms)^\[workspace\.package\][^\r\n]*\r?\n(.*?)(?=^\[|\Z)", text)
    if not section:
        raise RuntimeError("Missing [workspace.package] in Cargo.toml")
    body, count = re.subn(r'(?m)^(version\s*=\s*)"[^"\r\n]+"',
                          lambda m: m[1] + '"' + version + '"', section[1])
    if count != 1:
        raise RuntimeError("Expected exactly one workspace version")
    return text[:section.start(1)] + body + text[section.end(1):]


def build(repo, platform, package, skip_deps):
    if platform == "windows":
        if package:
            run(repo, "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
                "-File", str(repo / "dist/package-windows.ps1"))
        else:
            run(repo, "cargo", "build", "--locked", "--release", "-p", "ruzu", "-p", "ruzu_cmd")
    else:
        environment = os.environ.copy()
        if platform == "linux":
            environment["RUZU_LINUX_PACKAGE"] = "1" if package else "0"
            args = ["sh", str(repo / "scripts/build-linux.sh"), "--release"]
        else:
            environment["RUZU_MACOS_PACKAGE"] = "1" if package else "0"
            args = ["sh", str(repo / "scripts/build-macos.sh"), "--release"]
        if skip_deps or package:
            args.append("--skip-deps")
        run(repo, *args, env=environment)


def release(repo, platform, skip_deps=False):
    clean(repo)
    branch = git(repo, "symbolic-ref", "--quiet", "--short", "HEAD")
    # Never overwrite remote history or silently publish detached HEAD to main.
    git(repo, "remote", "get-url", "--push", "origin")
    run(repo, "git", "fetch", "origin", "--tags")
    tags = git(repo, "tag", "--list").splitlines()
    suggestion, alternative = suggested_versions(tags)
    answer = input(f"Release version: [1] {suggestion} (default), [2] {alternative}, "
                   "or type another one (Ctrl+C to cancel): ").strip()
    answer = {"": suggestion, "1": suggestion, "2": alternative}.get(answer, answer)
    parsed = parse_version(answer)
    if parsed is None:
        raise RuntimeError("Use a version such as 0.1.2 or 0.1.2-rc1 (a leading v is accepted)")
    version = format_version(parsed)
    tag = "v" + version
    if tag in tags:
        raise RuntimeError(f"Tag {tag} already exists; it will not be replaced")
    print(f"Will commit Cargo version {version}, build, tag {tag}, package and push to origin/{branch}.")
    if input("Continue? [y/N]: ").strip().lower() not in ("y", "yes", "o", "oui"):
        print("Cancelled; no files or commits changed.")
        return
    clean(repo)
    manifest = repo / "Cargo.toml"
    with manifest.open(encoding="utf-8", newline="") as source:
        text = source.read()
    updated = update_manifest(text, version)
    with manifest.open("w", encoding="utf-8", newline="") as output:
        output.write(updated)
    # Refresh workspace versions without upgrading registry dependencies.
    run(repo, "cargo", "update", "--workspace", "--offline")
    changed = set(git(repo, "diff", "--name-only").splitlines())
    if not changed <= {"Cargo.toml", "Cargo.lock"}:
        raise RuntimeError("Unexpected source edits while updating Cargo; review the checkout")
    run(repo, "git", "add", "--", "Cargo.toml", "Cargo.lock")
    run(repo, "git", "commit", "--allow-empty", "-m", f"chore: release {tag}",
        "--", "Cargo.toml", "Cargo.lock")
    commit = git(repo, "rev-parse", "HEAD")

    def unchanged():
        clean(repo)
        if (git(repo, "rev-parse", "HEAD") != commit or
                git(repo, "symbolic-ref", "--quiet", "--short", "HEAD") != branch):
            raise RuntimeError("Release checkout changed; refusing to tag or publish")

    build(repo, platform, package=False, skip_deps=skip_deps)
    unchanged()
    run(repo, "git", "tag", "-a", tag, commit, "-m", tag)
    if annotated_tag_commit(repo, tag) != commit:
        raise RuntimeError(f"Tag {tag} does not point at the release commit")
    # Rebuild after tagging so embedded SCM metadata and artifact names agree.
    build(repo, platform, package=True, skip_deps=True)
    unchanged()
    if annotated_tag_commit(repo, tag) != commit:
        raise RuntimeError("Release tag changed during packaging")
    run(repo, "git", "push", "--atomic", "origin",
        f"{commit}:refs/heads/{branch}", f"refs/tags/{tag}:refs/tags/{tag}")
    print(f"Published {tag} and origin/{branch}; local package creation succeeded.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=("windows", "macos", "linux"), required=True)
    parser.add_argument("--skip-deps", action="store_true")
    args = parser.parse_args()
    try:
        release(Path(__file__).resolve().parents[1], args.platform, args.skip_deps)
    except (RuntimeError, OSError, subprocess.CalledProcessError, EOFError, KeyboardInterrupt) as error:
        print(f"Release stopped: {error}\nLocal files/commit/tag, if created, are retained for review. "
              "Nothing is pushed before both builds and packaging succeed.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
