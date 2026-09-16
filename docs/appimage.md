# Linux AppImage / Steam Deck

On a **glibc-based Linux x86_64** build machine:

```sh
./build.sh appimage
# Or skip installing/checking build dependencies when already installed:
./build.sh appimage --skip-deps
```

This builds the release GUI and creates
`target/release/Ruzu-<Git revision>-x86_64.AppImage`. The revision follows the
existing packaging convention: exact version tag at HEAD, otherwise branch and
commit, with `-dirty` for local changes. No commit, tag or push is performed.
The Debian command remains `./build.sh package`.

To package an already built `target/release/ruzu` without rebuilding:

```sh
python3 scripts/package-appimage.py
```

Ubuntu needs the normal Ruzu build dependencies plus `python3`, `file`,
`binutils`, `librsvg2-dev` and `libgirepository1.0-dev`. The build command includes
these in its dependency check. Building needs network access to download pinned,
SHA-256-verified linuxdeploy, GTK plugin and AppImage runtime into
`${XDG_CACHE_HOME:-~/.cache}/ruzu-packaging`; subsequent builds reuse this cache.
FUSE is not needed on the build machine. The original binary is not stripped or
modified; packaging strips a temporary copy and publishes only after success.

## Steam Deck

Copy the result to the Deck in Desktop Mode, make it executable and run it:

```sh
chmod +x Ruzu-*.AppImage
./Ruzu-<Git revision>-x86_64.AppImage
```

You can then add that file as a non-Steam game. If FUSE is unavailable, use
`./Ruzu-<Git revision>-x86_64.AppImage --appimage-extract-and-run`.

Ruzu's embedded game window currently requires X11/XWayland. SteamOS Gamescope
supports XWayland; switching the entire desktop session to X11 is not necessary
([Valve's Gamescope documentation](https://github.com/ValveSoftware/gamescope)).
If Ruzu shows its Wayland warning on first launch, select **Use X11**, then restart
Ruzu. The saved preference also applies when subsequently launching from Steam.
An explicit external `GDK_BACKEND` setting takes precedence; do not impose
`GDK_BACKEND=wayland` in Steam launch options. Hiding the warning alone does not
enable native Wayland game rendering.

GTK resources and application libraries are bundled. Graphics drivers and their
Vulkan/OpenGL loaders remain supplied by SteamOS. The launcher does not force
`GDK_BACKEND`, `GTK_THEME`, or replace user configuration/data directories.

**An AppImage does not remove the build machine's glibc requirement.** The script
prints the highest GLIBC version required by bundled ELF files and includes it in
`usr/share/doc/ruzu/build-info.txt` inside the image. Build on a distribution with
a glibc no newer than the target's (`ldd --version`), and do not use CPU-specific
flags such as `-C target-cpu=native` or `-march=native`. Cross-compilation, custom
Cargo target directories and musl builds are not supported by this command.

Before publishing, test the actual image on the intended SteamOS version:
GUI startup, theme, controller/audio, Vulkan rendering, Stop, game restart and
application closure. Successful packaging alone does not certify Steam Deck
compatibility. Check the redistribution licenses of bundled dependencies before
publishing a release.
