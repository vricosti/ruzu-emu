#!/bin/sh
# Install the tools and native libraries ruzu needs on Linux, then build it.
set -eu

PLATFORM_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=build-common.sh
. "${PLATFORM_SCRIPT_DIR}/build-common.sh"

if [ ! -r /etc/os-release ]; then
    echo "Unable to determine the Linux distribution." >&2
    exit 1
fi

# shellcheck disable=SC1091
. /etc/os-release
PLATFORM_NAME=${PRETTY_NAME:-${ID:-Linux}}

case "${ID:-}" in
    ubuntu|debian)
        PACKAGE_MANAGER=apt
        REQUIRED_PACKAGES="
            build-essential ca-certificates clang cmake curl git
            glslang-tools libasound2-dev libavcodec-dev libavutil-dev
            libclang-dev libdbus-1-dev libdecor-0-dev libdrm-dev
            libegl1-mesa-dev libgbm-dev libgl1-mesa-dev libgtk-4-dev
            libjack-jackd2-dev libopus-dev libpipewire-0.3-dev libpulse-dev
            libssl-dev libudev-dev libvulkan-dev libwayland-dev libx11-dev
            libxcursor-dev libxext-dev libxfixes-dev libxi-dev
            libxkbcommon-dev libxrandr-dev libxss-dev libxtst-dev
            ninja-build pkg-config vulkan-tools wayland-protocols
        "
        ;;
    fedora)
        PACKAGE_MANAGER=dnf
        REQUIRED_PACKAGES="
            alsa-lib-devel ca-certificates clang clang-devel cmake
            curl dbus-devel ffmpeg-free-devel gcc gcc-c++ git
            glslang gtk4-devel jack-audio-connection-kit-devel
            libX11-devel libXcursor-devel libXext-devel libXfixes-devel
            libXi-devel libXrandr-devel libXScrnSaver-devel libXtst-devel
            libdecor-devel libdrm-devel libxkbcommon-devel
            make mesa-libEGL-devel mesa-libGL-devel mesa-libgbm-devel
            ninja-build openssl-devel opus-devel pipewire-devel
            pkgconf-pkg-config
            pulseaudio-libs-devel systemd-devel vulkan-headers
            vulkan-loader-devel vulkan-tools wayland-devel
            wayland-protocols-devel
        "
        ;;
    arch|manjaro|endeavouros)
        PACKAGE_MANAGER=pacman
        REQUIRED_PACKAGES="
            base-devel alsa-lib ca-certificates clang cmake curl
            dbus ffmpeg git glslang gtk4 jack2 libdecor libdrm libpulse
            libx11 libxcursor libxext libxfixes libxi libxkbcommon
            libxrandr libxss libxtst mesa ninja openssl opus pipewire pkgconf
            systemd-libs vulkan-headers vulkan-icd-loader vulkan-tools
            wayland wayland-protocols
        "
        ;;
    opensuse-tumbleweed|opensuse-leap)
        PACKAGE_MANAGER=zypper
        REQUIRED_PACKAGES="
            alsa-devel ca-certificates clang clang-devel cmake curl
            dbus-1-devel ffmpeg-8-libavcodec-devel ffmpeg-8-libavutil-devel
            gcc gcc-c++ git glslang-devel gtk4-devel
            libX11-devel libXcursor-devel libXext-devel libXfixes-devel
            libXi-devel libXrandr-devel libXss-devel libXtst-devel
            libdecor-devel libdrm-devel libjack-devel libopenssl-devel
            libopus-devel libpulse-devel libxkbcommon-devel make
            Mesa-libEGL-devel Mesa-libGL-devel ninja pipewire-devel
            pkgconf-pkg-config systemd-devel vulkan-devel vulkan-tools
            wayland-devel wayland-protocols-devel
        "
        ;;
    alpine)
        PACKAGE_MANAGER=apk
        REQUIRED_PACKAGES="
            alsa-lib-dev build-base ca-certificates clang clang-dev
            cmake curl dbus-dev eudev-dev ffmpeg-dev git glslang-dev
            gtk4.0-dev jack-dev libdecor-dev libdrm-dev libx11-dev
            libxcursor-dev libxext-dev libxfixes-dev libxi-dev
            libxkbcommon-dev libxrandr-dev libxscrnsaver-dev libxtst-dev
            mesa-dev
            ninja openssl-dev opus-dev pipewire-dev pkgconf pulseaudio-dev
            vulkan-headers vulkan-loader-dev vulkan-tools wayland-dev
            wayland-protocols
        "
        ;;
    *)
        echo "Unsupported Linux distribution: $PLATFORM_NAME." >&2
        exit 1
        ;;
esac

package_installed() {
    package_name=$1
    case "$PACKAGE_MANAGER" in
        apt)
            [ "$(dpkg-query -W -f='${db:Status-Abbrev}' "$package_name" 2>/dev/null || true)" = "ii " ]
            ;;
        dnf|zypper)
            rpm -q "$package_name" >/dev/null 2>&1
            ;;
        pacman)
            pacman -Q "$package_name" >/dev/null 2>&1
            ;;
        apk)
            apk info -e "$package_name" >/dev/null 2>&1
            ;;
    esac
}

install_packages() {
    # Word splitting is intentional: package names cannot contain whitespace.
    # shellcheck disable=SC2086
    case "$PACKAGE_MANAGER" in
        apt)
            run_privileged env DEBIAN_FRONTEND=noninteractive apt-get update
            run_privileged env DEBIAN_FRONTEND=noninteractive \
                apt-get install -y --no-install-recommends $MISSING_PACKAGES
            ;;
        dnf)
            run_privileged dnf install -y $MISSING_PACKAGES
            ;;
        pacman)
            run_privileged pacman -Syu --needed --noconfirm $MISSING_PACKAGES
            ;;
        zypper)
            run_privileged zypper --non-interactive refresh
            run_privileged zypper --non-interactive install --no-recommends $MISSING_PACKAGES
            ;;
        apk)
            run_privileged apk add --no-cache $MISSING_PACKAGES
            ;;
    esac
}

post_build_platform() {
    if [ "${RUZU_LINUX_PACKAGE:-0}" != 1 ]; then
        return 0
    fi
    if [ ! -x "${PLATFORM_SCRIPT_DIR}/../target/release/ruzu" ]; then
        echo
        echo "Skipping the Debian package: target/release/ruzu was not built."
        return 0
    fi
    echo
    echo "Packaging the Debian archive..."
    sh "${PLATFORM_SCRIPT_DIR}/package-linux.sh"
}

run_pipeline "$@"
