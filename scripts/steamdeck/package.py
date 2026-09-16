#!/usr/bin/env python3
"""Container-only sharun packaging, following Eden-CI/.ci/package/linux.sh."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def package(revision, source=Path("/source"), build_output=Path("/output")):
    if not re.fullmatch(r"[A-Za-z0-9._-]+", revision):
        raise ValueError("Invalid artifact revision")
    output = build_output / "artifacts"
    output.mkdir(parents=True, exist_ok=True)
    name = f"Ruzu-SteamDeck-{revision}-x86_64.AppImage"
    with tempfile.TemporaryDirectory(prefix="package-", dir=build_output) as temp:
        work = Path(temp)
        appdir = work / "AppDir"
        icon = work / "ruzu.png"
        shutil.copy2(source / "src/ruzu/assets/ruzu-rusty-lemon.png", icon)
        binary = work / "ruzu"
        shutil.copy2(build_output / "build/release/ruzu", binary)
        subprocess.run(["strip", "--strip-unneeded", str(binary)], check=True)
        env = dict(os.environ, APPDIR=str(appdir), ICON=str(icon),
                   DESKTOP=str(source / "dist/linux/ruzu.desktop"), MAIN_BIN="ruzu",
                   OUTPATH=str(work), OUTNAME=name, VERSION=revision,
                   DEPLOY_GTK="1", DEPLOY_GLIBC="1", DEPLOY_OPENGL="1", DEPLOY_VULKAN="1",
                   # Startup profiling mounts/runs the finished image via FUSE.
                   # Keep packaging unprivileged; only skip this optional pass.
                   ADD_HOOKS="wayland-is-broken.hook", OPTIMIZE_LAUNCH="0",
                   XDG_CONFIG_HOME=str(work / "config"), XDG_DATA_HOME=str(work / "data"),
                   XDG_CACHE_HOME=str(work / "cache"))
        # Trace in a private display; no host GPU, controller, config or save mounts.
        subprocess.run(["xvfb-run", "-a", "/opt/quick-sharun", str(binary)],
                       cwd=work, env=env, check=True)
        if not list((appdir / "lib").rglob("libc.so*")):
            raise RuntimeError("Missing bundled glibc")
        hook = appdir / "bin/05-wayland-is-broken.hook"
        if "export GDK_BACKEND=x11" not in hook.read_text():
            raise RuntimeError("Missing upstream X11 launch policy")
        info = appdir / "share/doc/ruzu"
        info.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / "LICENSE", info / "LICENSE")
        shutil.copy2(source / "scripts/steamdeck/Dockerfile", info / "Dockerfile")
        (info / "build-info.txt").write_text(
            f"Revision: {revision}\nCPU: znver2\nRust: 1.92.0\n"
            "glibc, GTK and Mesa: bundled by pinned quick-sharun\n"
            "X11: forced unless I_WANT_A_BROKEN_WAYLAND_UI=1\n"
            "Steam Deck Desktop/Gaming hardware validation required.\n")
        subprocess.run(["/opt/quick-sharun", "--make-appimage"],
                       cwd=work, env=env, check=True)
        artifact = work / name
        if not artifact.is_file() or artifact.stat().st_size == 0:
            raise RuntimeError("No AppImage produced")
        with artifact.open("rb") as stream:
            checksum = hashlib.file_digest(stream, "sha256").hexdigest()
        artifact.chmod(0o755)
        artifact.replace(output / name)
        (output / f"{name}.sha256").write_text(f"{checksum}  {name}\n")


if __name__ == "__main__":
    package(sys.argv[1])
