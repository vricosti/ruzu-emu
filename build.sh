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
       ./build.sh package [--skip-deps]

The package command builds a macOS release and creates
target/release/Ruzu-macOS-v<Cargo version>.zip containing
Ruzu-macOS-v<Cargo version>/ruzu.app.
Packaging requires a clean checkout and an exact Git tag matching v<Cargo version>.

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
    if [ "$(uname -s)" != Darwin ]; then
        echo "The package command currently supports macOS only." >&2
        exit 1
    fi
    for arg in "$@"; do
        case "$arg" in
            --skip-deps|--release) ;;
            -h|--help) usage; exit 0 ;;
            *) echo "Unsupported package option: $arg (use --skip-deps)." >&2; exit 1 ;;
        esac
    done
    sh "$SCRIPT_DIR/scripts/check-release.sh" "$SCRIPT_DIR" >/dev/null
    RUZU_MACOS_PACKAGE=1
    export RUZU_MACOS_PACKAGE
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
