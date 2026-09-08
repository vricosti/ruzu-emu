#!/bin/sh
# Validate a local release checkout without changing its refs or files.
set -eu

repo=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$repo"

if [ -n "$(git status --porcelain --untracked-files=all --ignore-submodules=none)" ]; then
    echo "Release packaging requires a clean checkout (including untracked files and submodules). Commit or stash your changes first." >&2
    exit 1
fi

submodules=$(git submodule status --recursive)
if printf '%s\n' "$submodules" | grep -Eq '^[-+U]'; then
    echo "Release submodules must be initialized and match the recorded commits." >&2
    exit 1
fi

# Resolve workspace inheritance using Cargo, not a second TOML parser.
package_id=$(cargo pkgid --offline -p ruzu)
version=${package_id##*#}
version=${version##*@}
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "Release packaging requires a numeric major.minor.patch Cargo version: $version" >&2
    exit 1
fi

# Match the same exact-tag selection used by common/build.rs for the window title.
if ! tag=$(git describe --tags --exact-match HEAD 2>/dev/null); then
    echo "Release packaging requires HEAD to have the exact tag v$version." >&2
    exit 1
fi
if [ "$tag" != "v$version" ]; then
    echo "Release tag $tag does not match Cargo version $version (expected v$version). Update Cargo.toml and the release tag before packaging." >&2
    exit 1
fi

# pkgid must not silently leave generated metadata in the release checkout.
if [ -n "$(git status --porcelain --untracked-files=all --ignore-submodules=none)" ]; then
    echo "Cargo changed the checkout while resolving the release version; review and commit the generated files first." >&2
    exit 1
fi
printf '%s\n' "$version"
