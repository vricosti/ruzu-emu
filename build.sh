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

The package command builds a release and creates, on macOS,
target/release/Ruzu-macOS-<Git revision>-<arch>-clang.zip containing
Ruzu-macOS-<Git revision>-<arch>-clang/ruzu.app, and on Linux the Debian
archive target/release/Ruzu-<Distro><Version>-<Git revision>-<arch>.deb.
Git revision is an exact version tag on a clean checkout, otherwise
<branch>-<12-character commit>[-dirty]. Detached builds use branch "detached".
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
