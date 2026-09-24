#!/usr/bin/env python3
"""Package an existing Linux release; no source, Git or system configuration writes."""

import argparse
import fnmatch
import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import urllib.request


LINUXDEPLOY = (
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/"
    "1-alpha-20251107-1/linuxdeploy-x86_64.AppImage",
    "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
)
GTK_PLUGIN = (
    "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/"
    "7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh",
    "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171",
)
RUNTIME = (
    "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64",
    "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
)
# Keep the host graphics stack, including its driver/loader pairing.
EXCLUDED = (
    "libvulkan.so*", "libvulkan_*.so*", "libGL.so*", "libGLX*.so*",
    "libEGL*.so*", "libGLdispatch.so*", "libGLES*.so*", "libgbm.so*",
    "libdrm*.so*", "*_dri.so",
)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def download(cache, name, source):
    url, checksum = source
    destination = cache / name
    if destination.is_file() and digest(destination) == checksum:
        return destination
    with tempfile.TemporaryDirectory(prefix="download-", dir=cache) as temp:
        candidate = Path(temp) / name
        with urllib.request.urlopen(url, timeout=120) as response, candidate.open("wb") as out:
            shutil.copyfileobj(response, out)
        if digest(candidate) != checksum:
            raise RuntimeError(f"Checksum mismatch: {url}")
        candidate.chmod(0o755)
        candidate.replace(destination)
    return destination


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def remove_plugin_overrides(appdir):
    # The GTK plugin invokes linuxdeploy recursively without forwarding our
    # exclusions. Remove any host graphics libraries it brought back in.
    for path in (appdir / "usr/lib").rglob("*"):
        if (path.is_file() or path.is_symlink()) and any(
                fnmatch.fnmatchcase(path.name, pattern) for pattern in EXCLUDED):
            path.unlink()
    # Even custom launchers get wrapped if this generated hook is left behind.
    (appdir / "apprun-hooks/linuxdeploy-plugin-gtk.sh").unlink()
    (appdir / "AppRun").unlink()


def glibc_requirement(appdir):
    versions = set()
    for path in (appdir / "usr").rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        with path.open("rb") as source:
            if source.read(4) != b"\x7fELF":
                continue
        output = run("readelf", "--version-info", str(path), capture_output=True, text=True).stdout
        versions.update(tuple(map(int, item.split(".")))
                        for item in re.findall(r"GLIBC_([0-9]+\.[0-9]+(?:\.[0-9]+)?)", output))
    return ".".join(map(str, max(versions))) if versions else "unknown"


def package(root):
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("Requires a native Linux x86_64 build host")
    if os.environ.get("CARGO_TARGET_DIR") or os.environ.get("CARGO_BUILD_TARGET"):
        raise RuntimeError("Unset CARGO_TARGET_DIR/CARGO_BUILD_TARGET: packaging uses target/release/ruzu")
    for name in ("file", "readelf", "strip", "pkg-config", "glib-compile-schemas"):
        if not shutil.which(name):
            raise RuntimeError(f"Missing packaging tool: {name}")
    run("pkg-config", "--exists", "gtk4", "librsvg-2.0", "gobject-introspection-1.0")
    binary = root / "target/release/ruzu"
    if not binary.is_file():
        raise RuntimeError("Build target/release/ruzu first (./build.sh appimage)")
    header = run("readelf", "-h", str(binary), capture_output=True, text=True,
                 env=dict(os.environ, LC_ALL="C")).stdout
    if "Advanced Micro Devices X86-64" not in header:
        raise RuntimeError("target/release/ruzu is not an x86_64 ELF binary")
    revision = run("sh", str(root / "scripts/package-revision.sh"), str(root),
                   capture_output=True, text=True).stdout.strip()
    cache = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "ruzu-packaging"
    cache.mkdir(parents=True, exist_ok=True)
    deploy_image = download(cache, "linuxdeploy-x86_64.AppImage", LINUXDEPLOY)
    download(cache, "linuxdeploy-plugin-gtk.sh", GTK_PLUGIN)
    runtime = download(cache, "runtime-x86_64", RUNTIME)
    output_name = f"Ruzu-Linux-{revision}-x86_64.AppImage"
    with tempfile.TemporaryDirectory(prefix="appimage-", dir=binary.parent) as temp:
        work = Path(temp)
        run(str(deploy_image), "--appimage-extract", cwd=work, stdout=subprocess.DEVNULL)
        deploy = work / "squashfs-root/AppRun"
        appdir = work / "AppDir"
        (appdir / "usr/bin").mkdir(parents=True)
        staged_binary = appdir / "usr/bin/ruzu"
        shutil.copy2(binary, staged_binary)
        run("strip", "--strip-unneeded", str(staged_binary))
        env = dict(os.environ, DEPLOY_GTK_VERSION="4", ARCH="x86_64",
                   LDAI_RUNTIME_FILE=str(runtime),
                   PATH=f"{cache}:{os.environ.get('PATH', '')}", OUTPUT=str(work / output_name))
        excludes = [f"--exclude-library={pattern}" for pattern in EXCLUDED]
        run(str(deploy), "--appdir", str(appdir), "--executable", str(staged_binary),
            "--desktop-file", str(root / "dist/linux/ruzu.desktop"),
            "--icon-file", str(root / "src/ruzu/assets/ruzu-rusty-lemon.png"),
            "--icon-filename", "ruzu", "--plugin", "gtk", *excludes, env=env)
        remove_plugin_overrides(appdir)
        # Our launcher sets resource paths without overriding user settings.
        launcher = work / "AppRun"
        shutil.copy2(root / "dist/linux/AppRun", launcher)
        launcher.chmod(0o755)
        minimum = glibc_requirement(appdir)
        info = appdir / "usr/share/doc/ruzu"
        info.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / "LICENSE", info / "LICENSE")
        (info / "build-info.txt").write_text(
            f"Revision: {revision}\nArchitecture: x86_64\nRequired GLIBC: {minimum}\n"
            "Graphics drivers: supplied by host. Target Linux distribution validation required.\n")
        run(str(deploy), "--appdir", str(appdir), "--custom-apprun", str(launcher),
            "--output", "appimage", *excludes, env=env, cwd=work)
        current_revision = run("sh", str(root / "scripts/package-revision.sh"), str(root),
                               capture_output=True, text=True).stdout.strip()
        if current_revision != revision:
            raise RuntimeError("Git revision changed during packaging; retry on a stable checkout")
        artifact = work / output_name
        if not artifact.is_file():
            raise RuntimeError("AppImage output was not created")
        artifact.chmod(0o755)
        checksum = digest(artifact)
        artifact.replace(binary.parent / output_name)
    print(f"Created {binary.parent / output_name}\nSHA256: {checksum}\n"
          f"Requires GLIBC >= {minimum} and compatible host graphics drivers.\n"
          "Test on the target Linux distributions before distribution; packaging is not a compatibility guarantee.")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    try:
        package(Path(__file__).resolve().parent.parent)
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"AppImage packaging failed: {error}") from error
