#!/usr/bin/env python3
"""Build the current working tree without changing Git, host packages or settings."""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
MIN_FREE_BYTES = 35 * 1024**3


def commands(root, jobs):
    context = root / "scripts/steamdeck"
    image = "ruzu-steamdeck:" + hashlib.sha256(
        (context / "Dockerfile").read_bytes()).hexdigest()[:16]
    output = root / "target/steamdeck"
    return [
        ["docker", "build", "--platform", "linux/amd64", "-t", image, str(context)],
        ["docker", "run", "--rm", "--platform", "linux/amd64",
         "--user", f"{os.getuid()}:{os.getgid()}",
         "--mount", f"type=bind,src={root},dst=/source,readonly",
         "--mount", f"type=bind,src={output},dst=/output",
         "--env", "CARGO_HOME=/output/cargo",
         "--env", "RUSTUP_HOME=/usr/local/rustup",
         "--env", "GIT_CONFIG_COUNT=1", "--env", "GIT_CONFIG_KEY_0=safe.directory",
         "--env", "GIT_CONFIG_VALUE_0=*",
         image, "sh", "/source/scripts/steamdeck/build.sh", str(jobs)],
    ]


def preflight(root):
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("Steam Deck packaging requires a Linux x86_64 build host")
    # A target build need not run on its host. Cargo uses an explicit target
    # and packaging disables tracing/launch profiling of the optimized binary.
    print("Building for Steam Deck (Zen 2), not for the host CPU. "
          "The AppImage requires a compatible CPU to run; use ./build.sh appimage "
          "for a generic Linux package.")
    if not shutil.which("docker"):
        raise RuntimeError("Docker is required; no host packages will be installed automatically")
    subprocess.run(["docker", "info"], check=True, stdout=subprocess.DEVNULL)
    if shutil.disk_usage(root).free < MIN_FREE_BYTES:
        raise RuntimeError("At least 35 GiB free is required on the source/output filesystem; "
                           "also reserve space for Docker images. Nothing was deleted.")
    status = subprocess.check_output(
        ["git", "submodule", "status", "--recursive"], cwd=root, text=True)
    if any(line.startswith(("-", "U")) for line in status.splitlines()):
        raise RuntimeError("Initialize/resolve submodules before packaging")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=2, help="Cargo compilation jobs (default: 2)")
    parser.add_argument("--dry-run", action="store_true", help="print commands without building")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("Steam Deck packaging requires Linux x86_64")
    if "," in str(ROOT):
        parser.error("Docker bind mounts require a checkout path without commas")
    pipeline = commands(ROOT, args.jobs)
    if args.dry_run:
        for command in pipeline:
            print(shlex.join(command))
        return
    preflight(ROOT)
    (ROOT / "target/steamdeck").mkdir(parents=True, exist_ok=True)
    for command in pipeline:
        subprocess.run(command, check=True)
    print("Steam Deck AppImage: target/steamdeck/artifacts/ (hardware validation required)")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Steam Deck packaging failed: {error}") from error
