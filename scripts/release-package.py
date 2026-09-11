#!/usr/bin/env python3
"""Interactive release transaction shared by Windows and macOS (Python 3.9+)."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time


VERSION = re.compile(r"(?:v)?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z")
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


def suggested_version(tags):
    versions = [tuple(map(int, match.groups())) for tag in tags
                if tag.startswith("v") and (match := VERSION.fullmatch(tag))]
    if not versions:
        return "0.0.1"
    major, minor, patch = max(versions)
    return f"{major}.{minor}.{patch + 1}"


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
    suggestion = suggested_version(tags)
    answer = input(f"Release version [{suggestion}] (Ctrl+C to cancel): ").strip() or suggestion
    match = VERSION.fullmatch(answer)
    if not match:
        raise RuntimeError("Use a numeric version such as 0.0.5 or v0.0.5")
    version = ".".join(match.groups())
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
    # Rebuild after tagging so embedded SCM metadata and artifact names agree.
    build(repo, platform, package=True, skip_deps=True)
    unchanged()
    if git(repo, "rev-parse", f"refs/tags/{tag}^{{commit}}") != commit:
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
