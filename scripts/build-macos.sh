#!/bin/sh
# Install the tools and native libraries ruzu needs on macOS, then build it.
set -eu

PLATFORM_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=build-common.sh
. "${PLATFORM_SCRIPT_DIR}/build-common.sh"

if [ "$(uname -s)" != Darwin ]; then
    echo "This setup script only supports macOS." >&2
    exit 1
fi

PLATFORM_NAME="$(sw_vers -productName) $(sw_vers -productVersion)"
PACKAGE_MANAGER=brew
REQUIRED_PACKAGES="
    cmake ffmpeg glslang gtk4 molten-vk ninja openssl@3 opus
    pkgconf vulkan-headers vulkan-loader vulkan-tools
"

load_homebrew() {
    if command -v brew >/dev/null 2>&1; then
        return 0
    fi
    if [ -x /opt/homebrew/bin/brew ]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
    elif [ -x /usr/local/bin/brew ]; then
        eval "$(/usr/local/bin/brew shellenv)"
    fi
}

ensure_homebrew() {
    load_homebrew
    if command -v brew >/dev/null 2>&1; then
        return 0
    fi

    echo "[MISSING] Homebrew is not installed."
    if ! confirm_install "Install Homebrew?"; then
        echo "Homebrew installation declined."
        return 1
    fi

    NONINTERACTIVE=1 /bin/bash -c \
        "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    load_homebrew
    command -v brew >/dev/null 2>&1
}

prepare_platform() {
    if ! ensure_homebrew; then
        echo "Setup is incomplete because Homebrew is required on macOS." >&2
        exit 1
    fi

    # Keep Rust, cc-rs dependencies, and Homebrew libraries on one deployment
    # target. Rust otherwise defaults x86_64 macOS links to 10.12 while current
    # Apple SDKs compile native dependencies for a much newer macOS release,
    # which leaves modern libc/dispatch availability symbols unresolved.
    if [ -z "${MACOSX_DEPLOYMENT_TARGET:-}" ]; then
        MACOSX_DEPLOYMENT_TARGET=$(sw_vers -productVersion | awk -F. '{ print $1 "." $2 }')
        export MACOSX_DEPLOYMENT_TARGET
        echo "Using macOS deployment target ${MACOSX_DEPLOYMENT_TARGET}."
    fi

    # Homebrew's pkg-config must win over any other one in PATH. devkitPro
    # ships its own at /opt/devkitpro/tools/bin/pkg-config which cannot see
    # Homebrew's .pc files, so whenever it comes first both the GTK probe below
    # and the Cargo build scripts report installed libraries as missing.
    brew_bin="$(brew --prefix)/bin"
    case ":${PATH}:" in
        "${brew_bin}:"*) ;;
        *)
            if [ "$(command -v pkg-config || true)" != "${brew_bin}/pkg-config" ] &&
                [ -x "${brew_bin}/pkg-config" ]; then
                echo "[WARN] $(command -v pkg-config) shadows ${brew_bin}/pkg-config."
                echo "       Prepending ${brew_bin} to PATH for this run; add it to"
                echo "       your shell profile before building."
            fi
            PATH="${brew_bin}:${PATH}"
            export PATH
            ;;
    esac
    if ! xcrun --find clang >/dev/null 2>&1; then
        cat >&2 <<'EOF'
Apple Command Line Tools are missing. Run `xcode-select --install`, finish the
installer, then rerun this script. Homebrew and the native compiler require
these tools.
EOF
        exit 1
    fi

    # Rust invokes the Apple linker with -nodefaultlibs, so Clang's Darwin
    # runtime is not added automatically. SDL3 uses Clang's platform
    # availability builtin, whose implementation lives in this archive.
    clang_runtime=$(xcrun clang -print-file-name=libclang_rt.osx.a)
    if [ ! -f "$clang_runtime" ]; then
        echo "Apple Clang runtime was not found: $clang_runtime" >&2
        exit 1
    fi
    case " ${RUSTFLAGS:-} " in
        *"link-arg=${clang_runtime}"*) ;;
        *) RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }-C link-arg=${clang_runtime}" ;;
    esac
    export RUSTFLAGS
}

package_installed() {
    brew list --versions "$1" >/dev/null 2>&1
}

install_packages() {
    # Word splitting is intentional: package names cannot contain whitespace.
    # shellcheck disable=SC2086
    brew install $MISSING_PACKAGES
}

# macOS ships an app bundle, not a bare binary: a .app is what carries the
# Info.plist, the icon and the bundled MoltenVK, and it is the only form that
# launches from Finder. Skipped when the build did not produce the `ruzu`
# binary, which happens when the caller targeted another crate.
post_build_platform() {
    if [ "$BUILD_PROFILE" != release ]; then
        echo
        echo "Skipping the .app bundle: it is only built for the release profile."
        return 0
    fi
    if [ ! -x "${PLATFORM_SCRIPT_DIR}/../target/release/ruzu" ]; then
        echo
        echo "Skipping the .app bundle: target/release/ruzu was not built."
        return 0
    fi
    echo
    echo "Packaging ruzu.app..."
    if [ "${RUZU_MACOS_PACKAGE:-0}" = 1 ]; then
        "${PLATFORM_SCRIPT_DIR}/build-macos-app.sh" --no-build --package
    else
        "${PLATFORM_SCRIPT_DIR}/build-macos-app.sh" --no-build
    fi
}

run_pipeline "$@"
