#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This script only supports macOS." >&2
    exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_dir="$repo_root/target/release"
binary="$build_dir/ruzu"
app="$build_dir/ruzu.app"
plist="$repo_root/src/ruzu/Info.plist"
icon_source="$repo_root/src/ruzu/assets/ruzu-rusty-lemon.png"

if command -v brew >/dev/null 2>&1; then
    brew_prefix="$(brew --prefix)"
    export PATH="$brew_prefix/bin:$PATH"

    pkg_config_paths=(
        "$brew_prefix/lib/pkgconfig"
        "$brew_prefix/share/pkgconfig"
    )
    for formula in opus ffmpeg openssl@3; do
        formula_prefix="$(brew --prefix "$formula" 2>/dev/null || true)"
        if [[ -n "$formula_prefix" ]]; then
            pkg_config_paths+=("$formula_prefix/lib/pkgconfig")
        fi
    done
    joined_pkg_config_path="$(IFS=:; echo "${pkg_config_paths[*]}")"
    export PKG_CONFIG_PATH="$joined_pkg_config_path${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi

# The build pipeline has already compiled the workspace; standalone runs still
# need cargo, so this is opt-out rather than removed.
skip_build=false
for arg in "$@"; do
    case "$arg" in
        --no-build) skip_build=true ;;
    esac
done

cd "$repo_root"
if [[ "$skip_build" != true ]]; then
    cargo build --locked --release --bin ruzu
fi

if [[ ! -x "$binary" ]]; then
    echo "Cargo did not produce $binary." >&2
    exit 1
fi

staging_root="$(mktemp -d "$build_dir/.ruzu-app.XXXXXX")"
staging="$staging_root/ruzu.app"
trap 'rm -rf "$staging_root"' EXIT

contents="$staging/Contents"
macos="$contents/MacOS"
frameworks="$contents/Frameworks"
resources="$contents/Resources"
iconset="$staging/ruzu.iconset"

mkdir -p "$macos" "$frameworks" "$resources" "$iconset"
install -m 755 "$binary" "$macos/ruzu"
install -m 644 "$plist" "$contents/Info.plist"

if [[ -n "${RUZU_FREE_GAMES_ROOT:-}" ]]; then
    freebrick_source="$RUZU_FREE_GAMES_ROOT/freebrick"
    freebrick_resources="$resources/freegames/freebrick"
    required_freebrick_files=(
        "$freebrick_source/switch/freebrick.nro"
        "$freebrick_source/LICENSE"
        "$freebrick_source/ASSET_LICENSES.md"
        "$freebrick_source/README.md"
    )
    for required_file in "${required_freebrick_files[@]}"; do
        if [[ ! -f "$required_file" ]]; then
            echo "Required FreeBrick package file is missing: $required_file" >&2
            exit 1
        fi
    done
    mkdir -p "$freebrick_resources"
    install -m 644 "$freebrick_source/switch/freebrick.nro" "$freebrick_resources/freebrick.nro"
    install -m 644 "$freebrick_source/LICENSE" "$freebrick_resources/LICENSE.txt"
    install -m 644 "$freebrick_source/ASSET_LICENSES.md" "$freebrick_resources/ASSET_LICENSES.md"
    install -m 644 "$freebrick_source/README.md" "$freebrick_resources/README.md"
fi

make_icon() {
    sips -z "$1" "$1" "$icon_source" --out "$iconset/$2" >/dev/null
}

make_icon 16 icon_16x16.png
make_icon 32 icon_16x16@2x.png
make_icon 32 icon_32x32.png
make_icon 64 icon_32x32@2x.png
make_icon 128 icon_128x128.png
make_icon 256 icon_128x128@2x.png
make_icon 256 icon_256x256.png
make_icon 512 icon_256x256@2x.png
make_icon 512 icon_512x512.png
make_icon 1024 icon_512x512@2x.png
iconutil -c icns "$iconset" -o "$resources/ruzu.icns"
rm -rf "$iconset"

moltenvk="${MOLTENVK_LIBRARY:-}"
if [[ -z "$moltenvk" ]]; then
    eden_moltenvk="$repo_root/../eden/build/bin/eden.app/Contents/Frameworks/libMoltenVK.dylib"
    if [[ -f "$eden_moltenvk" ]]; then
        moltenvk="$eden_moltenvk"
    fi
fi
if [[ -z "$moltenvk" ]] && command -v brew >/dev/null 2>&1; then
    brew_moltenvk="$(brew --prefix molten-vk 2>/dev/null || true)"
    if [[ -n "$brew_moltenvk" ]]; then
        moltenvk="$brew_moltenvk/lib/libMoltenVK.dylib"
    fi
fi

if [[ -z "$moltenvk" || ! -f "$moltenvk" ]]; then
    echo "MoltenVK was not found." >&2
    echo "Build Eden, install it with scripts/build-macos.sh, or set MOLTENVK_LIBRARY." >&2
    exit 1
fi
install -m 755 "$moltenvk" "$frameworks/libMoltenVK.dylib"

plutil -lint "$contents/Info.plist" >/dev/null
codesign --force --deep --sign - "$staging" >/dev/null

if [[ -e "$app" ]]; then
    rm -rf "$app"
fi
mv "$staging" "$app"
rmdir "$staging_root"
trap - EXIT

echo "Built $app"
echo "Launch it with: open '$app'"
