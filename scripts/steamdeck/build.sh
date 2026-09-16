#!/bin/sh
# Container entry point. /source is read-only; /output is dedicated to this target.
set -eu
cd /source
revision=$(sh scripts/package-revision.sh /source)
export RUSTFLAGS='-C target-cpu=znver2'
export CFLAGS='-march=znver2 -mtune=znver2'
export CXXFLAGS="$CFLAGS"
export CARGO_INCREMENTAL=0
# Preserve the release panic/unwind policy; omit debug info to bound disk usage.
export CARGO_PROFILE_RELEASE_DEBUG=0
# cubeb-sys invokes Cargo recursively and looks for its PulseAudio archive in
# that nested project's target directory. An exported CARGO_TARGET_DIR redirects
# the nested build as well, but cubeb-sys does not adjust its link-search path.
unset CARGO_TARGET_DIR
cargo build --locked --release --target-dir /output/build --bin ruzu --jobs "$1"
[ "$revision" = "$(sh scripts/package-revision.sh /source)" ] || {
    echo 'Git revision changed during compilation; retry on a stable checkout.' >&2
    exit 1
}
python3 /source/scripts/steamdeck/package.py "$revision"
