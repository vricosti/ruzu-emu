#!/bin/sh
# Build ruzu: check the platform dependencies, then compile the workspace.
#
# Dispatches to the per-OS script, which installs anything missing and then
# runs the build. Release is the default; pass --debug for a debug build.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

usage() {
    cat <<'EOF'
Usage: ./build.sh [options] [-- <extra cargo arguments>]
       ./build.sh package [--skip-deps] [--official]
       ./build.sh appimage [--skip-deps]
       ./build.sh steamdeck [--jobs N] [--dry-run]

The steamdeck command builds a dedicated Zen 2 AppImage in Docker, with
bundled runtime libraries and X11 forced like Eden's AppImage.
Output: target/steamdeck/artifacts/. See docs/steamdeck-appimage.md.

The appimage command builds a Linux x86_64 release and packages it as
target/release/Ruzu-Linux-<Git revision>-x86_64.AppImage (no commit, tag or push).
The host glibc and graphics drivers must be compatible; test on target Linux distributions
before distributing the package.

The package command builds a release and creates, on macOS,
target/release/Ruzu-macOS-<Git revision>-<arch>-clang.zip containing
Ruzu-macOS-<Git revision>-<arch>-clang/ruzu.app, and on Linux the Debian
archive target/release/Ruzu-<Distro><Version>-<Git revision>-<arch>.deb.
Git revision is an exact local version tag at HEAD (even on a branch), otherwise
<branch>-<12-character commit>. Both forms append -dirty for local changes.
Detached builds without a version tag use branch "detached".
By default package builds the current sources locally, without commits, tags or pushes.
Only --official prompts for a release version, commits it, builds, creates
an annotated tag, rebuilds/packages and atomically pushes the branch and tag.
Python 3.9+ and a clean attached branch are required for this release workflow.
--development remains accepted as an alias for the default local mode.

Options:
  --debug        Build the debug profile instead of release.
  --deps-only    Only check and install dependencies; do not build.
  --skip-deps    Skip the dependency check and build straight away.
  -h, --help     Show this help.

Everything after `--` is forwarded to `cargo build`, so a single crate can be
built with, for example:

  ./build.sh -- --bin ruzu-cmd
EOF
}

case "${1-}" in
    -h|--help)
        usage
        exit 0
        ;;
esac

if [ "${1-}" = steamdeck ]; then
    shift
    exec python3 "$SCRIPT_DIR/scripts/package-steamdeck.py" "$@"
fi

if [ "${1-}" = appimage ]; then
    shift
    appimage_skip_deps=
    for arg in "$@"; do
        case "$arg" in
            --skip-deps) appimage_skip_deps=--skip-deps ;;
            -h|--help) usage; exit 0 ;;
            *) echo "Unsupported appimage option: $arg" >&2; exit 1 ;;
        esac
    done
    if [ "$(uname -s)/$(uname -m)" != Linux/x86_64 ]; then
        echo "AppImage packaging requires Linux x86_64 (no cross-compilation)." >&2
        exit 1
    fi
    if [ -n "${CARGO_TARGET_DIR-}${CARGO_BUILD_TARGET-}" ]; then
        echo "AppImage packaging uses native target/release; unset CARGO_TARGET_DIR and CARGO_BUILD_TARGET." >&2
        exit 1
    fi
    export RUZU_LINUX_APPIMAGE=1
    set -- --release ${appimage_skip_deps:+"$appimage_skip_deps"} -- --bin ruzu
fi

if [ "${1-}" = package ]; then
    shift
    case "$(uname -s)" in
        Darwin) package_platform=macos ;;
        Linux) package_platform=linux ;;
        *) echo "The package command supports macOS and Linux only." >&2; exit 1 ;;
    esac
    package_development=0
    package_official=0
    package_skip_deps=
    for arg in "$@"; do
        case "$arg" in
            --skip-deps) package_skip_deps=--skip-deps ;;
            --release) ;;
            --development) package_development=1 ;;
            --official) package_official=1 ;;
            -h|--help) usage; exit 0 ;;
            *) echo "Unsupported package option: $arg (use --skip-deps)." >&2; exit 1 ;;
        esac
    done
    if [ "$package_official" = 1 ] && [ "$package_development" = 1 ]; then
        echo "--official and --development cannot be combined." >&2
        exit 1
    fi
    if [ "$package_official" = 1 ]; then
        exec python3 "$SCRIPT_DIR/scripts/release-package.py" --platform "$package_platform" ${package_skip_deps:+"$package_skip_deps"}
    fi
    set -- --release ${package_skip_deps:+"$package_skip_deps"}
    sh "$SCRIPT_DIR/scripts/package-revision.sh" "$SCRIPT_DIR" >/dev/null
    if [ "$package_platform" = macos ]; then
        RUZU_MACOS_PACKAGE=1
        export RUZU_MACOS_PACKAGE
    else
        RUZU_LINUX_PACKAGE=1
        export RUZU_LINUX_PACKAGE
    fi
fi

case "$(uname -s)" in
    Linux)
        PLATFORM_BUILD="${SCRIPT_DIR}/scripts/build-linux.sh"
        ;;
    FreeBSD|NetBSD|OpenBSD)
        PLATFORM_BUILD="${SCRIPT_DIR}/scripts/build-bsd.sh"
        ;;
    Darwin)
        PLATFORM_BUILD="${SCRIPT_DIR}/scripts/build-macos.sh"
        ;;
    *)
        echo "Unsupported operating system: $(uname -s)." >&2
        exit 1
        ;;
esac

if [ ! -x "$PLATFORM_BUILD" ]; then
    echo "Platform build script is missing or not executable: $PLATFORM_BUILD" >&2
    exit 1
fi

exec "$PLATFORM_BUILD" "$@"
