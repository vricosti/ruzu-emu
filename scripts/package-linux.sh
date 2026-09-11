#!/bin/sh
# Package the already-built Linux release of ruzu as a Debian archive.
#
# Linux counterpart of build-macos-app.sh --package: the archive name comes
# from scripts/package-revision.sh (an exact version tag on a clean checkout,
# otherwise <branch>-<12-character commit>[-dirty]), Cargo supplies the numeric
# Debian version only, and the binary is never rebuilt here.
#
# Produces target/release/Ruzu-<Distro><Version>-<Git revision>-<arch>.deb,
# for example Ruzu-Ubuntu2404-v0.0.6-amd64.deb.
set -eu
export LC_ALL=C

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "dpkg-deb is required to build the Debian package (install dpkg-dev)." >&2
    exit 1
fi

binary="$repo_root/target/release/ruzu"
if [ ! -x "$binary" ]; then
    echo "target/release/ruzu was not built; run ./build.sh package." >&2
    exit 1
fi

package_revision=$(sh "$repo_root/scripts/package-revision.sh" "$repo_root")
release_commit=$(git rev-parse HEAD)

# Cargo supplies numeric package metadata only, never the archive name.
package_id=$(cargo pkgid --offline -p ruzu)
version=${package_id##*#}
version=${version##*@}
case "$version" in
    *[!0-9.]*|"") echo "Linux release packaging requires a numeric major.minor.patch version: $version" >&2; exit 1 ;;
esac
# A clean tagged checkout ships the bare Cargo version; anything else records
# the commit so the Debian version stays unique and monotonic (+ sorts after).
deb_version=$version
case "$package_revision" in
    v[0-9]*) ;;
    *) deb_version="$version+git$(printf '%.12s' "$release_commit")"
       case "$package_revision" in *-dirty) deb_version="$deb_version.dirty" ;; esac ;;
esac

arch=$(dpkg --print-architecture)
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
fi
distro_id=${ID:-linux}
distro_tag=$(printf '%s' "$distro_id" | sed -E 's/[^A-Za-z0-9]//g; s/^./\U&/')
distro_tag="$distro_tag$(printf '%s' "${VERSION_ID:-}" | sed -E 's/[^0-9]//g')"
distro_name=${PRETTY_NAME:-$distro_id}

package_name="Ruzu-$distro_tag-$package_revision-$arch"
archive="$repo_root/target/release/$package_name.deb"
printf 'Package: %s\nDebian version: %s\n' "$package_name.deb" "$deb_version"
printf 'Generate this package? [y/N]: '
IFS= read -r answer || answer=
case "$answer" in
    y|Y|yes|Yes|YES) ;;
    *) echo "Packaging cancelled; no package files were changed."; exit 1 ;;
esac

staging_root=$(mktemp -d "${TMPDIR:-/tmp}/ruzu-deb.XXXXXX")
trap 'rm -rf "$staging_root"' EXIT INT TERM
staging="$staging_root/ruzu"
install -d -m 755 "$staging/DEBIAN" "$staging/usr/bin" \
    "$staging/usr/share/applications" "$staging/usr/share/doc/ruzu" \
    "$staging/usr/share/icons/hicolor/256x256/apps"

# Ship a stripped copy; target/release/ruzu keeps its debug info for development.
install -m 755 "$binary" "$staging/usr/bin/ruzu"
if command -v strip >/dev/null 2>&1; then
    strip --strip-unneeded "$staging/usr/bin/ruzu"
fi
install -m 644 "$repo_root/src/ruzu/assets/ruzu-rusty-lemon.png" \
    "$staging/usr/share/icons/hicolor/256x256/apps/ruzu.png"
for license in LICENSE LICENSE.txt LICENSE.md; do
    if [ -f "$repo_root/$license" ]; then
        install -m 644 "$repo_root/$license" "$staging/usr/share/doc/ruzu/copyright"
        break
    fi
done
cat > "$staging/usr/share/applications/ruzu.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Ruzu
Comment=Nintendo Switch emulator
Exec=ruzu
TryExec=ruzu
Icon=ruzu
Terminal=false
Categories=Game;Emulator;
DESKTOP
chmod 644 "$staging/usr/share/applications/ruzu.desktop"

# Linked shared libraries come from dpkg-shlibdeps against the shipped binary.
# Libraries ruzu loads at run time (Vulkan, GL, audio, windowing) are not
# visible to it, so they are declared explicitly below.
shlib_depends=
if command -v dpkg-shlibdeps >/dev/null 2>&1; then
    shlibs_dir="$staging_root/shlibs"
    install -d "$shlibs_dir/debian"
    printf 'Source: ruzu\n\nPackage: ruzu\nArchitecture: any\n' > "$shlibs_dir/debian/control"
    shlib_depends=$(cd "$shlibs_dir" && dpkg-shlibdeps -O "$staging/usr/bin/ruzu" 2>/dev/null \
        | sed -n 's/^shlibs:Depends=//p') || shlib_depends=
fi
if [ -z "$shlib_depends" ]; then
    echo "Warning: dpkg-shlibdeps produced no result; using the recorded library list." >&2
    shlib_depends="libavcodec60 (>= 7:6.0), libavutil58 (>= 7:6.0), libc6 (>= 2.39), libcairo2 (>= 1.6.0), libgcc-s1 (>= 4.2), libgdk-pixbuf-2.0-0 (>= 2.22.0), libgl1, libglib2.0-0t64 (>= 2.54.0), libgraphene-1.0-0 (>= 1.5.4), libgtk-4-1 (>= 4.6.0), libopus0 (>= 1.1), libpango-1.0-0 (>= 1.14.0), libpulse0 (>= 0.99.1), libspeexdsp1 (>= 1.2.1), libssl3t64 (>= 3.0.0), libstdc++6 (>= 6), libx11-6"
fi
runtime_depends="ca-certificates, libvulkan1, libgl1, libegl1, libasound2t64, libpulse0, libpipewire-0.3-0t64, libudev1, libdbus-1-3, libwayland-client0, libwayland-cursor0, libwayland-egl1, libxkbcommon0, libdecor-0-0, libx11-6, libxcursor1, libxext6, libxfixes3, libxi6, libxrandr2, libxss1, libxtst6, xdg-utils"

installed_size=$(du -sk "$staging/usr" | cut -f1)
cat > "$staging/DEBIAN/control" <<CONTROL
Package: ruzu
Version: $deb_version
Architecture: $arch
Section: games
Priority: optional
Maintainer: Ruzu contributors <vricosti@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $shlib_depends, $runtime_depends
Recommends: mesa-vulkan-drivers | vulkan-icd, xwayland
Homepage: https://github.com/vricosti/ruzu-emu
Description: Nintendo Switch emulator
 Ruzu desktop emulator, packaged for $distro_name.
CONTROL

# Reject changes to the package identity during packaging.
checked_revision=$(sh "$repo_root/scripts/package-revision.sh" "$repo_root")
if [ "$checked_revision" != "$package_revision" ] || [ "$(git rev-parse HEAD)" != "$release_commit" ]; then
    echo "Git package identity changed while packaging; rebuild the package." >&2
    exit 1
fi

# Keep the previous archive intact until the new one is fully written.
dpkg-deb --build --root-owner-group "$staging" "$staging_root/package.deb" >/dev/null
mv -f "$staging_root/package.deb" "$archive"
printf 'Created %s\n' "$archive"
