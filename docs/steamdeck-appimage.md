# Dedicated Steam Deck AppImage

```sh
./build.sh steamdeck --jobs 2
```

Requires Python 3, a running Docker daemon and a Linux x86_64 build machine
supporting the Zen 2 instruction set (the optimized executable is traced during
packaging). Reserve at least 35 GiB on the checkout filesystem and additional
space for Docker images. No packages are installed on the host; no privileged
container, FUSE mount, host GPU, display, controller, configuration or save-data
mount is required. Docker must be configured separately by the user.

`./build.sh steamdeck --dry-run` prints the container commands without building.
The current working tree is mounted read-only, including local changes. Initialize
submodules first. Do not edit sources during a build. The command neither changes
branches nor commits, tags or pushes anything.

## Artifacts and build policy

- Output: `target/steamdeck/artifacts/Ruzu-SteamDeck-<revision>-x86_64.AppImage`
  and its `.sha256` checksum.
- Independent Cargo cache/build directory under `target/steamdeck`; the usual
  `target/release/ruzu` is not overwritten. Container output belongs to your UID.
- Rust 1.92.0 container pinned by image digest, Debian native packages from a
  fixed snapshot, Cargo dependencies from `Cargo.lock`.
- Rust uses `-C target-cpu=znver2`; C/C++ use `-march=znver2 -mtune=znver2`.
  Release debug information is omitted; panic unwinding is unchanged.
- `quick-sharun` is pinned by commit and SHA256. That version pins and verifies
  its own sharun, appimagetool and helper downloads. It packages GTK4, glibc and
  the OpenGL/Vulkan user-space stack, rather than depending on Ubuntu's glibc.
- Unlike Eden, optional `OPTIMIZE_LAUNCH` profiling is disabled: it mounts and
  executes the image using FUSE during packaging. Building therefore does not
  require `/dev/fuse` or `SYS_ADMIN`. This does not disable Zen 2 compilation
  or change rendering; only startup file-layout profiling is skipped.
- Unlike Eden's rolling Arch container, this uses a fixed Debian/Rust base.
  Dependency pins improve repeatability, but bit-for-bit reproducibility is not
  claimed. Review security updates when deliberately refreshing the pins.

## X11 policy

Like Eden's AppImage, this package includes `wayland-is-broken.hook`. It forces
`SDL_VIDEO_DRIVER=x11`, `QT_QPA_PLATFORM=xcb`, `GDK_BACKEND=x11`,
`XDG_SESSION_TYPE=x11`, and unsets `WAYLAND_DISPLAY`. Thus this dedicated package
overrides Ruzu's Wayland selection. On a Wayland desktop it requires XWayland.

The upstream escape hatch remains available for diagnosis:

```sh
I_WANT_A_BROKEN_WAYLAND_UI=1 ./Ruzu-SteamDeck-<revision>-x86_64.AppImage
```

This bypasses the packaging hook, not Ruzu's own saved Force X11 preference.
The existing generic `./build.sh appimage` command and launcher are unchanged.

## Validation before distribution

The scripts' offline regressions run with:

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*appimage.py'
python3 -m unittest discover -s scripts/tests -p 'test_steamdeck.py'
```

A completed container build still needs testing on an actual Steam Deck in
Desktop and Gaming modes: first launch, controller navigation, file chooser,
Vulkan rendering, audio, Stop/restart, closure, and persistence of settings.
No Steam Deck compatibility or performance gain is claimed before these tests.

References: [Eden packaging](https://github.com/Eden-CI/Workflow/blob/master/.ci/package/linux.sh),
[CPU targets](https://github.com/Eden-CI/Workflow/blob/master/.ci/build/targets.sh),
[pinned quick-sharun](https://github.com/pkgforge-dev/Anylinux-AppImages/blob/45964bcbcb5456a6c902c55185635c06700d3284/useful-tools/quick-sharun.sh).
