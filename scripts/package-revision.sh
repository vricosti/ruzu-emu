#!/bin/sh
# Git-derived, filename-safe identity. Keep in sync with package-revision.ps1.
set -eu
export LC_ALL=C
repo=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$repo"
commit=$(git rev-parse --verify HEAD)
hash=$(printf '%.12s' "$commit")
branch=$(git rev-parse --abbrev-ref HEAD)
if [ "$branch" = HEAD ]; then branch=detached; fi
branch=$(printf '%s' "$branch" | sed -E 's/[^A-Za-z0-9._-]+/-/g; s/^[.-]+//; s/[.-]+$//')
branch=${branch:-branch}
status=$(git status --porcelain --untracked-files=all --ignore-submodules=none)
submodules=$(git submodule status --recursive)
dirty=
if [ -n "$status" ] || printf '%s\n' "$submodules" | grep -Eq '^[-+U]'; then dirty=-dirty; fi
tags=$(git tag --points-at HEAD --sort=refname)
tag=$(printf '%s\n' "$tags" | sed -nE '/^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9][A-Za-z0-9.-]*)?$/p' | head -n 1)
if [ -n "$tag" ] && [ -z "$dirty" ]; then
    printf '%s\n' "$tag"
else
    printf '%s-%s%s\n' "$branch" "$hash" "$dirty"
fi
